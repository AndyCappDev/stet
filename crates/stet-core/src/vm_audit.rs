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
fn table_len_for(ctx: &Context, obj: &PsObject) -> Option<(usize, usize)> {
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
/// Sweeps both VM arenas plus the interpreter roots that outlive a `restore`
/// — the operand, execution, and dictionary stacks, and the well-known
/// dictionary handles cached on [`Context`].
///
/// This is the companion to [`audit_global_vm`]. That one asks whether global
/// VM *could* come to hold a dangling reference; this one asks whether
/// anything at all *already does*. On a build where `restore` reclaims
/// nothing the result is always empty, because no entity id is ever retired.
pub fn audit_dangling_refs(ctx: &Context) -> Vec<DanglingRef> {
    let mut out = Vec::new();

    for (store, label) in [
        (&ctx.dicts.local, "local dict"),
        (&ctx.dicts.global, "global dict"),
    ] {
        for (entity, entry) in store.iter_entities() {
            let name = String::from_utf8_lossy(&entry.name);
            let holder = if name.is_empty() {
                format!("{label} #{}", entity.raw_index())
            } else {
                format!("{label} {name}")
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
    ] {
        check_ref(
            ctx,
            &PsObject::dict(entity),
            "Context handle",
            name.to_string(),
            &mut out,
        );
    }

    if let Some(pd) = ctx.gstate.page_device {
        check_ref(
            ctx,
            &PsObject::dict(pd),
            "graphics state",
            "page_device".to_string(),
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
}
