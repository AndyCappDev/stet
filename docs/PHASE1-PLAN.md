# xforge Phase 1: Foundation — Detailed Implementation Plan

## Goal

Build the minimal Rust infrastructure to tokenize, parse, and execute basic PostScript programs: type system, arena allocator, tokenizer, execution engine, and ~96 core operators.

**Done when**: `3 4 add 7 eq { (YES\n) print } { (NO\n) print } ifelse` prints `YES`.

**Stretch goal**: PostForge's `arithmetic_tests.ps`, `stack_tests.ps`, and `dict_tests.ps` pass.

---

## Step 1: Project Scaffolding

### 1.1 Create Cargo Workspace

```
~/Projects/xforge/
├── Cargo.toml                  # Workspace root
├── LICENSE                     # AGPL-3.0-or-later
├── README.md
├── ROADMAP.md                  # Already exists
├── PHASE1-PLAN.md              # This file
├── crates/
│   ├── xforge-core/            # Types, arena, VM, tokenizer, context, errors
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── object.rs       # PsObject, PsValue enum, ObjFlags
│   │       ├── arena.rs        # Arena allocator, EntityId, EntityEntry
│   │       ├── stack.rs        # Stack<T> with limit checking
│   │       ├── name.rs         # NameTable (interning: bytes → NameId)
│   │       ├── dict.rs         # PsDict (HashMap-backed PS dictionary)
│   │       ├── string_store.rs # StringStore (byte buffer for PS strings)
│   │       ├── array_store.rs  # ArrayStore (Vec<PsObject> storage)
│   │       ├── context.rs      # Context (stacks, VM refs, state)
│   │       ├── tokenizer.rs    # PostScript tokenizer
│   │       └── error.rs        # PsError enum, error codes
│   ├── xforge-ops/             # Operator implementations
│   │   ├── Cargo.toml          # depends on xforge-core
│   │   └── src/
│   │       ├── lib.rs          # Operator registration (build_system_dict)
│   │       ├── stack_ops.rs    # pop, dup, exch, roll, index, etc.
│   │       ├── math_ops.rs     # add, sub, mul, div, trig, etc.
│   │       ├── relational_ops.rs # eq, ne, lt, gt, and, or, not, bitshift
│   │       ├── type_ops.rs     # type, cvx, cvlit, cvn, cvs, cvrs, cvi, cvr
│   │       ├── dict_ops.rs     # def, begin, end, load, store, where, known
│   │       ├── control_ops.rs  # if, ifelse, for, repeat, loop, forall, exit, stop, stopped
│   │       ├── array_ops.rs    # array, aload, astore, ], }
│   │       ├── string_ops.rs   # string, anchorsearch, search, token
│   │       ├── composite_ops.rs # get, put, getinterval, putinterval, length, copy
│   │       ├── file_ops.rs     # print, =, ==, flush (stdout only for Phase 1)
│   │       └── misc_ops.rs     # bind, null, version, languagelevel, pstack, run
│   ├── xforge-engine/          # Execution engine
│   │   ├── Cargo.toml          # depends on xforge-core, xforge-ops
│   │   └── src/
│   │       ├── lib.rs
│   │       └── eval.rs         # exec_exec equivalent (the core loop)
│   └── xforge-cli/             # Binary entry point
│       ├── Cargo.toml          # depends on xforge-engine
│       └── src/
│           └── main.rs         # CLI: file input or interactive REPL
└── tests/
    └── integration/
        ├── basic_test.rs       # Rust integration tests
        └── ps/                 # PostScript test files (from PostForge)
```

### 1.2 Workspace Cargo.toml

```toml
[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-or-later"
authors = ["Scott Bowman <scott@bowmans.org>"]

[workspace.dependencies]
thiserror = "2"
xforge-core = { path = "crates/xforge-core" }
xforge-ops = { path = "crates/xforge-ops" }
xforge-engine = { path = "crates/xforge-engine" }
```

### 1.3 Initial Files

- `LICENSE`: Full AGPL-3.0 text
- `README.md`: Brief project description, build instructions
- `.gitignore`: Standard Rust gitignore (`/target`, `Cargo.lock` for libs)
- Git init with `main` branch, GitHub remote

---

## Step 2: Core Type System (`xforge-core/src/object.rs`)

### 2.1 Object Flags

```rust
/// Packed object metadata (1 byte)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjFlags(u8);

impl ObjFlags {
    // Bits 0-2: access level (0-4)
    // Bit 3: executable (0=literal, 1=executable)
    // Bit 4: global (0=local, 1=global)
    // Bit 5: composite (0=simple, 1=composite)

    pub const LITERAL: u8 = 0;
    pub const EXECUTABLE: u8 = 1 << 3;

    pub const ACCESS_NONE: u8 = 0;
    pub const ACCESS_EXECUTE_ONLY: u8 = 1;
    pub const ACCESS_READ_ONLY: u8 = 2;
    pub const ACCESS_WRITE_ONLY: u8 = 3;
    pub const ACCESS_UNLIMITED: u8 = 4;

    pub fn new(access: u8, executable: bool, global: bool, composite: bool) -> Self;
    pub fn access(self) -> u8;
    pub fn is_executable(self) -> bool;
    pub fn is_literal(self) -> bool;
    pub fn is_global(self) -> bool;
    pub fn is_composite(self) -> bool;
    pub fn set_executable(&mut self);
    pub fn set_literal(&mut self);
    pub fn set_access(&mut self, access: u8);
}
```

### 2.2 Newtype Indices

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NameId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpCode(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveLevel(pub u32);
```

### 2.3 PsValue Enum

```rust
#[derive(Clone, Copy, Debug)]
pub enum PsValue {
    // Simple types (no arena allocation)
    Null,
    Mark,
    Bool(bool),
    Int(i32),                           // PostScript integers are 32-bit signed
    Real(f64),

    // Interned name (index into NameTable)
    Name(NameId),

    // Composite types (arena-backed)
    String { entity: EntityId, len: u32 },
    Array { entity: EntityId, start: u32, len: u32 },
    PackedArray { entity: EntityId, start: u32, len: u32 },
    Dict(EntityId),

    // Executable types
    Operator(OpCode),

    // Special types
    File(EntityId),
    Save(SaveLevel),

    // Control flow (internal, not user-visible)
    Stopped,
    Loop(EntityId),                     // Points to LoopState in arena
    HardReturn,
}
```

**Design decisions:**
- `Int(i32)`: PostScript spec mandates 32-bit signed integers. `MAX_INT = 2,147,483,647`.
- `Real(f64)`: Standard double precision.
- `Array` has `start` + `len` for `getinterval` subarray support (same backing store).
- `String` has `entity` + `len`; the `entity` points to a byte buffer in StringStore.

### 2.4 PsObject

```rust
#[derive(Clone, Copy, Debug)]
pub struct PsObject {
    pub value: PsValue,
    pub flags: ObjFlags,
}

impl PsObject {
    // Convenience constructors
    pub fn int(v: i32) -> Self;
    pub fn real(v: f64) -> Self;
    pub fn bool(v: bool) -> Self;
    pub fn null() -> Self;
    pub fn mark() -> Self;
    pub fn name_lit(id: NameId) -> Self;    // /foo (literal name)
    pub fn name_exec(id: NameId) -> Self;   // foo (executable name)
    pub fn operator(op: OpCode) -> Self;
    pub fn string(entity: EntityId, len: u32) -> Self;
    pub fn array(entity: EntityId, len: u32) -> Self;
    pub fn procedure(entity: EntityId, len: u32) -> Self; // executable array

    // Type queries
    pub fn is_numeric(&self) -> bool;
    pub fn is_int(&self) -> bool;
    pub fn is_real(&self) -> bool;
    pub fn is_bool(&self) -> bool;
    pub fn is_array_type(&self) -> bool;    // Array | PackedArray
    pub fn is_composite(&self) -> bool;
    pub fn type_name(&self) -> &'static [u8]; // b"integertype", b"realtype", etc.

    // Numeric extraction (for math operators)
    pub fn as_f64(&self) -> Option<f64>;    // Int or Real → f64
    pub fn as_i32(&self) -> Option<i32>;    // Int → i32
}
```

### 2.5 Type Name Mapping

```rust
impl PsObject {
    pub fn type_name(&self) -> &'static [u8] {
        match self.value {
            PsValue::Int(_)         => b"integertype",
            PsValue::Real(_)        => b"realtype",
            PsValue::Bool(_)        => b"booleantype",
            PsValue::Null           => b"nulltype",
            PsValue::Mark           => b"marktype",
            PsValue::Name(_)        => b"nametype",
            PsValue::String { .. }  => b"stringtype",
            PsValue::Array { .. }   => b"arraytype",
            PsValue::PackedArray { .. } => b"packedarraytype",
            PsValue::Dict(_)        => b"dicttype",
            PsValue::Operator(_)    => b"operatortype",
            PsValue::File(_)        => b"filetype",
            PsValue::Save(_)        => b"savetype",
            _ => b"nulltype", // internal types
        }
    }
}
```

---

## Step 3: Storage Infrastructure (`xforge-core/src/`)

### 3.1 Name Table (`name.rs`)

Interning table: maps byte sequences to `NameId` values. Names persist forever (not subject to save/restore or GC).

```rust
pub struct NameTable {
    names: Vec<Vec<u8>>,                    // NameId → bytes
    lookup: HashMap<Vec<u8>, NameId>,       // bytes → NameId
}

impl NameTable {
    pub fn new() -> Self;
    pub fn intern(&mut self, name: &[u8]) -> NameId;   // Get or create
    pub fn get_bytes(&self, id: NameId) -> &[u8];      // NameId → bytes
    pub fn find(&self, name: &[u8]) -> Option<NameId>;  // Lookup without creating
}
```

Pre-intern common names during init: `def`, `begin`, `end`, `if`, `ifelse`, `true`, `false`, `null`, `mark`, all operator names, all type names, all error names.

### 3.2 String Store (`string_store.rs`)

Contiguous byte buffer for PostScript string storage. Strings reference `(offset, length)` into this buffer.

```rust
pub struct StringStore {
    data: Vec<u8>,
}

impl StringStore {
    pub fn new() -> Self;
    pub fn allocate(&mut self, len: usize) -> EntityId;     // Reserve len bytes (zeroed), return ID
    pub fn allocate_from(&mut self, bytes: &[u8]) -> EntityId; // Copy bytes in
    pub fn get(&self, entity: EntityId, len: u32) -> &[u8];
    pub fn get_mut(&mut self, entity: EntityId, len: u32) -> &mut [u8];
    pub fn put_byte(&mut self, entity: EntityId, offset: u32, byte: u8);
    pub fn get_byte(&self, entity: EntityId, offset: u32) -> u8;
}
```

**EntityId for strings**: Stores the byte offset into `data`. `len` is stored in the PsObject itself.

**Phase 1 simplification**: Single string store (no global/local split yet — that's Phase 2). All strings go to one store.

### 3.3 Array Store (`array_store.rs`)

Storage for PostScript array contents. Each array is a contiguous range of `PsObject` values.

```rust
pub struct ArrayStore {
    data: Vec<PsObject>,
}

impl ArrayStore {
    pub fn new() -> Self;
    pub fn allocate(&mut self, len: usize) -> EntityId;     // Reserve len slots (null-filled)
    pub fn allocate_from(&mut self, items: &[PsObject]) -> EntityId;
    pub fn get(&self, entity: EntityId, start: u32, len: u32) -> &[PsObject];
    pub fn get_mut(&mut self, entity: EntityId, start: u32, len: u32) -> &mut [PsObject];
    pub fn get_element(&self, entity: EntityId, index: u32) -> PsObject;
    pub fn set_element(&mut self, entity: EntityId, index: u32, obj: PsObject);
}
```

**EntityId for arrays**: Stores the index offset into `data`. `start` and `len` are in the PsObject for subarray support.

### 3.4 Dict Store (`dict.rs`)

PostScript dictionaries backed by `HashMap`. Each dict has a unique `EntityId`.

```rust
/// Key type for PostScript dictionary entries
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DictKey {
    Name(NameId),
    Int(i32),
    Real(u64),          // f64 bits for hashable comparison
    Bool(bool),
    String(Vec<u8>),    // String keys are copied (per PLRM)
}

pub struct DictEntry {
    pub max_length: usize,
    pub entries: HashMap<DictKey, PsObject>,
    pub access: u8,
    pub name: Vec<u8>,  // b"systemdict", b"userdict", etc.
}

pub struct DictStore {
    dicts: Vec<DictEntry>,
}

impl DictStore {
    pub fn new() -> Self;
    pub fn allocate(&mut self, max_length: usize, name: &[u8]) -> EntityId;
    pub fn get(&self, entity: EntityId, key: &DictKey) -> Option<PsObject>;
    pub fn put(&mut self, entity: EntityId, key: DictKey, value: PsObject);
    pub fn known(&self, entity: EntityId, key: &DictKey) -> bool;
    pub fn length(&self, entity: EntityId) -> usize;
    pub fn max_length(&self, entity: EntityId) -> usize;
    pub fn remove(&mut self, entity: EntityId, key: &DictKey);
    pub fn entry(&self, entity: EntityId) -> &DictEntry;
    pub fn entry_mut(&mut self, entity: EntityId) -> &mut DictEntry;
    pub fn keys(&self, entity: EntityId) -> impl Iterator<Item = &DictKey>;
}
```

**DictKey design**: PostForge uses Python dict keys directly. In Rust, we need an explicit key enum. `Real` uses bit representation for hashing (f64 is not `Hash`). `String` keys are cloned on insertion per PLRM spec.

---

## Step 4: Context & Stack (`xforge-core/src/`)

### 4.1 Stack (`stack.rs`)

```rust
pub struct Stack {
    data: Vec<PsObject>,
    max_size: usize,
}

impl Stack {
    pub fn new(max_size: usize) -> Self;
    pub fn push(&mut self, obj: PsObject) -> Result<(), PsError>;  // Checks overflow
    pub fn pop(&mut self) -> Result<PsObject, PsError>;            // Checks underflow
    pub fn peek(&self, from_top: usize) -> Result<PsObject, PsError>; // 0 = top
    pub fn peek_mut(&mut self, from_top: usize) -> Result<&mut PsObject, PsError>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn clear(&mut self);
    pub fn as_slice(&self) -> &[PsObject];
    pub fn truncate(&mut self, len: usize);     // For cleartomark, exit, stop
    pub fn swap_top_two(&mut self) -> Result<(), PsError>; // For exch
}
```

**Stack limits** (from PostForge):
- Operand stack: 500
- Execution stack: 250
- Dictionary stack: 250

### 4.2 Context (`context.rs`)

```rust
pub struct Context {
    // Stacks
    pub o_stack: Stack,                 // Operand stack (500)
    pub e_stack: Stack,                 // Execution stack (250)
    pub d_stack: Vec<EntityId>,         // Dictionary stack (EntityIds into DictStore)

    // Storage
    pub strings: StringStore,
    pub arrays: ArrayStore,
    pub dicts: DictStore,
    pub names: NameTable,

    // Operator table
    pub operators: Vec<OpEntry>,        // OpCode → function pointer + name

    // Well-known dict IDs
    pub systemdict: EntityId,
    pub globaldict: EntityId,
    pub userdict: EntityId,
    pub errordict: EntityId,
    pub dollar_error: EntityId,         // $error dict

    // State
    pub vm_alloc_mode: bool,            // false=local, true=global (Phase 1: always local)
    pub rand_state: u64,                // RNG state for rand/srand/rrand

    // Pre-interned names (for fast comparison in hot paths)
    pub name_cache: NameCache,

    // Stdout handle for print/= operators
    pub stdout: std::io::Stdout,
}

pub struct OpEntry {
    pub func: fn(&mut Context) -> Result<(), PsError>,
    pub name: NameId,
}

/// Pre-interned NameIds for frequently-used names
pub struct NameCache {
    pub n_def: NameId,
    pub n_true: NameId,
    pub n_false: NameId,
    pub n_null: NameId,
    pub n_mark: NameId,
    pub n_stopped: NameId,
    // ... type names, error names, common operators
}
```

### 4.3 Dictionary Stack Operations

```rust
impl Context {
    /// Look up a name in the dictionary stack (top to bottom)
    pub fn dict_load(&self, key: &DictKey) -> Option<PsObject>;

    /// Look up and return (dict_entity, value) pair
    pub fn dict_where(&self, key: &DictKey) -> Option<(EntityId, PsObject)>;

    /// Store in current dict (top of d_stack)
    pub fn dict_def(&mut self, key: DictKey, value: PsObject) -> Result<(), PsError>;

    /// Store in first dict that contains key, or current dict if not found
    pub fn dict_store(&mut self, key: DictKey, value: PsObject) -> Result<(), PsError>;

    /// Convert PsObject to DictKey
    pub fn make_dict_key(&self, obj: &PsObject) -> Result<DictKey, PsError>;
}
```

---

## Step 5: Error System (`xforge-core/src/error.rs`)

```rust
#[derive(Debug, Clone, thiserror::Error)]
pub enum PsError {
    #[error("VMerror")]              VMError,           // 0
    #[error("dictfull")]             DictFull,          // 1
    #[error("dictstackoverflow")]    DictStackOverflow, // 2
    #[error("dictstackunderflow")]   DictStackUnderflow,// 3
    #[error("execstackoverflow")]    ExecStackOverflow, // 4
    #[error("invalidaccess")]        InvalidAccess,     // 5
    #[error("invalidexit")]          InvalidExit,       // 6
    #[error("invalidfileaccess")]    InvalidFileAccess, // 7
    #[error("invalidfont")]          InvalidFont,       // 8
    #[error("invalidrestore")]       InvalidRestore,    // 9
    #[error("ioerror")]              IOError,           // 10
    #[error("limitcheck")]           LimitCheck,        // 11
    #[error("nocurrentpoint")]       NoCurrentPoint,    // 12
    #[error("rangecheck")]           RangeCheck,        // 13
    #[error("stackoverflow")]        StackOverflow,     // 14
    #[error("stackunderflow")]       StackUnderflow,    // 15
    #[error("syntaxerror")]          SyntaxError,       // 16
    #[error("timeout")]              Timeout,           // 17
    #[error("typecheck")]            TypeCheck,         // 18
    #[error("undefined")]            Undefined,         // 19
    #[error("undefinedfilename")]    UndefinedFilename, // 20
    #[error("undefinedresource")]    UndefinedResource, // 21
    #[error("undefinedresult")]      UndefinedResult,   // 22
    #[error("unmatchedmark")]        UnmatchedMark,     // 23
    #[error("unregistered")]         Unregistered,      // 24
    #[error("unsupported")]          Unsupported,       // 25
    #[error("configurationerror")]   ConfigurationError,// 26

    // Internal (not PostScript errors)
    #[error("quit")]                 Quit,
    #[error("stop")]                 Stop,              // stop operator (not an error per se)
    #[error("exit")]                 Exit,              // exit operator
}
```

**Phase 1 error handling**: Errors print to stderr and clear stacks (simplified). Full PostScript error dictionary dispatch (errordict, $error, handleerror) comes in Phase 2 when we have file I/O and the resource system.

---

## Step 6: Tokenizer (`xforge-core/src/tokenizer.rs`)

### 6.1 Token Types

```rust
pub enum Token {
    Int(i32),
    Real(f64),
    Name(Vec<u8>, bool),          // (bytes, is_executable)
    LiteralName(Vec<u8>),         // /name
    ImmediateName(Vec<u8>),       // //name
    String(Vec<u8>),              // (hello) or <hex>
    ProcBegin,                    // {
    ProcEnd,                      // }
    ArrayBegin,                   // [
    ArrayEnd,                     // ]
    DictBegin,                    // <<
    DictEnd,                      // >>
    Eof,
}
```

### 6.2 Tokenizer Structure

```rust
pub struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a [u8]) -> Self;
    pub fn next_token(&mut self) -> Result<Option<Token>, PsError>;

    // Internal helpers
    fn skip_whitespace_and_comments(&mut self);
    fn scan_number(&mut self) -> Result<Token, PsError>;
    fn scan_name(&mut self) -> Token;
    fn scan_literal_name(&mut self) -> Token;         // After /
    fn scan_immediate_name(&mut self) -> Token;       // After //
    fn scan_string(&mut self) -> Result<Token, PsError>;       // (...)
    fn scan_hex_string(&mut self) -> Result<Token, PsError>;   // <...>
    fn scan_ascii85_string(&mut self) -> Result<Token, PsError>; // <~...~>
    fn is_whitespace(b: u8) -> bool;
    fn is_delimiter(b: u8) -> bool;
}
```

### 6.3 Key Tokenizer Rules

**Whitespace**: `\0`, `\t`, `\n`, `\x0c` (form feed), `\r`, ` ` (space)

**Delimiters**: `(`, `)`, `<`, `>`, `[`, `]`, `{`, `}`, `/`, `%`

**Number parsing**:
- Integers: optional sign + digits, must fit i32 (-2,147,483,648 to 2,147,483,647)
- Reals: digits with `.` or `e`/`E` exponent
- Radix: `base#digits` where base is 2-36
- Overflow: integers that overflow i32 → promote to Real

**String escapes** (inside parentheses):
- `\n` `\r` `\t` `\b` `\f` `\\` `\(` `\)`
- `\nnn` (octal, 1-3 digits, masked to 8 bits)
- `\<newline>` (line continuation, stripped)
- Unrecognized `\x` → just `x`
- Balanced parentheses allowed inside: `(a(b)c)` is valid

**Hex strings**: Pairs of hex digits, whitespace ignored, odd trailing nibble padded with 0.

**ASCII85**: `<~` ... `~>`, groups of 5 chars decode to 4 bytes, `z` = four zero bytes.

---

## Step 7: Execution Engine (`xforge-engine/src/eval.rs`)

### 7.1 Core Eval Loop

```rust
pub fn eval(ctx: &mut Context) -> Result<(), PsError> {
    while let Some(obj) = ctx.e_stack.try_pop() {
        // Path 1: Literal objects → push to operand stack
        if obj.flags.is_literal() && !matches!(obj.value, PsValue::Stopped | PsValue::Loop(_) | PsValue::HardReturn) {
            ctx.o_stack.push(obj)?;
            continue;
        }

        match obj.value {
            // Path 1b: Simple types always push (even if executable)
            PsValue::Int(_) | PsValue::Real(_) | PsValue::Bool(_)
            | PsValue::Null | PsValue::Mark => {
                ctx.o_stack.push(obj)?;
            }

            // Path 2: Operators → dispatch
            PsValue::Operator(opcode) => {
                let func = ctx.operators[opcode.0 as usize].func;
                match func(ctx) {
                    Ok(()) => {}
                    Err(PsError::Quit) => return Ok(()),
                    Err(PsError::Stop) => {
                        // Unwind e_stack to Stopped marker
                        unwind_to_stopped(ctx)?;
                        ctx.o_stack.push(PsObject::bool(true))?;
                    }
                    Err(PsError::Exit) => {
                        // Unwind e_stack to Loop marker
                        unwind_to_loop(ctx)?;
                    }
                    Err(e) => {
                        // Phase 1: print error and continue
                        // Phase 2+: dispatch to errordict
                        eprintln!("Error: {}", e);
                    }
                }
            }

            // Path 3: Names → dictionary lookup
            PsValue::Name(name_id) => {
                let key = DictKey::Name(name_id);
                match ctx.dict_load(&key) {
                    Some(val) => {
                        if val.flags.is_executable() {
                            ctx.e_stack.push(val)?;
                        } else {
                            ctx.o_stack.push(val)?;
                        }
                    }
                    None => {
                        let name_bytes = ctx.names.get_bytes(name_id);
                        eprintln!("Error: undefined: {}", String::from_utf8_lossy(name_bytes));
                    }
                }
            }

            // Path 4: Executable arrays (procedures)
            PsValue::Array { entity, start, len } => {
                exec_procedure(ctx, entity, start, len)?;
            }

            // Path 5: Executable strings → tokenize and execute
            PsValue::String { entity, len } => {
                exec_string(ctx, entity, len)?;
            }

            // Path 6: Stopped marker → push false (normal completion)
            PsValue::Stopped => {
                ctx.o_stack.push(PsObject::bool(false))?;
            }

            // Path 7: Loop state → advance loop
            PsValue::Loop(loop_entity) => {
                advance_loop(ctx, loop_entity)?;
            }

            // Path 8: HardReturn → just consume (exit current procedure)
            PsValue::HardReturn => {}

            // Everything else → push to operand stack
            _ => {
                ctx.o_stack.push(obj)?;
            }
        }
    }
    Ok(())
}
```

### 7.2 Procedure Execution

```rust
fn exec_procedure(ctx: &mut Context, entity: EntityId, start: u32, len: u32) -> Result<(), PsError> {
    if len == 0 {
        return Ok(()); // Empty procedure
    }

    let elements = ctx.arrays.get(entity, start, len);

    // Push elements in reverse order onto execution stack
    // (last element pushed first, so first element is on top)
    for i in (0..len).rev() {
        let elem = elements[i as usize];

        // CRITICAL: Copy executable arrays to prevent cvlit corruption
        // (see PostForge's procedure element copy rule)
        let to_push = if matches!(elem.value, PsValue::Array { .. }) && elem.flags.is_executable() {
            // Clone the array backing (or just the PsObject — it's Copy)
            // The PsObject is Copy, so this is fine for Phase 1
            // Full copy-on-write semantics come in Phase 2
            elem
        } else {
            elem
        };

        ctx.e_stack.push(to_push)?;
    }
    Ok(())
}
```

**Note on procedure element copy rule**: In PostForge, this is critical because Python objects are mutable references. In Rust, `PsObject` is `Copy` (it's a value type containing indices, not references), so pushing it to the e_stack creates an independent copy by default. The procedure body array data in `ArrayStore` is never mutated by `cvlit` — `cvlit` only changes the `flags` field on the `PsObject` value that's on the o_stack. Since `PsObject` is `Copy`, this is safe without explicit cloning. This is a structural advantage of the Rust design.

### 7.3 String Execution

```rust
fn exec_string(ctx: &mut Context, entity: EntityId, len: u32) -> Result<(), PsError> {
    let bytes = ctx.strings.get(entity, len).to_vec();
    let mut tokenizer = Tokenizer::new(&bytes);

    while let Some(token) = tokenizer.next_token()? {
        let obj = token_to_object(ctx, token)?;
        if obj.flags.is_executable() {
            ctx.e_stack.push(obj)?;
            eval(ctx)?; // Execute each token immediately
        } else {
            ctx.o_stack.push(obj)?;
        }
    }
    Ok(())
}
```

### 7.4 Loop State Machine

```rust
pub struct LoopState {
    pub loop_type: LoopType,
    pub proc_entity: EntityId,
    pub proc_start: u32,
    pub proc_len: u32,

    // for/repeat state
    pub counter: f64,
    pub increment: f64,
    pub limit: f64,
    pub use_int: bool,      // true if all values are integral

    // forall state
    pub source: PsObject,   // array/dict/string being iterated
    pub index: u32,          // current position
}

pub enum LoopType {
    For,
    Repeat,
    Loop,
    Forall,
}
```

---

## Step 8: Operator Implementations (`xforge-ops/src/`)

### 8.1 Operator Registration

```rust
// lib.rs
pub fn build_system_dict(ctx: &mut Context) -> EntityId {
    let sd = ctx.dicts.allocate(400, b"systemdict");

    // Register each operator
    register(ctx, sd, "add", math_ops::op_add);
    register(ctx, sd, "sub", math_ops::op_sub);
    register(ctx, sd, "mul", math_ops::op_mul);
    // ... all ~96 Phase 1 operators

    // Register constants
    let true_obj = PsObject::bool(true);
    let false_obj = PsObject::bool(false);
    let null_obj = PsObject::null();
    ctx.dicts.put(sd, DictKey::Name(ctx.names.intern(b"true")), true_obj);
    ctx.dicts.put(sd, DictKey::Name(ctx.names.intern(b"false")), false_obj);
    ctx.dicts.put(sd, DictKey::Name(ctx.names.intern(b"null")), null_obj);
    // ... version, languagelevel, etc.

    sd
}

fn register(ctx: &mut Context, dict: EntityId, name: &str, func: fn(&mut Context) -> Result<(), PsError>) {
    let name_id = ctx.names.intern(name.as_bytes());
    let opcode = OpCode(ctx.operators.len() as u16);
    ctx.operators.push(OpEntry { func, name: name_id });
    let op_obj = PsObject::operator(opcode);
    ctx.dicts.put(dict, DictKey::Name(name_id), op_obj);
}
```

### 8.2 Operator Function Signature

All operators follow this pattern:

```rust
pub fn op_name(ctx: &mut Context) -> Result<(), PsError> {
    // 1. Validate stack depth
    if ctx.o_stack.len() < N {
        return Err(PsError::StackUnderflow);
    }

    // 2. Validate types (peek, don't pop)
    let arg = ctx.o_stack.peek(0)?;
    match arg.value {
        PsValue::Int(_) | PsValue::Real(_) => {}
        _ => return Err(PsError::TypeCheck),
    }

    // 3. Validate access/ranges (if applicable)

    // 4. ONLY NOW pop operands
    let arg = ctx.o_stack.pop()?;

    // 5. Execute
    let result = /* ... */;

    // 6. Push result
    ctx.o_stack.push(result)?;
    Ok(())
}
```

### 8.3 Phase 1 Operator Inventory (96 operators)

**Stack (11):** `pop`, `dup`, `exch`, `copy`, `index`, `roll`, `clear`, `count`, `mark`, `cleartomark`, `counttomark`

**Math (24):** `add`, `sub`, `mul`, `div`, `idiv`, `mod`, `abs`, `neg`, `ceiling`, `floor`, `round`, `truncate`, `sqrt`, `exp`, `ln`, `log`, `sin`, `cos`, `atan`, `rand`, `srand`, `rrand`, `max`, `min`

**Relational/Boolean/Bitwise (11):** `eq`, `ne`, `gt`, `ge`, `lt`, `le`, `and`, `or`, `xor`, `not`, `bitshift`

**Type/Conversion (14):** `type`, `cvx`, `cvlit`, `cvn`, `cvs`, `cvrs`, `cvi`, `cvr`, `xcheck`, `executeonly`, `noaccess`, `readonly`, `rcheck`, `wcheck`

**Control Flow (11):** `exec`, `if`, `ifelse`, `for`, `repeat`, `loop`, `forall`, `exit`, `stop`, `stopped`, `quit`

**Dictionary (14):** `dict`, `begin`, `end`, `def`, `load`, `store`, `known`, `where`, `maxlength`, `currentdict`, `countdictstack`, `dictstack`, `undef`, `cleardictstack`

**Array (4):** `array`, `aload`, `astore`, `]` (array_from_mark)

**String (3):** `string`, `anchorsearch`, `search`

**Composite (7):** `get`, `put`, `getinterval`, `putinterval`, `length`, `copy` (composite form), `forall` (already in control)

**Output (3):** `print`, `=`, `==` (write to stdout only — minimal file I/O)

**Misc (6):** `bind`, `null`, `version`, `languagelevel`, `pstack`, `run`

**Total: ~96 operators** (some like `copy`/`forall` serve double duty)

### 8.4 Key Implementation Notes

**Math type promotion:**
- `Int OP Int` → `Int` if result fits i32, else → `Real`
- `Int OP Real` or `Real OP Int` → `Real`
- `Real OP Real` → `Real`
- Use `i32::checked_add/sub/mul` for overflow detection
- `div` always returns `Real`; `idiv`/`mod` require both `Int`

**PostScript integer range:** -2,147,483,648 to 2,147,483,647 (i32)

**`atan` returns degrees in [0, 360):** Use `f64::atan2(num, den).to_degrees()`, add 360 if negative.

**`mod` sign rule:** Result sign matches dividend. `(-5) 3 mod` → `-2`.

**`round` tie-breaking:** 0.5 rounds UP (toward +infinity): `floor(x + 0.5)`.

**`eq` cross-type:** `Int == Real` compares values: `4 4.0 eq` → `true`. `String == Name` compares bytes.

**`and`/`or`/`xor`/`not` dual-mode:** Both operands must be same type (both Bool OR both Int). Bool → logical, Int → bitwise.

**`copy` is polymorphic:** If top is Int → stack copy. If top is composite → composite copy. Detect by checking if operand 1 (top) is an Int and operand 2 is also reachable.

---

## Step 9: CLI Entry Point (`xforge-cli/src/main.rs`)

### Phase 1 CLI (minimal)

```rust
fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut ctx = Context::new();
    build_system_dict(&mut ctx);

    if args.len() > 1 {
        // File mode: read and execute PS file
        let filename = &args[1];
        let source = std::fs::read(filename).expect("Cannot read file");
        execute_ps(&mut ctx, &source);
    } else {
        // Interactive mode: simple REPL
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            eprint!("PS> ");
            line.clear();
            if stdin.read_line(&mut line).unwrap() == 0 {
                break; // EOF
            }
            execute_ps(&mut ctx, line.as_bytes());
        }
    }
}

fn execute_ps(ctx: &mut Context, source: &[u8]) {
    // Tokenize source into objects, push to e_stack, eval
    let mut tokenizer = Tokenizer::new(source);
    // ... parse all tokens into objects, handle procedures as nested arrays
    // Push resulting executable object(s) onto e_stack
    // Call eval(ctx)
}
```

---

## Step 10: Testing Strategy

### 10.1 Rust Unit Tests

Each operator module gets `#[cfg(test)] mod tests` with:
- Happy path for each operator
- Each PLRM-specified error condition
- Type promotion edge cases (math ops)
- Boundary values (MAX_INT, 0, negative)

Example:
```rust
#[test]
fn test_add_int_int() {
    let mut ctx = test_context();
    ctx.o_stack.push(PsObject::int(3)).unwrap();
    ctx.o_stack.push(PsObject::int(4)).unwrap();
    op_add(&mut ctx).unwrap();
    assert_eq!(ctx.o_stack.pop().unwrap().value, PsValue::Int(7));
}

#[test]
fn test_add_overflow_to_real() {
    let mut ctx = test_context();
    ctx.o_stack.push(PsObject::int(i32::MAX)).unwrap();
    ctx.o_stack.push(PsObject::int(1)).unwrap();
    op_add(&mut ctx).unwrap();
    match ctx.o_stack.pop().unwrap().value {
        PsValue::Real(v) => assert_eq!(v, i32::MAX as f64 + 1.0),
        _ => panic!("Expected Real"),
    }
}

#[test]
fn test_add_underflow() {
    let mut ctx = test_context();
    ctx.o_stack.push(PsObject::int(1)).unwrap();
    assert_eq!(op_add(&mut ctx), Err(PsError::StackUnderflow));
}
```

### 10.2 Integration Tests

```rust
// tests/integration/basic_test.rs
fn run_ps(source: &str) -> String {
    let mut ctx = Context::new();
    build_system_dict(&mut ctx);
    // Capture stdout
    execute_ps(&mut ctx, source.as_bytes());
    // Return captured output
}

#[test]
fn test_hello_world() {
    assert_eq!(run_ps("(Hello World\n) print"), "Hello World\n");
}

#[test]
fn test_arithmetic() {
    assert_eq!(run_ps("3 4 add ="), "7\n");
}

#[test]
fn test_conditional() {
    assert_eq!(
        run_ps("3 4 add 7 eq { (YES\n) print } { (NO\n) print } ifelse"),
        "YES\n"
    );
}
```

### 10.3 PostScript Test Suite Integration (Stretch Goal)

Port PostForge's `unittest.ps` assert framework and run:
- `arithmetic_tests.ps` — math operator coverage
- `stack_tests.ps` — stack operator coverage
- `dict_tests.ps` — dictionary operator coverage
- `array_tests.ps` — array operator coverage
- `string_tests.ps` — string operator coverage
- `control_flow_tests.ps` — control flow coverage

---

## Implementation Order

This is the recommended build sequence, where each step builds on the previous:

1. **`object.rs`** — PsValue, PsObject, ObjFlags, newtype IDs
2. **`error.rs`** — PsError enum
3. **`name.rs`** — NameTable
4. **`stack.rs`** — Stack with push/pop/peek
5. **`string_store.rs`** — StringStore
6. **`array_store.rs`** — ArrayStore
7. **`dict.rs`** — DictStore, DictKey
8. **`context.rs`** — Context struct, dict stack operations
9. **`tokenizer.rs`** — Full tokenizer (numbers, names, strings, procedures)
10. **`eval.rs`** — Core execution loop (names, operators, procedures)
11. **`stack_ops.rs`** — Stack operators (pop, dup, exch, etc.)
12. **`math_ops.rs`** — Arithmetic operators
13. **`relational_ops.rs`** — Comparison and boolean operators
14. **`type_ops.rs`** — Type conversion operators
15. **`dict_ops.rs`** — Dictionary operators
16. **`composite_ops.rs`** — get, put, length, copy, getinterval, putinterval
17. **`array_ops.rs`** — array, aload, astore, ] (array_from_mark)
18. **`string_ops.rs`** — string, anchorsearch, search
19. **`control_ops.rs`** — if, ifelse, for, repeat, loop, forall, exit, stop, stopped
20. **`file_ops.rs`** — print, =, == (stdout only)
21. **`misc_ops.rs`** — bind, null, version, languagelevel, pstack
22. **`lib.rs` (ops)** — build_system_dict registration
23. **`main.rs`** — CLI entry point with file and interactive modes
24. **Integration tests** — verify end-to-end PS execution

---

## Success Criteria

Phase 1 is complete when:

1. `cargo build` succeeds with no warnings
2. `cargo test` passes all unit tests (one per operator + edge cases)
3. The following PS programs execute correctly:
   - `3 4 add =` → prints `7`
   - `(Hello World\n) print` → prints `Hello World`
   - `3 4 add 7 eq { (YES\n) print } { (NO\n) print } ifelse` → prints `YES`
   - `1 1 10 { add } for =` → prints `55`
   - `/square { dup mul } def 7 square =` → prints `49`
   - `[1 2 3 4 5] { 2 mul } forall count { = } repeat` → prints `2 4 6 8 10`
4. Interactive REPL works: type PS at prompt, see results
