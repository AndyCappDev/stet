// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostScript object representation.
//!
//! `PsObject` is the fundamental unit — a tagged value with metadata flags.
//! Objects are `Clone + Copy` (value types with arena indices, not heap references).

/// Packed object metadata (1 byte).
///
/// Layout:
/// - Bits 0-2: access level (0-4)
/// - Bit 3: executable (0=literal, 1=executable)
/// - Bit 4: global (0=local, 1=global)
/// - Bit 5: composite (0=simple, 1=composite)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjFlags(u8);

impl ObjFlags {
    pub const LITERAL: u8 = 0;
    pub const EXECUTABLE: u8 = 1 << 3;

    pub const ACCESS_NONE: u8 = 0;
    pub const ACCESS_EXECUTE_ONLY: u8 = 1;
    pub const ACCESS_READ_ONLY: u8 = 2;
    pub const ACCESS_WRITE_ONLY: u8 = 3;
    pub const ACCESS_UNLIMITED: u8 = 4;

    const ACCESS_MASK: u8 = 0b0000_0111;
    const EXEC_BIT: u8 = 1 << 3;
    const GLOBAL_BIT: u8 = 1 << 4;
    const COMPOSITE_BIT: u8 = 1 << 5;
    const DEFERRED_BIT: u8 = 1 << 6;

    /// Create new flags with specified attributes.
    pub fn new(access: u8, executable: bool, global: bool, composite: bool) -> Self {
        let mut bits = access & Self::ACCESS_MASK;
        if executable {
            bits |= Self::EXEC_BIT;
        }
        if global {
            bits |= Self::GLOBAL_BIT;
        }
        if composite {
            bits |= Self::COMPOSITE_BIT;
        }
        Self(bits)
    }

    /// Convenience: literal simple object with unlimited access.
    pub fn literal() -> Self {
        Self::new(Self::ACCESS_UNLIMITED, false, false, false)
    }

    /// Convenience: executable simple object with unlimited access.
    pub fn executable() -> Self {
        Self::new(Self::ACCESS_UNLIMITED, true, false, false)
    }

    /// Convenience: literal composite object with unlimited access.
    pub fn literal_composite() -> Self {
        Self::new(Self::ACCESS_UNLIMITED, false, false, true)
    }

    /// Convenience: executable composite object with unlimited access.
    pub fn executable_composite() -> Self {
        Self::new(Self::ACCESS_UNLIMITED, true, false, true)
    }

    pub fn access(self) -> u8 {
        self.0 & Self::ACCESS_MASK
    }

    pub fn is_executable(self) -> bool {
        self.0 & Self::EXEC_BIT != 0
    }

    pub fn is_literal(self) -> bool {
        !self.is_executable()
    }

    pub fn is_global(self) -> bool {
        self.0 & Self::GLOBAL_BIT != 0
    }

    pub fn is_composite(self) -> bool {
        self.0 & Self::COMPOSITE_BIT != 0
    }

    pub fn set_executable(&mut self) {
        self.0 |= Self::EXEC_BIT;
    }

    pub fn set_literal(&mut self) {
        self.0 &= !Self::EXEC_BIT;
    }

    pub fn set_access(&mut self, access: u8) {
        self.0 = (self.0 & !Self::ACCESS_MASK) | (access & Self::ACCESS_MASK);
    }

    /// Check if this object is deferred (should be pushed to o_stack from e_stack).
    ///
    /// Used by `exec_procedure` to mark nested executable arrays that should be
    /// pushed to the operand stack rather than executed when encountered on the
    /// execution stack. The executable flag remains set so operators like `if`
    /// and `ifelse` still accept them.
    pub fn is_deferred(self) -> bool {
        self.0 & Self::DEFERRED_BIT != 0
    }

    /// Mark this object as deferred.
    pub fn set_deferred(&mut self) {
        self.0 |= Self::DEFERRED_BIT;
    }

    /// Clear the deferred flag.
    pub fn clear_deferred(&mut self) {
        self.0 &= !Self::DEFERRED_BIT;
    }
}

/// Index into the name interning table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NameId(pub u32);

/// Index into an arena store (strings, arrays, dicts, loop states).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityId(pub u32);

/// Index into the operator dispatch table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpCode(pub u16);

/// Save/restore nesting level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveLevel(pub u32);

/// The value payload of a PostScript object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PsValue {
    // Simple types (no arena allocation)
    Null,
    Mark,
    /// Dict mark from `<<` — distinguished from `Mark` so `]` only matches `[`-marks.
    DictMark,
    Bool(bool),
    Int(i32),
    Real(f64),

    // Interned name (index into NameTable)
    Name(NameId),

    // Composite types (arena-backed)
    String {
        entity: EntityId,
        start: u32,
        len: u32,
    },
    Array {
        entity: EntityId,
        start: u32,
        len: u32,
    },
    PackedArray {
        entity: EntityId,
        start: u32,
        len: u32,
    },
    Dict(EntityId),

    // Executable types
    Operator(OpCode),

    // Special types
    File(EntityId),
    Save(SaveLevel),
    FontID(i32),

    // Control flow (internal, not user-visible)
    Stopped,
    Loop(EntityId),
    HardReturn,
    /// Marker that conditionally pops the dict stack when reached (used by
    /// resource operators to clean up after dispatching to PS-defined category
    /// procedures). Carries the expected entity so we only pop if it's still
    /// on top — the PS procedure may have already called `end`.
    DictEnd(EntityId),

    /// Procedure cursor on the exec stack — tracks position within a procedure
    /// being executed. The eval loop advances `pos` one element at a time.
    ExecArray {
        entity: EntityId,
        start: u32,
        len: u32,
        pos: u32,
    },
}

/// A PostScript object: a tagged value with metadata flags.
///
/// `PsObject` is `Clone + Copy` — it's a value type containing indices
/// into arena stores, not heap references.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PsObject {
    pub value: PsValue,
    pub flags: ObjFlags,
}

impl PsObject {
    // --- Convenience constructors ---

    pub fn int(v: i32) -> Self {
        Self {
            value: PsValue::Int(v),
            flags: ObjFlags::literal(),
        }
    }

    pub fn real(v: f64) -> Self {
        Self {
            value: PsValue::Real(v),
            flags: ObjFlags::literal(),
        }
    }

    pub fn bool(v: bool) -> Self {
        Self {
            value: PsValue::Bool(v),
            flags: ObjFlags::literal(),
        }
    }

    pub fn null() -> Self {
        Self {
            value: PsValue::Null,
            flags: ObjFlags::literal(),
        }
    }

    pub fn mark() -> Self {
        Self {
            value: PsValue::Mark,
            flags: ObjFlags::literal(),
        }
    }

    /// Dict mark from `<<` — distinct from `[`/`mark` marks.
    pub fn dict_mark() -> Self {
        Self {
            value: PsValue::DictMark,
            flags: ObjFlags::literal(),
        }
    }

    /// Literal name: `/foo`
    pub fn name_lit(id: NameId) -> Self {
        Self {
            value: PsValue::Name(id),
            flags: ObjFlags::literal(),
        }
    }

    /// Executable name: `foo`
    pub fn name_exec(id: NameId) -> Self {
        Self {
            value: PsValue::Name(id),
            flags: ObjFlags::executable(),
        }
    }

    pub fn operator(op: OpCode) -> Self {
        Self {
            value: PsValue::Operator(op),
            flags: ObjFlags::executable(),
        }
    }

    /// Literal string.
    pub fn string(entity: EntityId, len: u32) -> Self {
        Self {
            value: PsValue::String {
                entity,
                start: 0,
                len,
            },
            flags: ObjFlags::literal_composite(),
        }
    }

    /// Literal array.
    pub fn array(entity: EntityId, len: u32) -> Self {
        Self {
            value: PsValue::Array {
                entity,
                start: 0,
                len,
            },
            flags: ObjFlags::literal_composite(),
        }
    }

    /// Executable array (procedure body).
    pub fn procedure(entity: EntityId, len: u32) -> Self {
        Self {
            value: PsValue::Array {
                entity,
                start: 0,
                len,
            },
            flags: ObjFlags::executable_composite(),
        }
    }

    /// Dict object.
    pub fn dict(entity: EntityId) -> Self {
        Self {
            value: PsValue::Dict(entity),
            flags: ObjFlags::literal_composite(),
        }
    }

    /// Stopped marker (internal).
    pub fn stopped_mark() -> Self {
        Self {
            value: PsValue::Stopped,
            flags: ObjFlags::executable(),
        }
    }

    /// Loop marker (internal).
    pub fn loop_mark(entity: EntityId) -> Self {
        Self {
            value: PsValue::Loop(entity),
            flags: ObjFlags::executable(),
        }
    }

    /// HardReturn marker (internal).
    pub fn hard_return() -> Self {
        Self {
            value: PsValue::HardReturn,
            flags: ObjFlags::executable(),
        }
    }

    /// DictEnd marker (internal) — conditionally pops the dict stack when reached.
    pub fn dict_end(entity: EntityId) -> Self {
        Self {
            value: PsValue::DictEnd(entity),
            flags: ObjFlags::executable(),
        }
    }

    // --- Type queries ---

    pub fn is_numeric(&self) -> bool {
        matches!(self.value, PsValue::Int(_) | PsValue::Real(_))
    }

    pub fn is_int(&self) -> bool {
        matches!(self.value, PsValue::Int(_))
    }

    pub fn is_real(&self) -> bool {
        matches!(self.value, PsValue::Real(_))
    }

    pub fn is_bool(&self) -> bool {
        matches!(self.value, PsValue::Bool(_))
    }

    pub fn is_array_type(&self) -> bool {
        matches!(
            self.value,
            PsValue::Array { .. } | PsValue::PackedArray { .. }
        )
    }

    pub fn is_composite(&self) -> bool {
        self.flags.is_composite()
    }

    /// PostScript type name as bytes (e.g. `b"integertype"`).
    pub fn type_name(&self) -> &'static [u8] {
        match self.value {
            PsValue::Int(_) => b"integertype",
            PsValue::Real(_) => b"realtype",
            PsValue::Bool(_) => b"booleantype",
            PsValue::Null => b"nulltype",
            PsValue::Mark | PsValue::DictMark => b"marktype",
            PsValue::Name(_) => b"nametype",
            PsValue::String { .. } => b"stringtype",
            PsValue::Array { .. } => b"arraytype",
            PsValue::PackedArray { .. } => b"packedarraytype",
            PsValue::Dict(_) => b"dicttype",
            PsValue::Operator(_) => b"operatortype",
            PsValue::File(_) => b"filetype",
            PsValue::Save(_) => b"savetype",
            PsValue::FontID(_) => b"fonttype",
            _ => b"nulltype", // internal types
        }
    }

    // --- Numeric extraction ---

    /// Extract as `f64` (works for both Int and Real).
    pub fn as_f64(&self) -> Option<f64> {
        match self.value {
            PsValue::Int(v) => Some(v as f64),
            PsValue::Real(v) => Some(v),
            _ => None,
        }
    }

    /// Extract as `i32` (Int only).
    pub fn as_i32(&self) -> Option<i32> {
        match self.value {
            PsValue::Int(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_obj_flags_basic() {
        let f = ObjFlags::new(ObjFlags::ACCESS_UNLIMITED, true, false, true);
        assert_eq!(f.access(), ObjFlags::ACCESS_UNLIMITED);
        assert!(f.is_executable());
        assert!(!f.is_global());
        assert!(f.is_composite());
    }

    #[test]
    fn test_obj_flags_set_literal() {
        let mut f = ObjFlags::executable();
        assert!(f.is_executable());
        f.set_literal();
        assert!(f.is_literal());
    }

    #[test]
    fn test_ps_object_int() {
        let obj = PsObject::int(42);
        assert!(obj.is_int());
        assert!(obj.is_numeric());
        assert!(!obj.is_real());
        assert_eq!(obj.as_i32(), Some(42));
        assert_eq!(obj.as_f64(), Some(42.0));
        assert_eq!(obj.type_name(), b"integertype");
    }

    #[test]
    fn test_ps_object_real() {
        let obj = PsObject::real(3.14);
        assert!(obj.is_real());
        assert!(obj.is_numeric());
        assert_eq!(obj.as_f64(), Some(3.14));
        assert_eq!(obj.as_i32(), None);
        assert_eq!(obj.type_name(), b"realtype");
    }

    #[test]
    fn test_ps_object_copy_semantics() {
        let a = PsObject::int(10);
        let b = a; // Copy
        assert_eq!(a.as_i32(), Some(10));
        assert_eq!(b.as_i32(), Some(10));
    }

    #[test]
    fn test_ps_object_procedure() {
        let obj = PsObject::procedure(EntityId(0), 3);
        assert!(obj.flags.is_executable());
        assert!(obj.flags.is_composite());
        assert!(obj.is_array_type());
    }
}
