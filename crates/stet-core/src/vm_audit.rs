// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audit global VM for PLRM 3.7.2 violations.
//!
//! PLRM 3.7.2: "An object in global VM is not allowed to contain a reference
//! to an object in local VM." The rule exists because `restore` may deallocate
//! the local object, leaving the global object with a dangling reference.
//!
//! stet enforces the rule in the operators that can perform such a store —
//! `put`, `def`, `store`, `astore`, `putinterval`, `copy`, and the `[`…`]` /
//! `<<`…`>>` constructors — each of which raises `invalidaccess` per PLRM.
//!
//! Operator-level enforcement cannot be the whole story, though. It only
//! covers stores a PostScript program makes; the interpreter's own Rust code
//! writes into dictionaries and arrays directly, and
//! [`ArrayStore::get_mut`](crate::array_store::ArrayStore::get_mut) hands out
//! `&mut [PsObject]`, so such a write passes no checkpoint at all. This module
//! is the backstop: it sweeps the entity tables of both global stores and
//! reports every element that is a composite living in local VM, independent
//! of how it got there.
//!
//! # The sanctioned exception
//!
//! PLRM 3.7.5 carves out an explicit exception to 3.7.2:
//!
//! > **Note:** `systemdict`, a global dictionary, contains several entries
//! > whose values are local dictionaries, such as `userdict` and `$error`.
//! > This is an exception to the normal rule, described in Section 3.7.2 …
//!
//! `userdict`, `errordict`, `$error`, `statusdict`, and `FontDirectory` are
//! standard *local* dictionaries (PLRM Table 3.3) reachable as permanent
//! entries of the *global* `systemdict` (Table 3.4).
//!
//! What makes the exception safe is not the names but the lifetime: those
//! dictionaries are built during interpreter bootstrap, before any `save`, so
//! no `restore` can ever deallocate them. The audit therefore classifies by
//! lifetime rather than by name — see [`Violation::target_reclaimable`]. A
//! global→local reference whose target is unreclaimable can never dangle; one
//! whose target is reclaimable is a live use-after-free hazard the moment
//! `restore` starts releasing local VM.
//!
//! The sweep is O(global VM size) and is not on any hot path. It is intended
//! for tests and for `stet --audit-vm` runs.

use crate::context::Context;
use crate::dict::DictKey;
use crate::object::{EntityId, PsObject, PsValue};

/// One global→local reference found by [`audit_global_vm`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The global composite holding the offending reference.
    pub container: EntityId,
    /// Human-readable identification of the container, e.g. `dict systemdict`
    /// or `array #42`.
    pub container_desc: String,
    /// Where inside the container the reference sits: a dict key or an array
    /// index, rendered for display.
    pub slot: String,
    /// PostScript type name of the local object being referenced.
    pub value_type: &'static str,
    /// Entity ID of the local object being referenced.
    pub value_entity: EntityId,
    /// Whether the referenced local object can ever be released by `restore`.
    ///
    /// `false` means the object was allocated before any `save` executed, so
    /// it sits below every save level's high-water mark and no `restore` can
    /// reclaim it. Such a reference cannot dangle; this is the case that
    /// covers the PLRM 3.7.5 `systemdict` exception (`userdict`, `$error`, …).
    ///
    /// `true` means the object belongs to a save level that a `restore` may
    /// release, leaving the global container pointing at freed storage. These
    /// are the violations that matter.
    pub target_reclaimable: bool,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}] -> local {} (entity {}){}",
            self.container_desc,
            self.slot,
            self.value_type,
            self.value_entity.raw_index(),
            if self.target_reclaimable {
                " RECLAIMABLE"
            } else {
                " (permanent, sanctioned by PLRM 3.7.5)"
            }
        )
    }
}

/// Entity of `obj` if it is a composite living in local VM.
///
/// Simple objects (integers, names, operators, booleans, …) are not in VM at
/// all and may be stored anywhere, so they return `None`. This is the exact
/// complement of the `gcheck` operator's predicate.
pub fn local_composite_entity(obj: &PsObject) -> Option<EntityId> {
    let entity = match obj.value {
        PsValue::String { entity, .. } => entity,
        PsValue::Array { entity, .. } | PsValue::PackedArray { entity, .. } => entity,
        PsValue::Dict(entity) => entity,
        _ => return None,
    };
    if entity.is_global() {
        None
    } else {
        Some(entity)
    }
}

/// Whether `restore` could ever release the storage behind a local entity.
///
/// The test is `created_after_save != 0`: the entity was allocated while some
/// `save` was outstanding, so that save's `restore` releases it.
/// `EntityMeta::created_after_save` is stamped from
/// [`SaveStack::last_save_id`](crate::save_stack::SaveStack::last_save_id),
/// which reports 0 whenever no save is outstanding — including after a
/// save/restore pair has completed. An entity allocated at the outermost level
/// therefore sits below every future save's high-water mark and can never be
/// reclaimed.
///
/// Note that `EntityMeta::save_level` is *not* part of the test. Copy-on-write
/// bumps an entity's `save_level` to the level at which it was last modified,
/// so a bootstrap dictionary such as `$error` acquires a nonzero `save_level`
/// the first time anything writes to it inside a save bracket. That records
/// where the COW backup lives, not where the storage was allocated, and using
/// it here would report the whole PLRM 3.7.5 exception set as unsafe.
fn is_reclaimable(ctx: &Context, obj: &PsObject, entity: EntityId) -> bool {
    let meta = match obj.value {
        PsValue::String { .. } => ctx.strings.local.entities.get(entity),
        PsValue::Array { .. } | PsValue::PackedArray { .. } => {
            ctx.arrays.local.entities.get(entity)
        }
        PsValue::Dict(_) => ctx.dicts.local.entities.get(entity),
        _ => return false,
    };
    meta.created_after_save != 0
}

/// Render a dict key for diagnostics.
fn describe_key(ctx: &Context, key: &DictKey) -> String {
    match key {
        DictKey::Name(id) => String::from_utf8_lossy(ctx.names.get_bytes(*id)).into_owned(),
        DictKey::Int(v) => v.to_string(),
        DictKey::Real(bits) => f64::from_bits(*bits).to_string(),
        DictKey::Bool(v) => v.to_string(),
        DictKey::String(bytes) => format!("({})", String::from_utf8_lossy(bytes)),
        DictKey::Operator(op) => format!("op#{op}"),
        DictKey::Identity(e, s, l) => format!("identity#{e}+{s}:{l}"),
    }
}

/// Sweep global VM and report every reference into local VM.
///
/// An empty result means global VM satisfies PLRM 3.7.2.
pub fn audit_global_vm(ctx: &Context) -> Vec<Violation> {
    let mut out = Vec::new();

    for (entity, entry) in ctx.dicts.global.iter_entities() {
        let desc = {
            let name = String::from_utf8_lossy(&entry.name);
            if name.is_empty() {
                format!("dict #{}", entity.raw_index())
            } else {
                format!("dict {name}")
            }
        };
        for (key, value) in &entry.entries {
            if let Some(value_entity) = local_composite_entity(value) {
                out.push(Violation {
                    container: entity,
                    container_desc: desc.clone(),
                    slot: describe_key(ctx, key),
                    value_type: std::str::from_utf8(value.type_name()).unwrap_or("?"),
                    value_entity,
                    target_reclaimable: is_reclaimable(ctx, value, value_entity),
                });
            }
        }
    }

    for (entity, elements) in ctx.arrays.global.iter_entities() {
        for (index, value) in elements.iter().enumerate() {
            if let Some(value_entity) = local_composite_entity(value) {
                out.push(Violation {
                    container: entity,
                    container_desc: format!("array #{}", entity.raw_index()),
                    slot: index.to_string(),
                    value_type: std::str::from_utf8(value.type_name()).unwrap_or("?"),
                    value_entity,
                    target_reclaimable: is_reclaimable(ctx, value, value_entity),
                });
            }
        }
    }

    out
}

/// A reference to an entity whose storage no longer exists.
///
/// Only possible once `restore` actually releases local VM: truncating an
/// entity table makes every id at or above the new length invalid. Any
/// surviving reference to one is a use-after-free, and indexing the table
/// with it panics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DanglingRef {
    /// Where the stale reference lives.
    pub holder: String,
    /// Which slot inside the holder: a dict key, an array index, or a stack
    /// position.
    pub slot: String,
    /// PostScript type name of the reference's target.
    pub value_type: &'static str,
    /// Entity index the reference points at.
    pub target_index: usize,
    /// Current length of the entity table it points into.
    pub table_len: usize,
}

impl std::fmt::Display for DanglingRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}] -> {} entity {} (table len {})",
            self.holder, self.slot, self.value_type, self.target_index, self.table_len
        )
    }
}

/// Entity table length for the store `obj` lives in, or `None` for simples.
///
/// `Gstate` is included even though it indexes `Context::gstate_store` rather
/// than an entity table: `restore` rewinds that store too, so a surviving
/// `gstate` object can outlive its slot in exactly the same way.
fn table_len_for(ctx: &Context, obj: &PsObject) -> Option<(usize, usize)> {
    if let PsValue::Gstate(idx) = obj.value {
        return Some((idx as usize, ctx.gstate_store.len()));
    }
    let (entity, len) = match obj.value {
        PsValue::String { entity, .. } => (
            entity,
            if entity.is_global() {
                ctx.strings.global.entities.len()
            } else {
                ctx.strings.local.entities.len()
            },
        ),
        PsValue::Array { entity, .. } | PsValue::PackedArray { entity, .. } => (
            entity,
            if entity.is_global() {
                ctx.arrays.global.entities.len()
            } else {
                ctx.arrays.local.entities.len()
            },
        ),
        PsValue::Dict(entity) => (
            entity,
            if entity.is_global() {
                ctx.dicts.global.entities.len()
            } else {
                ctx.dicts.local.entities.len()
            },
        ),
        _ => return None,
    };
    Some((entity.raw_index(), len))
}

/// Walk a colour space for entity references, recursing through the
/// base/alternative spaces that `Indexed`, `Separation`, and `DeviceN` nest.
///
/// The `match` is deliberately exhaustive: adding a `ColorSpace` variant that
/// carries an `EntityId` or a `PsObject` should fail to compile here rather
/// than silently escape the audit.
fn check_color_space(
    ctx: &Context,
    cs: &crate::graphics_state::ColorSpace,
    holder: &str,
    slot: &str,
    out: &mut Vec<DanglingRef>,
) {
    use crate::graphics_state::ColorSpace as Cs;
    match cs {
        Cs::DeviceGray | Cs::DeviceRGB | Cs::DeviceCMYK => {}
        Cs::Indexed {
            base, lookup_proc, ..
        } => {
            check_color_space(ctx, base, holder, &format!("{slot}.base"), out);
            if let Some(p) = lookup_proc {
                check_ref(ctx, p, holder, format!("{slot}.lookup_proc"), out);
            }
        }
        Cs::CIEBasedABC { dict_entity, .. }
        | Cs::CIEBasedA { dict_entity, .. }
        | Cs::CIEBasedDEF { dict_entity, .. }
        | Cs::CIEBasedDEFG { dict_entity, .. }
        | Cs::ICCBased { dict_entity, .. } => {
            check_ref(
                ctx,
                &PsObject::dict(*dict_entity),
                holder,
                format!("{slot}.dict"),
                out,
            );
        }
        Cs::Separation {
            alt_space,
            tint_transform,
            ..
        }
        | Cs::DeviceN {
            alt_space,
            tint_transform,
            ..
        } => {
            check_color_space(ctx, alt_space, holder, &format!("{slot}.alt_space"), out);
            check_ref(
                ctx,
                tint_transform,
                holder,
                format!("{slot}.tint_transform"),
                out,
            );
        }
    }
}

/// Walk one graphics state for entity references.
///
/// A `GraphicsState` is the largest non-VM root the interpreter keeps: fonts,
/// halftone and transfer procedures, the colour space, and the page device
/// are all VM objects held by raw handle. `restore` reinstates `gstate` and
/// `gstate_stack` from the save record, but `gstate_store` — the backing
/// array for `PsValue::Gstate` objects — is not rewound, so a `gstate`
/// captured after the save keeps whatever was current when it was taken.
fn check_gstate(
    ctx: &Context,
    gs: &crate::graphics_state::GraphicsState,
    holder: &str,
    out: &mut Vec<DanglingRef>,
) {
    let one = |obj: &Option<PsObject>, slot: &str, out: &mut Vec<DanglingRef>| {
        if let Some(o) = obj {
            check_ref(ctx, o, holder, slot.to_string(), out);
        }
    };
    one(&gs.current_font, "current_font", out);
    one(&gs.root_font, "root_font", out);
    one(&gs.screen_proc, "screen_proc", out);
    one(&gs.halftone, "halftone", out);
    one(&gs.transfer_function, "transfer_function", out);
    one(&gs.black_generation, "black_generation", out);
    one(&gs.undercolor_removal, "undercolor_removal", out);
    one(&gs.color_rendering, "color_rendering", out);

    if let Some(pd) = gs.page_device {
        check_ref(
            ctx,
            &PsObject::dict(pd),
            holder,
            "page_device".to_string(),
            out,
        );
    }
    if let Some(screens) = &gs.color_screen {
        for (i, (_, _, proc_obj)) in screens.iter().enumerate() {
            check_ref(ctx, proc_obj, holder, format!("color_screen[{i}]"), out);
        }
    }
    if let Some(transfers) = &gs.color_transfer {
        for (i, proc_obj) in transfers.iter().enumerate() {
            check_ref(ctx, proc_obj, holder, format!("color_transfer[{i}]"), out);
        }
    }
    check_color_space(ctx, &gs.color_space, holder, "color_space", out);
}

/// Record `obj` in `out` if it points past the end of its entity table.
fn check_ref(
    ctx: &Context,
    obj: &PsObject,
    holder: &str,
    slot: String,
    out: &mut Vec<DanglingRef>,
) {
    if let Some((index, table_len)) = table_len_for(ctx, obj)
        && index >= table_len
    {
        out.push(DanglingRef {
            holder: holder.to_string(),
            slot,
            value_type: std::str::from_utf8(obj.type_name()).unwrap_or("?"),
            target_index: index,
            table_len,
        });
    }
}

/// Find every reference to storage that no longer exists.
///
/// Copy-on-write backup entities are skipped. They are allocated by
/// `cow_copy` purely so `restore` can swap a composite's contents back, and
/// are unreachable from PostScript in either state — before the restore they
/// hold the snapshot, after it the discarded post-save data. Sweeping them
/// reports every `save`-bracketed mutation as a dangling reference.
///
/// Sweeps both VM arenas plus every interpreter root that outlives a
/// `restore`:
///
/// - the operand, execution, and dictionary stacks
/// - the well-known dictionary handles cached on [`Context`]
/// - the graphics state, the gstate stack, and `gstate_store`
/// - the entity-keyed caches (`glyph_caches`, `form_cache`)
///
/// The display list and `PatternData` need no coverage: they live in
/// `stet-graphics`, which does not depend on `stet-core`, so they cannot name
/// an [`EntityId`] at all. That is a property of the crate graph rather than
/// of any particular type, and `group_stack` inherits it — its frames hold
/// display lists.
///
/// This is the companion to [`audit_global_vm`]. That one asks whether global
/// VM *could* come to hold a dangling reference; this one asks whether
/// anything at all *already does*. On a build where `restore` reclaims
/// nothing the result is always empty, because no entity id is ever retired.
/// [`Context::vm_restore`] asserts it is empty in debug builds, so every
/// `cargo test` run polices the invariant that lets `restore` truncate.
pub fn audit_dangling_refs(ctx: &Context) -> Vec<DanglingRef> {
    let mut out = Vec::new();

    for (store, label) in [
        (&ctx.dicts.local, "local dict"),
        (&ctx.dicts.global, "global dict"),
    ] {
        for (entity, entry) in store.iter_entities() {
            if store.entities.get(entity).is_cow_backup() {
                continue;
            }
            let name = String::from_utf8_lossy(&entry.name);
            let holder = if name.is_empty() {
                format!("{label} #{}", entity.raw_index())
            } else {
                format!("{label} {name} #{}", entity.raw_index())
            };
            for (key, value) in &entry.entries {
                check_ref(ctx, value, &holder, describe_key(ctx, key), &mut out);
            }
        }
    }

    for (store, label) in [
        (&ctx.arrays.local, "local array"),
        (&ctx.arrays.global, "global array"),
    ] {
        for (entity, elements) in store.iter_entities() {
            if store.entities.get(entity).is_cow_backup() {
                continue;
            }
            let holder = format!("{label} #{}", entity.raw_index());
            for (index, value) in elements.iter().enumerate() {
                check_ref(ctx, value, &holder, index.to_string(), &mut out);
            }
        }
    }

    for (index, obj) in ctx.o_stack.as_slice().iter().enumerate() {
        check_ref(ctx, obj, "operand stack", index.to_string(), &mut out);
    }
    for (index, obj) in ctx.e_stack.as_slice().iter().enumerate() {
        check_ref(ctx, obj, "execution stack", index.to_string(), &mut out);
    }
    for (index, entity) in ctx.d_stack.iter().enumerate() {
        check_ref(
            ctx,
            &PsObject::dict(*entity),
            "dictionary stack",
            index.to_string(),
            &mut out,
        );
    }

    // Well-known handles the interpreter keeps outside VM. A restore that
    // retires one of these leaves the interpreter itself holding a stale id.
    for (entity, name) in [
        (ctx.systemdict, "systemdict"),
        (ctx.globaldict, "globaldict"),
        (ctx.userdict, "userdict"),
        (ctx.errordict, "errordict"),
        (ctx.dollar_error, "$error"),
        (ctx.font_directory, "FontDirectory"),
        (ctx.global_resources, "GlobalResources"),
        (ctx.local_resources, "LocalResources"),
        (ctx.category_registry, "CategoryRegistry"),
        (ctx.user_params, "UserParams"),
        (ctx.system_params, "SystemParams"),
        (ctx.internaldict, "internaldict"),
    ] {
        check_ref(
            ctx,
            &PsObject::dict(entity),
            "Context handle",
            name.to_string(),
            &mut out,
        );
    }

    // The graphics state and everything that clones it. `gstate_store` backs
    // `PsValue::Gstate`; unlike `gstate` / `gstate_stack` it is not rewound by
    // `restore`, so it is the one of the three that can outlive its contents.
    check_gstate(ctx, &ctx.gstate, "graphics state", &mut out);
    for (index, entry) in ctx.gstate_stack.iter().enumerate() {
        check_gstate(
            ctx,
            &entry.state,
            &format!("gstate stack #{index}"),
            &mut out,
        );
    }
    for (index, state) in ctx.gstate_store.iter().enumerate() {
        check_gstate(ctx, state, &format!("gstate store #{index}"), &mut out);
    }

    // Caches keyed by entity id. A stale key is not merely a leak: the next
    // lookup indexes a truncated entity table with it.
    for entity in ctx.glyph_caches.keys() {
        check_ref(
            ctx,
            &PsObject::dict(*entity),
            "glyph cache",
            "key".to_string(),
            &mut out,
        );
    }
    for entity in ctx.form_cache.keys() {
        check_ref(
            ctx,
            &PsObject::dict(*entity),
            "form cache",
            "key".to_string(),
            &mut out,
        );
    }

    out
}

/// Sweep global VM for references that could actually dangle.
///
/// This is [`audit_global_vm`] minus the permanent-target references that
/// PLRM 3.7.5 sanctions. An empty result is the invariant that has to hold
/// before `restore` can be allowed to release local VM.
pub fn audit_global_vm_unsafe_only(ctx: &Context) -> Vec<Violation> {
    audit_global_vm(ctx)
        .into_iter()
        .filter(|v| v.target_reclaimable)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjFlags;

    /// A bare context already contains the PLRM 3.7.5 exception: `systemdict`
    /// is global and holds `userdict`, `errordict`, `$error`, and
    /// `FontDirectory`, which are local per PLRM Table 3.3.
    #[test]
    fn bootstrap_exception_is_reported_but_not_reclaimable() {
        let ctx = Context::new();
        let all = audit_global_vm(&ctx);
        assert!(!all.is_empty(), "expected the systemdict exception entries");
        for v in &all {
            assert_eq!(v.container_desc, "dict systemdict", "{v}");
            assert!(!v.target_reclaimable, "{v}");
        }
        let slots: Vec<&str> = all.iter().map(|v| v.slot.as_str()).collect();
        for expected in ["userdict", "errordict", "$error", "FontDirectory"] {
            assert!(slots.contains(&expected), "missing {expected} in {slots:?}");
        }
        assert_eq!(audit_global_vm_unsafe_only(&ctx), Vec::new());
    }

    #[test]
    fn detects_local_string_in_global_dict() {
        let mut ctx = Context::new();
        let gdict = ctx.dicts.allocate_with(4, b"gdict", 0, true, 0);
        // save_level 1 / created_after_save 1 => a restore could release it.
        let lstr = ctx.strings.allocate_with(5, 0, false, 1);
        let key = DictKey::Name(ctx.names.intern(b"k"));
        ctx.dicts.put(gdict, key, PsObject::string(lstr, 5));

        let found = audit_global_vm_unsafe_only(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].container, gdict);
        assert_eq!(found[0].slot, "k");
        assert_eq!(found[0].value_type, "stringtype");
        assert!(found[0].target_reclaimable);
    }

    #[test]
    fn detects_local_array_written_through_get_mut() {
        // The case a per-store hook cannot see.
        let mut ctx = Context::new();
        let garr = ctx.arrays.allocate_with(2, 0, true, 0);
        let larr = ctx.arrays.allocate_with(1, 0, false, 1);
        ctx.arrays.get_mut(garr, 0, 2)[1] = PsObject::array(larr, 1);

        let found = audit_global_vm_unsafe_only(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].slot, "1");
        assert_eq!(found[0].value_type, "arraytype");
    }

    #[test]
    fn global_values_and_simple_values_are_clean() {
        let mut ctx = Context::new();
        let gdict = ctx.dicts.allocate_with(4, b"gdict", 0, true, 0);
        let gstr = ctx.strings.allocate_with(5, 0, true, 0);
        let k_str = DictKey::Name(ctx.names.intern(b"gs"));
        let k_int = DictKey::Name(ctx.names.intern(b"n"));
        ctx.dicts.put(gdict, k_str, PsObject::string(gstr, 5));
        ctx.dicts.put(gdict, k_int, PsObject::int(7));

        assert_eq!(audit_global_vm_unsafe_only(&ctx), Vec::new());
    }

    #[test]
    fn local_container_holding_local_value_is_clean() {
        // The rule is one-directional: local may reference anything.
        let mut ctx = Context::new();
        let ldict = ctx.dicts.allocate_at_level_zero(4, b"ldict");
        let lstr = ctx.strings.allocate_with(5, 0, false, 1);
        let key = DictKey::Name(ctx.names.intern(b"k"));
        ctx.dicts.put(ldict, key, PsObject::string(lstr, 5));

        assert_eq!(audit_global_vm_unsafe_only(&ctx), Vec::new());
    }

    #[test]
    fn local_composite_entity_ignores_simple_objects() {
        assert_eq!(local_composite_entity(&PsObject::int(3)), None);
        assert_eq!(local_composite_entity(&PsObject::null()), None);
        let named = PsObject {
            value: PsValue::Name(crate::object::NameId(0)),
            flags: ObjFlags::literal(),
        };
        assert_eq!(local_composite_entity(&named), None);
    }

    /// An id one past the end of the local dict table — what an entity becomes
    /// the moment a `restore` truncates past it.
    fn retired_dict(ctx: &Context) -> PsObject {
        PsObject::dict(EntityId(ctx.dicts.local.entities.len() as u32))
    }

    #[test]
    fn bootstrap_context_has_no_dangling_refs() {
        assert_eq!(audit_dangling_refs(&Context::new()), Vec::new());
    }

    #[test]
    fn gstate_font_reference_is_swept() {
        let mut ctx = Context::new();
        ctx.gstate.current_font = Some(retired_dict(&ctx));
        let found = audit_dangling_refs(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].holder, "graphics state");
        assert_eq!(found[0].slot, "current_font");
    }

    #[test]
    fn gstate_stack_and_gstate_store_are_swept() {
        let mut ctx = Context::new();
        let stale = retired_dict(&ctx);
        ctx.gstate_stack.push(crate::graphics_state::GstateEntry {
            state: ctx.gstate.clone(),
            saved_by_save: false,
        });
        ctx.gstate_stack[0].state.page_device = match stale.value {
            PsValue::Dict(e) => Some(e),
            _ => unreachable!(),
        };
        ctx.gstate_store.push(ctx.gstate.clone());
        ctx.gstate_store[0].root_font = Some(stale);

        let holders: Vec<&str> = audit_dangling_refs(&ctx)
            .iter()
            .map(|d| Box::leak(d.holder.clone().into_boxed_str()) as &str)
            .collect();
        assert!(holders.contains(&"gstate stack #0"), "{holders:?}");
        assert!(holders.contains(&"gstate store #0"), "{holders:?}");
    }

    /// The colour space walk has to recurse: a `Separation`'s tint transform
    /// hangs off the alternative space, not off the gstate directly.
    #[test]
    fn nested_color_space_reference_is_swept() {
        use crate::graphics_state::ColorSpace;
        let mut ctx = Context::new();
        let stale_entity = EntityId(ctx.dicts.local.entities.len() as u32);
        ctx.gstate.color_space = ColorSpace::Separation {
            name: b"Spot".to_vec(),
            alt_space: Box::new(ColorSpace::ICCBased {
                dict_entity: stale_entity,
                n: 4,
                profile_hash: None,
            }),
            tint_transform: PsObject::null(),
            num_alt_components: 4,
        };
        let found = audit_dangling_refs(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].slot, "color_space.alt_space.dict");
    }

    /// `gstate` objects index `gstate_store`, which `restore` rewinds; a
    /// surviving one is dangling in the same sense as a retired entity id.
    #[test]
    fn stale_gstate_object_is_swept() {
        let mut ctx = Context::new();
        let stale = PsObject {
            value: PsValue::Gstate(0),
            flags: ObjFlags::literal(),
        };
        assert_eq!(ctx.gstate_store.len(), 0);
        ctx.o_stack.push(stale).unwrap();
        let found = audit_dangling_refs(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].value_type, "gstatetype");
    }

    #[test]
    fn entity_keyed_caches_are_swept() {
        let mut ctx = Context::new();
        let stale_entity = EntityId(ctx.dicts.local.entities.len() as u32);
        ctx.form_cache
            .insert(stale_entity, crate::display_list::DisplayList::new());
        let found = audit_dangling_refs(&ctx);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].holder, "form cache");
    }
}
