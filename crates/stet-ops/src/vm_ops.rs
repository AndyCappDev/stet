// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! VM operators: save, restore, vmstatus, setglobal, currentglobal, gcheck, vmreclaim.

use stet_core::context::Context;
use stet_core::error::PsError;
use stet_core::object::{ObjFlags, PsObject, PsValue, SaveLevel};

/// `save`: — → save (snapshot VM state)
pub fn op_save(ctx: &mut Context) -> Result<(), PsError> {
    let save_obj = ctx.vm_save();
    ctx.o_stack.push(save_obj)?;
    Ok(())
}

/// `restore`: save → — (revert VM to saved state)
pub fn op_restore(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let save_id = match obj.value {
        PsValue::Save(SaveLevel(id)) => id,
        _ => return Err(PsError::TypeCheck),
    };

    // INVALIDRESTORE check: scan stacks for local composites newer than save level
    // For now, we do a simplified check — just validate the save_id
    ctx.o_stack.pop()?;

    // Capture clip version before restore so we can emit InitClip+Clip if it changed
    let old_clip_version = ctx.gstate.clip_path_version;

    ctx.vm_restore(save_id)?;

    // Restore device clip in the display list (vm_restore restores the gstate
    // including clip_path, but doesn't update the display list)
    crate::graphics_state_ops::restore_device_clip(ctx, old_clip_version);

    Ok(())
}

/// `vmstatus`: — → level used max (report VM memory state)
pub fn op_vmstatus(ctx: &mut Context) -> Result<(), PsError> {
    let level = ctx.save_stack.depth() as i32;
    // Approximate used memory from store data sizes
    let used = (ctx.strings.data().len() + ctx.arrays.entities.len() * 16) as i32;
    let max_mem = 1_000_000i32; // 1MB nominal max
    ctx.o_stack.push(PsObject::int(level))?;
    ctx.o_stack.push(PsObject::int(used))?;
    ctx.o_stack.push(PsObject::int(max_mem))?;
    Ok(())
}

/// `setglobal`: bool → — (set VM allocation mode)
pub fn op_setglobal(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let global = match obj.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.vm_alloc_mode = global;
    Ok(())
}

/// `currentglobal`: — → bool (get current VM allocation mode)
pub fn op_currentglobal(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.vm_alloc_mode))?;
    Ok(())
}

/// `gcheck`: any → bool (check if object is in global VM)
pub fn op_gcheck(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    let is_global = match obj.value {
        PsValue::String { entity, .. } => ctx.strings.entities.get(entity).is_global(),
        PsValue::Array { entity, .. } => ctx.arrays.entities.get(entity).is_global(),
        PsValue::Dict(entity) => ctx.dicts.entities.get(entity).is_global(),
        // Simple types: global flag is in ObjFlags
        _ => obj.flags.is_global(),
    };
    ctx.o_stack.pop()?;
    ctx.o_stack.push(PsObject::bool(is_global))?;
    Ok(())
}

/// `vmreclaim`: int → — (request garbage collection — no-op for Phase 2)
pub fn op_vmreclaim(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() {
        return Err(PsError::StackUnderflow);
    }
    let obj = ctx.o_stack.peek(0)?;
    match obj.value {
        PsValue::Int(_) => {
            ctx.o_stack.pop()?;
            Ok(()) // No-op
        }
        _ => Err(PsError::TypeCheck),
    }
}

/// Helper: allocate a string in the current VM mode (global or local).
pub fn alloc_string(ctx: &mut Context, bytes: &[u8]) -> stet_core::object::EntityId {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let entity = ctx.strings.allocate_with(bytes.len(), save_level, global);
    ctx.strings
        .get_mut(entity, 0, bytes.len() as u32)
        .copy_from_slice(bytes);
    entity
}

/// Helper: allocate a zero-filled string in the current VM mode.
pub fn alloc_string_empty(ctx: &mut Context, len: usize) -> stet_core::object::EntityId {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    ctx.strings.allocate_with(len, save_level, global)
}

/// Helper: allocate an array in the current VM mode.
pub fn alloc_array(ctx: &mut Context, len: usize) -> stet_core::object::EntityId {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    ctx.arrays.allocate_with(len, save_level, global)
}

/// Helper: allocate an array from initial elements in the current VM mode.
pub fn alloc_array_from(ctx: &mut Context, items: &[PsObject]) -> stet_core::object::EntityId {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    let entity = ctx.arrays.allocate_with(items.len(), save_level, global);
    let dest = ctx.arrays.get_mut(entity, 0, items.len() as u32);
    dest.copy_from_slice(items);
    entity
}

/// Helper: allocate a dict in the current VM mode.
pub fn alloc_dict(
    ctx: &mut Context,
    max_length: usize,
    name: &[u8],
) -> stet_core::object::EntityId {
    let save_level = ctx.save_stack.current_level();
    let global = ctx.vm_alloc_mode;
    ctx.dicts
        .allocate_with(max_length, name, save_level, global)
}

/// Helper: create a PsObject array with the global flag set appropriately.
pub fn make_array_obj(ctx: &Context, entity: stet_core::object::EntityId, len: u32) -> PsObject {
    let mut obj = PsObject::array(entity, len);
    if ctx.vm_alloc_mode {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    obj
}

/// Helper: create a PsObject string with the global flag set appropriately.
pub fn make_string_obj(ctx: &Context, entity: stet_core::object::EntityId, len: u32) -> PsObject {
    let mut obj = PsObject::string(entity, len);
    if ctx.vm_alloc_mode {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    obj
}

/// Helper: create a PsObject dict with the global flag set appropriately.
pub fn make_dict_obj(ctx: &Context, entity: stet_core::object::EntityId) -> PsObject {
    let mut obj = PsObject::dict(entity);
    if ctx.vm_alloc_mode {
        obj.flags = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, false, true, true);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> Context {
        Context::new()
    }

    #[test]
    fn test_save_restore() {
        let mut ctx = test_ctx();
        op_save(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 1);
        let save_obj = ctx.o_stack.peek(0).unwrap();
        assert!(matches!(save_obj.value, PsValue::Save(_)));
        op_restore(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_vmstatus() {
        let mut ctx = test_ctx();
        op_vmstatus(&mut ctx).unwrap();
        assert_eq!(ctx.o_stack.len(), 3);
        let max = ctx.o_stack.pop().unwrap();
        let _used = ctx.o_stack.pop().unwrap();
        let level = ctx.o_stack.pop().unwrap();
        assert!(max.as_i32().unwrap() > 0);
        assert_eq!(level.as_i32(), Some(0));
    }

    #[test]
    fn test_setglobal_currentglobal() {
        let mut ctx = test_ctx();
        assert!(!ctx.vm_alloc_mode);

        ctx.o_stack.push(PsObject::bool(true)).unwrap();
        op_setglobal(&mut ctx).unwrap();
        assert!(ctx.vm_alloc_mode);

        op_currentglobal(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_gcheck_simple() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        op_gcheck(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(false)
        ));
    }

    #[test]
    fn test_gcheck_global_array() {
        let mut ctx = test_ctx();
        ctx.vm_alloc_mode = true;
        let entity = alloc_array(&mut ctx, 3);
        let obj = make_array_obj(&ctx, entity, 3);
        ctx.o_stack.push(obj).unwrap();
        op_gcheck(&mut ctx).unwrap();
        assert!(matches!(
            ctx.o_stack.pop().unwrap().value,
            PsValue::Bool(true)
        ));
    }

    #[test]
    fn test_vmreclaim_noop() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(0)).unwrap();
        op_vmreclaim(&mut ctx).unwrap();
        assert!(ctx.o_stack.is_empty());
    }

    #[test]
    fn test_restore_typecheck() {
        let mut ctx = test_ctx();
        ctx.o_stack.push(PsObject::int(42)).unwrap();
        assert_eq!(op_restore(&mut ctx), Err(PsError::TypeCheck));
    }

    #[test]
    fn test_restore_invalid() {
        let mut ctx = test_ctx();
        // Push a save object with a bogus ID
        ctx.o_stack
            .push(PsObject {
                value: PsValue::Save(SaveLevel(999)),
                flags: stet_core::object::ObjFlags::literal(),
            })
            .unwrap();
        assert_eq!(op_restore(&mut ctx), Err(PsError::InvalidRestore));
    }

    #[test]
    fn test_nested_save_restore() {
        let mut ctx = test_ctx();
        op_save(&mut ctx).unwrap();
        let s1 = ctx.o_stack.pop().unwrap();
        op_save(&mut ctx).unwrap();
        let s2 = ctx.o_stack.pop().unwrap();

        // Must restore in order: s2 first, then s1
        ctx.o_stack.push(s2).unwrap();
        op_restore(&mut ctx).unwrap();
        ctx.o_stack.push(s1).unwrap();
        op_restore(&mut ctx).unwrap();
    }

    #[test]
    fn test_restore_skips_newer() {
        // Per PLRM: "restore can reset VM to the state represented by any
        // save object that is still valid, not necessarily the one produced
        // by the most recent save."
        let mut ctx = test_ctx();
        op_save(&mut ctx).unwrap();
        let s1 = ctx.o_stack.pop().unwrap();
        op_save(&mut ctx).unwrap();
        let s2 = ctx.o_stack.pop().unwrap();

        // Restoring s1 should succeed and also invalidate s2
        ctx.o_stack.push(s1).unwrap();
        assert_eq!(op_restore(&mut ctx), Ok(()));

        // s2 is now invalid — restoring it should fail
        ctx.o_stack.push(s2).unwrap();
        assert_eq!(op_restore(&mut ctx), Err(PsError::InvalidRestore));
    }

    #[test]
    fn test_vmstatus_after_save() {
        let mut ctx = test_ctx();
        op_save(&mut ctx).unwrap();
        ctx.o_stack.pop().unwrap(); // discard save obj
        op_vmstatus(&mut ctx).unwrap();
        let _max = ctx.o_stack.pop().unwrap();
        let _used = ctx.o_stack.pop().unwrap();
        let level = ctx.o_stack.pop().unwrap();
        assert_eq!(level.as_i32(), Some(1));
    }
}
