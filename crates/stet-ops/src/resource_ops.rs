// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resource operators: findresource, defineresource, undefineresource,
//! resourcestatus, resourceforall, and helper operators.

use stet_core::context::Context;
use stet_core::dict::DictKey;
use stet_core::error::PsError;
use stet_core::object::{PsObject, PsValue};

/// `findresource`: key category → instance
///
/// Look up a resource. For category `/Category`, look directly in the category
/// registry. For other categories, dispatch to the category's FindResource
/// procedure.
pub fn op_findresource(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    let cat_key = DictKey::Name(cat_name);
    let category_name_id = ctx.name_cache.n_category;

    // Special case: /Category findresource → look up directly in category_registry
    if cat_name == category_name_id {
        let key = ctx.make_dict_key(&key_obj)?;
        let result = ctx.dicts.get(ctx.category_registry, &key);
        ctx.o_stack.pop()?; // category
        ctx.o_stack.pop()?; // key
        match result {
            Some(val) => {
                ctx.o_stack.push(val)?;
                Ok(())
            }
            None => Err(PsError::UndefinedResource),
        }
    } else {
        // Get the category implementation dict
        let impl_dict_obj = ctx.dicts.get(ctx.category_registry, &cat_key);
        let impl_entity = match impl_dict_obj {
            Some(obj) => match obj.value {
                PsValue::Dict(e) => e,
                _ => return Err(PsError::UndefinedResource),
            },
            None => return Err(PsError::UndefinedResource),
        };

        // Look up FindResource in the impl dict
        let find_key = DictKey::Name(ctx.name_cache.n_find_resource);
        let find_proc = ctx.dicts.get(impl_entity, &find_key);

        match find_proc {
            Some(proc) if proc.is_array_type() && proc.flags.is_executable() => {
                // PS procedure: pop category, push impl dict on d_stack, dispatch.
                // Push DictEnd first so it executes after the procedure finishes,
                // cleaning up the impl dict from the d_stack.
                ctx.o_stack.pop()?;
                ctx.d_stack.push(impl_entity);
                ctx.invalidate_name_cache();
                ctx.e_stack.push(PsObject::dict_end(impl_entity))?;
                ctx.e_stack.push(proc)?;
                Ok(())
            }
            _ => {
                // Operator or no procedure: direct lookup (avoids infinite recursion)
                findresource_direct(ctx, cat_name)
            }
        }
    }
}

/// Direct resource lookup (no PS dispatch) — searches local then global resource dicts.
fn findresource_direct(
    ctx: &mut Context,
    cat_name: stet_core::object::NameId,
) -> Result<(), PsError> {
    let key_obj = ctx.o_stack.peek(1)?;
    let key = ctx.make_dict_key(&key_obj)?;
    let cat_key = DictKey::Name(cat_name);

    // Search local resources
    if !ctx.vm_alloc_mode
        && let Some(local_cat_dict_obj) = ctx.dicts.get(ctx.local_resources, &cat_key)
        && let PsValue::Dict(local_cat) = local_cat_dict_obj.value
        && let Some(val) = ctx.dicts.get(local_cat, &key)
    {
        ctx.o_stack.pop()?; // category
        ctx.o_stack.pop()?; // key
        ctx.o_stack.push(val)?;
        return Ok(());
    }

    // Search global resources
    if let Some(global_cat_dict_obj) = ctx.dicts.get(ctx.global_resources, &cat_key)
        && let PsValue::Dict(global_cat) = global_cat_dict_obj.value
        && let Some(val) = ctx.dicts.get(global_cat, &key)
    {
        ctx.o_stack.pop()?; // category
        ctx.o_stack.pop()?; // key
        ctx.o_stack.push(val)?;
        return Ok(());
    }

    // Not found
    ctx.o_stack.pop()?; // category
    // Leave key on stack for error reporting
    Err(PsError::UndefinedResource)
}

/// `defineresource`: key instance category → instance
///
/// Register a resource instance. For category `/Category`, register in the
/// category registry. For other categories, dispatch to the category's
/// DefineResource procedure.
pub fn op_defineresource(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 3 {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let _instance_obj = ctx.o_stack.peek(1)?;
    let key_obj = ctx.o_stack.peek(2)?;

    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    // Handle dict-as-key (common in dvips Type 3 bitmap fonts: "fontdict dup definefont").
    // Extract /FontName or /FID from the dict, or generate a unique name.
    // Replace the key on the operand stack so PS category procedures see a name.
    if let PsValue::Dict(dict_entity) = key_obj.value {
        let font_name_key = DictKey::Name(ctx.name_cache.n_font_name);
        let fid_key = DictKey::Name(ctx.name_cache.n_fid);
        let replacement = if let Some(name_obj) = ctx.dicts.get(dict_entity, &font_name_key) {
            name_obj
        } else if let Some(fid_obj) = ctx.dicts.get(dict_entity, &fid_key) {
            fid_obj
        } else {
            // Generate a unique name from the entity id
            let unique = format!("Font_{}", dict_entity.0);
            let name_id = ctx.names.intern(unique.as_bytes());
            let name_obj = PsObject::name_lit(name_id);
            // Store in the font dict for future reference
            ctx.dicts.put(dict_entity, font_name_key, name_obj);
            name_obj
        };
        // Replace key on operand stack (position 2 from top)
        *ctx.o_stack.peek_mut(2)? = replacement;
    }

    // Re-read key after possible replacement
    let key_obj = ctx.o_stack.peek(2)?;

    let category_name_id = ctx.name_cache.n_category;

    // Special case: /Category defineresource → register in category_registry
    if cat_name == category_name_id {
        let instance = ctx.o_stack.peek(1)?;
        let key = ctx.make_dict_key(&key_obj)?;
        ctx.o_stack.pop()?; // category
        ctx.o_stack.pop()?; // instance
        ctx.o_stack.pop()?; // key
        // Update the /Category key in the impl dict to match the registered name.
        // PS code often copies Generic's impl dict (which has /Category /Generic)
        // without updating /Category. FindResource uses Category to look up the
        // right resource dict, so it must match the actual category name.
        if let PsValue::Dict(impl_entity) = instance.value {
            let cat_key_name = ctx.names.intern(b"Category");
            ctx.dicts
                .put(impl_entity, DictKey::Name(cat_key_name), key_obj);
        }
        ctx.dicts.put(ctx.category_registry, key, instance);
        ctx.o_stack.push(instance)?;
        return Ok(());
    }

    // Get category impl dict
    let cat_key = DictKey::Name(cat_name);
    let impl_dict_obj = ctx.dicts.get(ctx.category_registry, &cat_key);
    let impl_entity = match impl_dict_obj {
        Some(obj) => match obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::UndefinedResource),
        },
        None => return Err(PsError::UndefinedResource),
    };

    // Look up DefineResource in impl dict
    let def_key = DictKey::Name(ctx.name_cache.n_define_resource);
    let def_proc = ctx.dicts.get(impl_entity, &def_key);

    match def_proc {
        Some(proc) if proc.is_array_type() && proc.flags.is_executable() => {
            // PS procedure: pop category, push impl dict on d_stack, dispatch
            ctx.o_stack.pop()?;
            ctx.d_stack.push(impl_entity);
            ctx.invalidate_name_cache();
            ctx.e_stack.push(PsObject::dict_end(impl_entity))?;
            ctx.e_stack.push(proc)?;
            Ok(())
        }
        _ => {
            // Operator or no procedure: direct registration (avoids infinite recursion)
            defineresource_direct(ctx, cat_name)
        }
    }
}

/// Direct resource registration (no PS dispatch).
fn defineresource_direct(
    ctx: &mut Context,
    cat_name: stet_core::object::NameId,
) -> Result<(), PsError> {
    let instance = ctx.o_stack.peek(1)?;
    let key_obj = ctx.o_stack.peek(2)?;
    let key = ctx.make_dict_key(&key_obj)?;
    let cat_key = DictKey::Name(cat_name);

    ctx.o_stack.pop()?; // category
    ctx.o_stack.pop()?; // instance (will be re-pushed)
    ctx.o_stack.pop()?; // key

    // Determine which resource dict to use based on VM mode
    let res_dict = if ctx.vm_alloc_mode {
        ctx.global_resources
    } else {
        ctx.local_resources
    };

    // Ensure category dict exists in the resource dict
    let cat_dict = if let Some(existing) = ctx.dicts.get(res_dict, &cat_key) {
        match existing.value {
            PsValue::Dict(e) => e,
            _ => {
                let d = ctx.dicts.allocate(20, b"resource_cat");
                ctx.dicts.put(res_dict, cat_key.clone(), PsObject::dict(d));
                d
            }
        }
    } else {
        let d = ctx.dicts.allocate(20, b"resource_cat");
        ctx.dicts.put(res_dict, cat_key.clone(), PsObject::dict(d));
        d
    };

    ctx.dicts.put(cat_dict, key, instance);
    ctx.o_stack.push(instance)?;
    Ok(())
}

/// `undefineresource`: key category → —
pub fn op_undefineresource(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    let cat_key = DictKey::Name(cat_name);
    let impl_dict_obj = ctx.dicts.get(ctx.category_registry, &cat_key);
    let impl_entity = match impl_dict_obj {
        Some(obj) => match obj.value {
            PsValue::Dict(e) => e,
            _ => return Err(PsError::UndefinedResource),
        },
        None => return Err(PsError::UndefinedResource),
    };

    // Look up UndefineResource in impl dict
    let undef_key = DictKey::Name(ctx.name_cache.n_undef_resource);
    let undef_proc = ctx.dicts.get(impl_entity, &undef_key);

    match undef_proc {
        Some(proc) if proc.is_array_type() && proc.flags.is_executable() => {
            ctx.o_stack.pop()?; // category
            ctx.d_stack.push(impl_entity);
            ctx.invalidate_name_cache();
            ctx.e_stack.push(PsObject::dict_end(impl_entity))?;
            ctx.e_stack.push(proc)?;
            Ok(())
        }
        _ => {
            // Direct: remove from resource dicts
            let key = ctx.make_dict_key(&key_obj)?;
            ctx.o_stack.pop()?;
            ctx.o_stack.pop()?;

            // Remove from local
            if let Some(local_cat_obj) =
                ctx.dicts.get(ctx.local_resources, &DictKey::Name(cat_name))
                && let PsValue::Dict(local_cat) = local_cat_obj.value
            {
                ctx.dicts.remove(local_cat, &key);
            }
            // Remove from global
            if let Some(global_cat_obj) = ctx
                .dicts
                .get(ctx.global_resources, &DictKey::Name(cat_name))
                && let PsValue::Dict(global_cat) = global_cat_obj.value
            {
                ctx.dicts.remove(global_cat, &key);
            }
            Ok(())
        }
    }
}

/// `resourcestatus`: key category → status size true | false
pub fn op_resourcestatus(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let key_obj = ctx.o_stack.peek(1)?;

    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    let cat_key = DictKey::Name(cat_name);

    // Check if the category has a ResourceStatus procedure
    if let Some(obj) = ctx.dicts.get(ctx.category_registry, &cat_key)
        && let PsValue::Dict(impl_entity) = obj.value
        && let Some(proc) = ctx.dicts.get(
            impl_entity,
            &DictKey::Name(ctx.name_cache.n_resource_status),
        )
        && proc.is_array_type()
        && proc.flags.is_executable()
    {
        ctx.o_stack.pop()?; // category
        ctx.d_stack.push(impl_entity);
        ctx.invalidate_name_cache();
        ctx.e_stack.push(PsObject::dict_end(impl_entity))?;
        ctx.e_stack.push(proc)?;
        return Ok(());
    }

    // Direct check: search resource dicts
    let key = ctx.make_dict_key(&key_obj)?;
    ctx.o_stack.pop()?; // category
    ctx.o_stack.pop()?; // key

    let found = resource_exists(ctx, cat_name, &key);
    if found {
        ctx.o_stack.push(PsObject::int(1))?; // status
        ctx.o_stack.push(PsObject::int(15000))?; // estimated size
        ctx.o_stack.push(PsObject::bool(true))?;
    } else {
        ctx.o_stack.push(PsObject::bool(false))?;
    }
    Ok(())
}

/// Check if a resource exists in local or global dicts.
fn resource_exists(ctx: &Context, cat_name: stet_core::object::NameId, key: &DictKey) -> bool {
    let cat_key = DictKey::Name(cat_name);

    if let Some(local_cat_obj) = ctx.dicts.get(ctx.local_resources, &cat_key)
        && let PsValue::Dict(local_cat) = local_cat_obj.value
        && ctx.dicts.known(local_cat, key)
    {
        return true;
    }
    if let Some(global_cat_obj) = ctx.dicts.get(ctx.global_resources, &cat_key)
        && let PsValue::Dict(global_cat) = global_cat_obj.value
        && ctx.dicts.known(global_cat, key)
    {
        return true;
    }
    false
}

/// `resourceforall`: template proc scratch category → —
/// Match a resource name against a PostScript template pattern.
/// Supports `*` (match any sequence) and `?` (match single char).
fn template_matches(template: &[u8], name: &[u8]) -> bool {
    let mut ti = 0;
    let mut ni = 0;
    let mut star_ti = usize::MAX;
    let mut star_ni = 0;

    while ni < name.len() {
        if ti < template.len() && (template[ti] == b'?' || template[ti] == name[ni]) {
            ti += 1;
            ni += 1;
        } else if ti < template.len() && template[ti] == b'*' {
            star_ti = ti;
            star_ni = ni;
            ti += 1;
        } else if star_ti != usize::MAX {
            ti = star_ti + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }
    while ti < template.len() && template[ti] == b'*' {
        ti += 1;
    }
    ti == template.len()
}

/// `resourceforall`: template proc scratch category → —
///
/// Native implementation that enumerates resource names without pushing
/// category impl dicts onto d_stack (avoids dict stack pollution in callbacks).
pub fn op_resourceforall(ctx: &mut Context) -> Result<(), PsError> {
    use stet_core::object::NameId;

    if ctx.o_stack.len() < 4 {
        return Err(PsError::StackUnderflow);
    }
    // Validate types: category (name), scratch (string), proc (exec array), template (string)
    let cat_obj = ctx.o_stack.peek(0)?;
    let scratch_obj = ctx.o_stack.peek(1)?;
    let proc_obj = ctx.o_stack.peek(2)?;
    let template_obj = ctx.o_stack.peek(3)?;

    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };
    let (scratch_entity, scratch_start, scratch_len) = match scratch_obj.value {
        PsValue::String { entity, start, len } => (entity, start, len),
        _ => return Err(PsError::TypeCheck),
    };
    if !proc_obj.is_array_type() || !proc_obj.flags.is_executable() {
        return Err(PsError::TypeCheck);
    }
    let template_bytes = match template_obj.value {
        PsValue::String { entity, start, len } => ctx.strings.get(entity, start, len).to_vec(),
        _ => return Err(PsError::TypeCheck),
    };

    // Pop all 4 operands
    let _cat = ctx.o_stack.pop()?;
    let _scratch = ctx.o_stack.pop()?;
    let proc = ctx.o_stack.pop()?;
    let _template = ctx.o_stack.pop()?;

    let cat_key = DictKey::Name(cat_name);

    // Collect matching resource names from global (and local if in local mode) dicts.
    // We collect NameIds first to avoid borrowing issues during iteration.
    let mut name_ids: Vec<NameId> = Vec::new();

    // Global resources
    if let Some(dict_obj) = ctx.dicts.get(ctx.global_resources, &cat_key)
        && let PsValue::Dict(res_dict) = dict_obj.value {
            for key in ctx.dicts.keys(res_dict) {
                if let DictKey::Name(nid) = key {
                    name_ids.push(*nid);
                }
            }
        }

    // Local resources (only when in local VM mode)
    if !ctx.vm_alloc_mode
        && let Some(dict_obj) = ctx.dicts.get(ctx.local_resources, &cat_key)
            && let PsValue::Dict(res_dict) = dict_obj.value {
                for key in ctx.dicts.keys(res_dict) {
                    if let DictKey::Name(nid) = key
                        && !name_ids.contains(nid) {
                            name_ids.push(*nid);
                        }
                }
            }

    // Also check FontDirectory for Font category
    let font_name_id = ctx.names.find(b"Font");
    if font_name_id == Some(cat_name)
        && let Some(font_dir_obj) = ctx.dicts.get(
            ctx.systemdict,
            &DictKey::Name(ctx.names.intern(b"FontDirectory")),
        )
            && let PsValue::Dict(font_dir) = font_dir_obj.value {
                for key in ctx.dicts.keys(font_dir) {
                    if let DictKey::Name(nid) = key
                        && !name_ids.contains(nid) {
                            name_ids.push(*nid);
                        }
                }
            }

    // Filter by template pattern and sort
    let mut matching_names: Vec<(NameId, Vec<u8>)> = name_ids
        .into_iter()
        .filter_map(|nid| {
            let bytes = ctx.names.get_bytes(nid).to_vec();
            if template_matches(&template_bytes, &bytes) {
                Some((nid, bytes))
            } else {
                None
            }
        })
        .collect();
    matching_names.sort_by(|a, b| a.1.cmp(&b.1));

    // Execute callback for each matching name.
    // Write name into scratch string and push substring onto o_stack.
    for (_nid, name_bytes) in &matching_names {
        let copy_len = name_bytes.len().min(scratch_len as usize);
        let scratch_slice = ctx
            .strings
            .get_mut(scratch_entity, scratch_start, scratch_len);
        scratch_slice[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

        // Push a substring view of the scratch string (just the written portion)
        let str_obj = PsObject {
            value: PsValue::String {
                entity: scratch_entity,
                start: scratch_start,
                len: copy_len as u32,
            },
            flags: stet_core::object::ObjFlags::literal(),
        };
        ctx.o_stack.push(str_obj)?;

        // Execute the callback proc
        match ctx.exec_sync(proc) {
            Ok(()) => {}
            Err(PsError::Stop) => break,
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

/// `globalresourcedict`: category → dict true | false
///
/// Look up a category's resource dict in global_resources.
pub fn op_globalresourcedict(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    let cat_key = DictKey::Name(cat_name);
    if let Some(dict_obj) = ctx.dicts.get(ctx.global_resources, &cat_key)
        && matches!(dict_obj.value, PsValue::Dict(_))
    {
        ctx.o_stack.push(dict_obj)?;
        ctx.o_stack.push(PsObject::bool(true))?;
        return Ok(());
    }

    // Not found — create one and return it
    let d = ctx.dicts.allocate_with(20, b"global_res_cat", 0, true, 0);
    ctx.dicts
        .put(ctx.global_resources, cat_key, PsObject::dict(d));
    ctx.o_stack.push(PsObject::dict(d))?;
    ctx.o_stack.push(PsObject::bool(true))?;
    Ok(())
}

/// `localresourcedict`: category → dict true | false
///
/// Look up a category's resource dict in local_resources.
pub fn op_localresourcedict(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let cat_obj = ctx.o_stack.peek(0)?;
    let cat_name = match cat_obj.value {
        PsValue::Name(id) => id,
        _ => return Err(PsError::TypeCheck),
    };

    ctx.o_stack.pop()?;

    let cat_key = DictKey::Name(cat_name);
    if let Some(dict_obj) = ctx.dicts.get(ctx.local_resources, &cat_key)
        && matches!(dict_obj.value, PsValue::Dict(_))
    {
        ctx.o_stack.push(dict_obj)?;
        ctx.o_stack.push(PsObject::bool(true))?;
        return Ok(());
    }

    // Not found — create one and return it
    let d = ctx.dicts.allocate(20, b"local_res_cat");
    ctx.dicts
        .put(ctx.local_resources, cat_key, PsObject::dict(d));
    ctx.o_stack.push(PsObject::dict(d))?;
    ctx.o_stack.push(PsObject::bool(true))?;
    Ok(())
}

/// `categoryimpdict`: — → dict (return category registry)
pub fn op_categoryimpdict(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::dict(ctx.category_registry))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_core::context::Context;

    fn test_ctx() -> Context {
        let mut ctx = Context::new();
        crate::build_system_dict(&mut ctx);
        ctx
    }

    #[test]
    fn test_defineresource_category() {
        let mut ctx = test_ctx();
        let cat_name = ctx.name_cache.n_category;
        let key_name = ctx.names.intern(b"TestCat");

        let impl_dict = ctx.dicts.allocate(10, b"TestCat");
        ctx.o_stack.push(PsObject::name_lit(key_name)).unwrap();
        ctx.o_stack.push(PsObject::dict(impl_dict)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(cat_name)).unwrap();
        op_defineresource(&mut ctx).unwrap();

        // Result should be the instance
        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Dict(_)));

        // Should be in category_registry
        assert!(
            ctx.dicts
                .known(ctx.category_registry, &DictKey::Name(key_name))
        );
    }

    #[test]
    fn test_findresource_category() {
        let mut ctx = test_ctx();
        let cat_name = ctx.name_cache.n_category;

        // First register a category
        let key_name = ctx.names.intern(b"TestCat");
        let impl_dict = ctx.dicts.allocate(10, b"TestCat");
        ctx.dicts.put(
            ctx.category_registry,
            DictKey::Name(key_name),
            PsObject::dict(impl_dict),
        );

        // Now find it
        ctx.o_stack.push(PsObject::name_lit(key_name)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(cat_name)).unwrap();
        op_findresource(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_globalresourcedict() {
        let mut ctx = test_ctx();
        let font_name = ctx.names.intern(b"Font");

        ctx.o_stack.push(PsObject::name_lit(font_name)).unwrap();
        op_globalresourcedict(&mut ctx).unwrap();

        let flag = ctx.o_stack.pop().unwrap();
        assert!(matches!(flag.value, PsValue::Bool(true)));
        let dict = ctx.o_stack.pop().unwrap();
        assert!(matches!(dict.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_localresourcedict() {
        let mut ctx = test_ctx();
        let font_name = ctx.names.intern(b"Font");

        ctx.o_stack.push(PsObject::name_lit(font_name)).unwrap();
        op_localresourcedict(&mut ctx).unwrap();

        let flag = ctx.o_stack.pop().unwrap();
        assert!(matches!(flag.value, PsValue::Bool(true)));
        let dict = ctx.o_stack.pop().unwrap();
        assert!(matches!(dict.value, PsValue::Dict(_)));
    }

    #[test]
    fn test_categoryimpdict() {
        let mut ctx = test_ctx();
        op_categoryimpdict(&mut ctx).unwrap();
        let result = ctx.o_stack.pop().unwrap();
        if let PsValue::Dict(e) = result.value {
            assert_eq!(e, ctx.category_registry);
        } else {
            panic!("Expected dict");
        }
    }

    #[test]
    fn test_resourcestatus_not_found() {
        let mut ctx = test_ctx();
        let font_name = ctx.names.intern(b"Font");

        // Register Font as a category with no ResourceStatus proc
        let font_cat = ctx.dicts.allocate(10, b"FontCat");
        ctx.dicts.put(
            ctx.category_registry,
            DictKey::Name(font_name),
            PsObject::dict(font_cat),
        );

        let key = ctx.names.intern(b"NoSuchFont");
        ctx.o_stack.push(PsObject::name_lit(key)).unwrap();
        ctx.o_stack.push(PsObject::name_lit(font_name)).unwrap();
        op_resourcestatus(&mut ctx).unwrap();

        let result = ctx.o_stack.pop().unwrap();
        assert!(matches!(result.value, PsValue::Bool(false)));
    }
}
