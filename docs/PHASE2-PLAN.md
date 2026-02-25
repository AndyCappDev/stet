# xforge Phase 2: VM & Persistence — Detailed Implementation Plan

## Goal

Add PostScript virtual memory semantics: save/restore with copy-on-write, dual VM (global/local allocation), complete file I/O, and proper error handling via errordict.

**Done when**: Programs using `save`/`restore`, `setglobal`/`currentglobal`, file read/write, and `stopped` error catching execute correctly.

**Stretch goal**: PostForge's `vm_operators_tests.ps` and `file_operators_tests.ps` pass (adapted).

---

## Architectural Decisions

### Decision 1: Entity Table Indirection

**Problem**: Phase 1 stores use `EntityId` as a direct offset/index into flat backing vecs. Save/restore requires the ability to swap entity addresses on restore (the xpost pattern called for in ROADMAP.md).

**Solution**: Add an `EntityTable` to each store that maps `EntityId → EntityMeta(offset, len, save_level, is_global)`. All store access goes through this indirection layer.

```
EntityId  →  EntityTable  →  (offset into backing Vec)
               [ 0: {offset: 0,   len: 5, save_level: 0} ]   // "hello"
               [ 1: {offset: 5,   len: 3, save_level: 0} ]   // "abc"
               [ 2: {offset: 8,   len: 7, save_level: 1} ]   // "new str"
```

The public API of each store barely changes — methods still take `EntityId + len/start`. Only the internal implementation adds the indirection hop.

**Reference**: xpost's `Xpost_Memory_Table` in `xpost_memory.h` — segmented entity table with `(adr, used, sz, mark, tag)` per entry.

### Decision 2: Copy-on-Write Save/Restore

Following xpost's entity-swapping pattern rather than PostForge's pickle-based cloning:

1. **`save`**: Increment save level counter. No immediate data copying.
2. **First mutation after save**: Before modifying an entity whose `save_level < current_level`:
   - Copy entity's data to a new region in the backing vec
   - Create a new entity pointing to the **original** data (the "copy")
   - Update the **source** entity's offset to point to the new (mutable) copy
   - Record `SaveRecord { src, copy, store_type }` on the current save level
   - Set `src.save_level = current_level`
3. **`restore`**: For each `SaveRecord`, swap `entity_table[src].offset ↔ entity_table[copy].offset`. This reverts all mutations because the original data is now pointed to by `src` again.

**Rust borrow-checker note**: COW copies need `let temp = data[range].to_vec(); data.extend(&temp)` to avoid simultaneous mutable+immutable borrows of the same Vec.

**Reference**: xpost's `xpost_save.c` — `saverec = { src, cpy }` pattern, `xpost_save_restore_snapshot()` swaps address fields.

### Decision 3: Dual VM — Lightweight

Track `is_global` as a flag per entity table entry. Unified stores (one StringStore, one ArrayStore, one DictStore) — not separate global/local instances. The `vm_alloc_mode` flag on Context controls where new allocations are tagged. Global entities skip local save/restore COW.

The `ObjFlags` already has a `GLOBAL_BIT` (bit 4). Phase 2 uses this consistently.

**Reference**: PostForge's `vm_alloc_mode` flag; xpost's `vmmode = GLOBAL | LOCAL` in context.

### Decision 4: File I/O Scope

Implement 19 new file operators (expanding file_ops.rs from 5 to 24). New `FileStore` in xforge-core holds open file handles. Filter-based I/O (flate, ascii85, etc.) deferred to Phase 5.

**Reference**: PostForge's `file_types.py` (File, StandardFile, StandardFileProxy classes) and `file.py` operators.

### Decision 5: Error Handling via errordict

Replace Phase 1's `eprintln!` catch-all with PLRM-compliant error dispatch: look up error name in errordict, invoke handler procedure (which populates `$error` and calls `stop`). Default `handleerror` in systemdict prints to stderr.

Guard against infinite error loops with `in_error_handler: bool` flag.

**Reference**: PLRM Section 3.11; PostForge's `handle_error()` in interpreter.py.

### Decision 6: GC — Deferred

Not needed for Phase 2's scope. Stores remain append-only. The `EntityMeta` struct reserves a `gc_mark` field for future use (Phase 3+).

---

## Step 1: Entity Table Module

**New file**: `crates/xforge-core/src/entity_table.rs`

Standalone indirection layer with no dependencies on existing stores.

```rust
/// Metadata for a single entity in a backing store.
#[derive(Clone, Debug)]
pub struct EntityMeta {
    pub offset: u32,        // Byte/element offset into backing Vec
    pub len: u32,           // Allocated capacity
    pub save_level: u16,    // Save level when entity was created or last COW-copied
    pub flags: u8,          // Bit 0: is_global, Bit 1: gc_mark, Bit 2: freed
}

/// Indirection table: EntityId → EntityMeta.
pub struct EntityTable {
    entries: Vec<EntityMeta>,
}

impl EntityTable {
    pub fn new() -> Self;

    /// Allocate a new entity entry, returning its EntityId.
    pub fn allocate(&mut self, offset: u32, len: u32, save_level: u16, is_global: bool) -> EntityId;

    /// Look up entity metadata by ID.
    pub fn get(&self, id: EntityId) -> &EntityMeta;
    pub fn get_mut(&mut self, id: EntityId) -> &mut EntityMeta;

    /// Number of allocated entities.
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

**Tests (~8)**: allocate, get, get_mut, boundary checks, is_global flag, save_level tracking.

---

## Step 2: Refactor StringStore with EntityTable

**Modify**: `crates/xforge-core/src/string_store.rs`

The backing `Vec<u8>` remains identical. The change: `EntityId` no longer IS the offset; it's an index into the entity table which holds the offset.

Before:
```rust
pub struct StringStore { data: Vec<u8> }

pub fn allocate_from(&mut self, bytes: &[u8]) -> EntityId {
    let offset = self.data.len();
    self.data.extend_from_slice(bytes);
    EntityId(offset as u32)  // EntityId = raw offset
}

pub fn get(&self, entity: EntityId, len: u32) -> &[u8] {
    let start = entity.0 as usize;  // Direct offset access
    &self.data[start..start + len as usize]
}
```

After:
```rust
pub struct StringStore {
    data: Vec<u8>,
    pub entities: EntityTable,
}

pub fn allocate_from(&mut self, bytes: &[u8]) -> EntityId {
    let offset = self.data.len();
    self.data.extend_from_slice(bytes);
    self.entities.allocate(offset as u32, bytes.len() as u32, 0, false)
}

pub fn get(&self, entity: EntityId, len: u32) -> &[u8] {
    let meta = self.entities.get(entity);
    let start = meta.offset as usize;
    &self.data[start..start + len as usize]
}
```

All existing tests pass unchanged — zero behavioral change, only internal indirection.

**Tests**: +4 new tests verifying entity table indirection works correctly.

---

## Step 3: Refactor ArrayStore with EntityTable

**Modify**: `crates/xforge-core/src/array_store.rs`

Same pattern as StringStore. Backing `Vec<PsObject>` unchanged. Entity table provides indirection.

```rust
pub struct ArrayStore {
    data: Vec<PsObject>,
    pub entities: EntityTable,
}
```

---

## Step 4: Refactor DictStore with EntityTable

**Modify**: `crates/xforge-core/src/dict.rs`

DictStore already uses `Vec<DictEntry>` indexed by `EntityId.0`. The refactor adds entity table wrapper for uniformity and save_level tracking. Functionally, `entity_table[id].offset == id.0` initially, but the indirection enables future COW address swapping.

```rust
pub struct DictStore {
    dicts: Vec<DictEntry>,
    pub entities: EntityTable,
}
```

---

## Step 5: SaveStack Module

**New file**: `crates/xforge-core/src/save_stack.rs`

```rust
/// A record of one entity that was COW-copied at a save level.
#[derive(Clone, Debug)]
pub struct SaveRecord {
    pub src: EntityId,          // The entity that was modified (now points to copy)
    pub copy: EntityId,         // The copy holding the original data
    pub store_type: StoreType,  // Which store (String, Array, Dict)
}

/// Which backing store holds this entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreType {
    String,
    Array,
    Dict,
}

/// State for one save level.
#[derive(Clone, Debug)]
pub struct SaveFrame {
    pub level: u16,                 // Nesting depth (1-based)
    pub save_id: u32,               // Unique monotonic ID for invalidation
    pub records: Vec<SaveRecord>,   // COW records at this level
    pub valid: bool,                // Becomes false after restore
    pub d_stack_depth: usize,       // Dict stack depth at time of save
    pub vm_alloc_mode: bool,        // Saved allocation mode
}

/// The save stack — manages nested save/restore levels.
pub struct SaveStack {
    frames: Vec<SaveFrame>,
    next_save_id: u32,
}

impl SaveStack {
    pub fn new() -> Self;

    /// Create a new save level. Returns (new_level, save_id).
    pub fn save(&mut self, d_stack_depth: usize, vm_alloc_mode: bool) -> (u16, u32);

    /// Record a COW copy at the current level.
    pub fn add_record(&mut self, record: SaveRecord);

    /// Current nesting depth (0 = no active saves).
    pub fn current_level(&self) -> u16;

    /// Check if a save_id is still valid.
    pub fn is_valid(&self, save_id: u32) -> bool;

    /// Number of active save levels.
    pub fn depth(&self) -> usize;

    /// Restore to a specific save_id. Returns the frames to unwind
    /// (newest first) so the caller can swap entity table offsets.
    /// Invalidates all levels newer than the target.
    pub fn restore(&mut self, save_id: u32) -> Result<Vec<SaveFrame>, PsError>;
}
```

**Restore algorithm**:
1. Find the `SaveFrame` matching `save_id`. If not found or invalid → `InvalidRestore`.
2. Collect all frames at the target level and above (newest first).
3. Mark them all as `valid = false`.
4. Pop them off the save stack.
5. Return them so the caller can process each frame's `SaveRecord`s.

**Tests (~10)**: save/restore lifecycle, nested saves, invalidation of newer saves, restore of intermediate level, error on invalid save_id.

---

## Step 6: Wire Save/Restore + COW into Context

**Modify**: `crates/xforge-core/src/context.rs`

Add fields:
```rust
pub struct Context {
    // ... existing fields ...
    pub save_stack: SaveStack,
    pub vm_alloc_mode: bool,        // false=local (default), true=global
    pub current_operator: Option<NameId>,  // For error reporting
    pub in_error_handler: bool,     // Guard against infinite error loops
}
```

Add COW helper methods:
```rust
impl Context {
    /// Create a save snapshot, return a Save object to push on o_stack.
    pub fn vm_save(&mut self) -> PsObject;

    /// Restore to a save_id, swapping entity offsets.
    pub fn vm_restore(&mut self, save_id: u32) -> Result<(), PsError>;

    /// COW check before mutating a string entity.
    /// If the entity's save_level < current level, copies data and records SaveRecord.
    pub fn cow_check_string(&mut self, entity: EntityId, len: u32);

    /// COW check before mutating an array entity.
    pub fn cow_check_array(&mut self, entity: EntityId, start: u32, len: u32);

    /// COW check before mutating a dict entity.
    pub fn cow_check_dict(&mut self, entity: EntityId);
}
```

**COW check implementation** (string example):
```rust
pub fn cow_check_string(&mut self, entity: EntityId, len: u32) {
    let meta = self.strings.entities.get(entity);
    if meta.is_global() { return; }
    let current_level = self.save_stack.current_level();
    if current_level == 0 || meta.save_level >= current_level { return; }

    // Copy data to new region
    let old_offset = meta.offset;
    let byte_len = len as usize;
    let temp: Vec<u8> = self.strings.data[old_offset as usize..old_offset as usize + byte_len].to_vec();
    let new_offset = self.strings.data.len() as u32;
    self.strings.data.extend_from_slice(&temp);

    // New entity points to the ORIGINAL data (for restore)
    let copy_entity = self.strings.entities.allocate(old_offset, len, meta.save_level, false);

    // Source entity now points to the new mutable copy
    let meta = self.strings.entities.get_mut(entity);
    meta.offset = new_offset;
    meta.save_level = current_level;

    // Record for restore
    self.save_stack.add_record(SaveRecord {
        src: entity, copy: copy_entity, store_type: StoreType::String,
    });
}
```

**Restore implementation**:
```rust
pub fn vm_restore(&mut self, save_id: u32) -> Result<(), PsError> {
    let frames = self.save_stack.restore(save_id)?;
    // Process frames newest-first, swapping entity offsets
    for frame in &frames {
        for record in frame.records.iter().rev() {
            match record.store_type {
                StoreType::String => {
                    let src_off = self.strings.entities.get(record.src).offset;
                    let cpy_off = self.strings.entities.get(record.copy).offset;
                    self.strings.entities.get_mut(record.src).offset = cpy_off;
                    self.strings.entities.get_mut(record.copy).offset = src_off;
                }
                StoreType::Array => { /* same pattern with self.arrays.entities */ }
                StoreType::Dict => { /* same pattern with self.dicts.entities */ }
            }
        }
    }
    // Restore d_stack depth and vm_alloc_mode from the target frame
    let target_frame = frames.last().unwrap();
    self.d_stack.truncate(target_frame.d_stack_depth);
    self.vm_alloc_mode = target_frame.vm_alloc_mode;
    // Close files opened after save level (Phase 2 Step 9)
    Ok(())
}
```

**Tests (~6)**: COW triggers on mutation after save, COW skips global entities, COW skips when no active save, restore swaps offsets correctly.

---

## Step 7: Update Mutation Paths for COW

**Modify**: Operator files that mutate composite objects must call COW checks.

### 7.1 String mutations

In `composite_ops.rs`:
- `op_put` (string case): call `ctx.cow_check_string(entity, len)` before `ctx.strings.put_byte(...)`
- `op_putinterval` (string case): call `cow_check_string` before writing

### 7.2 Array mutations

In `composite_ops.rs`:
- `op_put` (array case): call `ctx.cow_check_array(entity, start, len)` before `ctx.arrays.set_element(...)`
- `op_putinterval` (array case): call `cow_check_array` before writing

In `array_ops.rs`:
- `op_astore`: call `cow_check_array` before filling array

### 7.3 Dict mutations

In `dict_ops.rs`:
- `op_def`: call `ctx.cow_check_dict(current_dict_entity)` before `ctx.dicts.put(...)`
- `op_store`: call `cow_check_dict` on target dict before put
- `op_undef`: call `cow_check_dict` before remove

In `composite_ops.rs`:
- `op_put` (dict case): call `cow_check_dict` before put

**Tests (~6)**: Verify that mutating a string/array/dict after save triggers COW and restore reverts correctly.

---

## Step 8: VM Operators

**New file**: `crates/xforge-ops/src/vm_ops.rs`

### 8.1 `save` — `— save`

```rust
pub fn op_save(ctx: &mut Context) -> Result<(), PsError> {
    let save_obj = ctx.vm_save();
    ctx.o_stack.push(save_obj)?;
    Ok(())
}
```

`ctx.vm_save()` increments save level, records d_stack depth + vm_alloc_mode, returns `PsObject { value: PsValue::Save(SaveLevel(save_id)), flags: literal }`.

### 8.2 `restore` — `save —`

```rust
pub fn op_restore(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() { return Err(PsError::StackUnderflow); }
    let obj = ctx.o_stack.peek(0)?;
    let save_id = match obj.value {
        PsValue::Save(SaveLevel(id)) => id,
        _ => return Err(PsError::TypeCheck),
    };
    if !ctx.save_stack.is_valid(save_id) {
        return Err(PsError::InvalidRestore);
    }

    // INVALIDRESTORE check: scan stacks for local composites newer than save
    check_invalidrestore(ctx, save_id)?;

    ctx.o_stack.pop()?;
    ctx.vm_restore(save_id)?;
    Ok(())
}
```

**INVALIDRESTORE check** (per PLRM and PostForge):
Scan o_stack, e_stack, and d_stack for local composite objects whose entity `save_level > target_save_level`. If any found → `InvalidRestore`. Skip the save object itself on o_stack. Skip systemdict/globaldict/userdict on d_stack (they're permanent).

### 8.3 `vmstatus` — `— level used maximum`

```rust
pub fn op_vmstatus(ctx: &mut Context) -> Result<(), PsError> {
    let level = ctx.save_stack.depth() as i32;
    let used = (ctx.strings.data_len() + ctx.arrays.data_len() * 16) as i32;
    let maximum = i32::MAX; // No hard limit in Phase 2
    ctx.o_stack.push(PsObject::int(level))?;
    ctx.o_stack.push(PsObject::int(used))?;
    ctx.o_stack.push(PsObject::int(maximum))?;
    Ok(())
}
```

### 8.4 `setglobal` — `bool —`

```rust
pub fn op_setglobal(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() { return Err(PsError::StackUnderflow); }
    let obj = ctx.o_stack.peek(0)?;
    let mode = match obj.value {
        PsValue::Bool(v) => v,
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop()?;
    ctx.vm_alloc_mode = mode;
    Ok(())
}
```

### 8.5 `currentglobal` — `— bool`

```rust
pub fn op_currentglobal(ctx: &mut Context) -> Result<(), PsError> {
    ctx.o_stack.push(PsObject::bool(ctx.vm_alloc_mode))?;
    Ok(())
}
```

### 8.6 `gcheck` — `any — bool`

```rust
pub fn op_gcheck(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.is_empty() { return Err(PsError::StackUnderflow); }
    let obj = ctx.o_stack.pop()?;
    let is_global = if obj.is_composite() {
        obj.flags.is_global()
    } else {
        true  // Simple objects are always considered "global"
    };
    ctx.o_stack.push(PsObject::bool(is_global))?;
    Ok(())
}
```

### 8.7 `vmreclaim` — `int —`

No-op for Phase 2 (GC deferred). Pop the int and return Ok.

**Tests (~12)**: save/restore round-trip, nested save/restore, invalidrestore detection, setglobal/currentglobal toggle, gcheck for simple vs local vs global composites, vmstatus returns three ints.

---

## Step 9: FileStore Module

**New file**: `crates/xforge-core/src/file_store.rs`

```rust
use std::fs::File;
use std::io::{self, Read, Write, Seek, SeekFrom, BufReader};
use crate::error::PsError;
use crate::object::EntityId;

/// The underlying file handle, which may be a real file or stdio.
pub enum FileHandle {
    /// A regular disk file (readable/writable depending on mode).
    Real {
        reader: Option<BufReader<File>>,
        writer: Option<File>,
    },
    /// Standard input (not closeable by user code).
    Stdin(io::Stdin),
    /// Standard output (not closeable by user code).
    Stdout,
    /// Standard error (not closeable by user code).
    Stderr,
    /// File has been closed.
    Closed,
}

/// Metadata and handle for one open file.
pub struct FileEntry {
    pub handle: FileHandle,
    pub name: String,           // Filename or %stdin/%stdout/%stderr
    pub is_readable: bool,
    pub is_writable: bool,
    pub is_standard: bool,      // Standard files can't be closed
    pub save_level: u16,        // For close-on-restore
    pub is_global: bool,
}

/// Storage for all PostScript file objects.
pub struct FileStore {
    files: Vec<FileEntry>,
}

impl FileStore {
    pub fn new() -> Self;

    /// Pre-register stdin, stdout, stderr. Returns their EntityIds (0, 1, 2).
    pub fn register_standard_files(&mut self) -> (EntityId, EntityId, EntityId);

    /// Open a disk file. Returns EntityId for the new file object.
    pub fn open(&mut self, name: &str, mode: &str, save_level: u16, is_global: bool) -> Result<EntityId, PsError>;

    /// Close a file. No-op on standard files or already-closed files.
    pub fn close(&mut self, entity: EntityId);

    // Read operations
    pub fn read_byte(&mut self, entity: EntityId) -> Result<Option<u8>, PsError>;
    pub fn read_into(&mut self, entity: EntityId, buf: &mut [u8]) -> Result<usize, PsError>;
    pub fn readline(&mut self, entity: EntityId, buf: &mut [u8]) -> Result<(usize, bool), PsError>;

    // Write operations
    pub fn write_byte(&mut self, entity: EntityId, byte: u8) -> Result<(), PsError>;
    pub fn write_from(&mut self, entity: EntityId, data: &[u8]) -> Result<(), PsError>;

    // Position
    pub fn position(&mut self, entity: EntityId) -> Result<u64, PsError>;
    pub fn set_position(&mut self, entity: EntityId, pos: u64) -> Result<(), PsError>;
    pub fn bytes_available(&mut self, entity: EntityId) -> Result<i32, PsError>;

    // Flush
    pub fn flush(&mut self, entity: EntityId) -> Result<(), PsError>;

    // Status queries
    pub fn is_open(&self, entity: EntityId) -> bool;
    pub fn get_entry(&self, entity: EntityId) -> &FileEntry;

    /// Close all files opened after a given save level (for restore).
    pub fn close_after_save_level(&mut self, level: u16);
}
```

**Special file handling**: `%stdin`, `%stdout`, `%stderr` are recognized by `open()` and redirect to the pre-registered standard file entries. Standard files have `is_standard = true` and `close()` is a no-op on them.

**File mode mapping** (PostScript access string → Rust):
- `(r)` → `File::open()` (read-only)
- `(w)` → `File::create()` (write-only, truncate)
- `(a)` → `OpenOptions::new().append(true)` (write-only, append)
- `(r+)` → `OpenOptions::new().read(true).write(true)` (read-write)

**Tests (~8)**: open/close lifecycle, read_byte/write_byte round-trip, standard file close is no-op, readline reads up to newline, close_after_save_level.

---

## Step 10: File I/O Operators

**Modify**: `crates/xforge-ops/src/file_ops.rs` — expand from 5 to 24 operators.

### New Operators (19)

| # | Operator | Stack Signature | Key Implementation Notes |
|---|----------|----------------|--------------------------|
| 1 | `file` | filename access — file | Resolve %stdin/%stdout/%stderr; map access string to mode; `ctx.files.open(...)` |
| 2 | `closefile` | file — | Flush before close for output files (PLRM); `ctx.files.close(entity)` |
| 3 | `read` | file — byte true \| false | Read one byte; push `int true` or `false` on EOF; auto-close at EOF |
| 4 | `write` | file byte — | Write one byte (0-255); RANGECHECK if outside range |
| 5 | `readstring` | file string — substring bool | Read up to `len` bytes into string; return actual substring + success flag |
| 6 | `writestring` | file string — | Write all string bytes to file |
| 7 | `readline` | file string — substring bool | Read until CR/LF/FF terminator; discard terminator; RANGECHECK if string fills before newline |
| 8 | `readhexstring` | file string — substring bool | Read hex digit pairs; ignore whitespace; odd trailing nibble padded with 0 |
| 9 | `writehexstring` | file string — | Write each byte as two hex digits (uppercase) |
| 10 | `token` | file — obj true \| false | Read one PostScript token from file via tokenizer; critical for advanced `run` |
| 11 | `bytesavailable` | file — int | Return bytes available; -1 if unknown |
| 12 | `flushfile` | file — | Output file: flush buffers; input file: read and discard to EOF |
| 13 | `currentfile` | — file | Walk e_stack backward to find topmost executable File; return invalid file if none |
| 14 | `fileposition` | file — int | Current byte position; IOERROR if non-seekable |
| 15 | `setfileposition` | file int — | Seek to position; IOERROR if non-seekable |
| 16 | `status` | filename — bool | Check if file exists on disk |
| 17 | `deletefile` | filename — | Delete file; UNDEFINEDFILENAME if not found |
| 18 | `renamefile` | old new — | Rename file |
| 19 | `filenameforall` | template proc scratch — | Glob pattern matching; call proc for each match |

### Existing Operators (5, enhanced)

- `print`: No changes needed (already writes string to stdout).
- `=`, `==`: No changes needed.
- `flush`: No changes needed (already flushes stdout).
- `pstack`: No changes needed.

### `token` Implementation Notes

The `token` operator reads from a file (or string) and returns one tokenized PostScript object. For file-based `token`:
1. Read bytes from the file into a buffer.
2. Feed buffer to `Tokenizer::new()`.
3. Call `next_token()` to get one token.
4. Convert token to PsObject via `token_to_object()`.
5. If `ProcBegin`, call `parse_procedure()` to read the full `{ ... }` body.
6. Push result + `true` (or just `false` if EOF).

Phase 2 approach: read remaining file content into a buffer, tokenize one token, then use `setfileposition` to rewind the file past what was consumed. A streaming tokenizer can be optimized in Phase 8.

**Tests (~16)**: file open/close, read/write round-trip, readstring/writestring, readline, readhexstring/writehexstring, currentfile from e_stack, token reads one token, fileposition/setfileposition, status on existing/missing file, deletefile.

---

## Step 11: Enhanced `run` + File Execution in Eval Loop

### 11.1 Enhanced `run`

**Modify**: `crates/xforge-ops/src/misc_ops.rs`

Current `run` reads a file into a string and pushes it as an executable string. Enhanced version:
1. Open the file via `ctx.files.open(filename, "r", ...)`
2. Read entire file content into a buffer
3. Create a string entity from the buffer
4. Push as executable string on e_stack (same as before)
5. Optionally push the file object for `currentfile` support

### 11.2 File Execution Path in Eval Loop

**Modify**: `crates/xforge-engine/src/eval.rs`

Add a case for `PsValue::File(entity)` on the execution stack:

```rust
PsValue::File(entity) => {
    // Read one token from the file
    if !ctx.files.is_open(entity) {
        continue; // File already closed
    }
    match read_token_from_file(ctx, entity)? {
        Some(obj) => {
            // Push file back on e_stack (so currentfile can find it)
            let file_obj = PsObject { value: PsValue::File(entity), flags: obj_flags };
            ctx.e_stack.push(file_obj)?;
            // Process the token
            if matches!(obj.value, PsValue::Name(_)) && obj.flags.is_executable() {
                ctx.e_stack.push(obj)?;
            } else if matches!(obj.value, PsValue::Array { .. }) && obj.flags.is_executable() {
                ctx.e_stack.push(obj)?;
            } else {
                ctx.o_stack.push(obj)?;
            }
        }
        None => {
            // EOF — close file
            ctx.files.close(entity);
        }
    }
}
```

**Phase 2 simplification**: For file execution, buffer the entire file and use the existing string-based tokenizer approach. The File object on the e_stack serves as a marker for `currentfile`. A true streaming approach can come in Phase 8.

**Tests (~4)**: run loads and executes a file, currentfile finds file on e_stack.

---

## Step 12: Error Dispatch via errordict

**Modify**: `crates/xforge-engine/src/eval.rs`

Replace the `eprintln!` catch-all in the operator dispatch error handler:

```rust
Err(e) => {
    if ctx.in_error_handler {
        // Already handling an error — fallback to avoid infinite loop
        eprintln!("Error in error handler: {}", e);
        continue;
    }

    // Look up error name in errordict
    let error_name_bytes = e.plrm_name(); // e.g., b"typecheck"
    let error_name_id = ctx.names.intern(error_name_bytes);
    let key = DictKey::Name(error_name_id);

    if let Some(handler) = ctx.dicts.get(ctx.errordict, &key) {
        // Push offending command name onto o_stack
        if let Some(op_name_id) = ctx.current_operator {
            ctx.o_stack.push(PsObject::name_exec(op_name_id))?;
        }
        // Record error info in $error dict
        ctx.dicts.put(ctx.dollar_error, DictKey::Name(ctx.names.intern(b"newerror")), PsObject::bool(true));
        ctx.dicts.put(ctx.dollar_error, DictKey::Name(ctx.names.intern(b"errorname")), PsObject::name_lit(error_name_id));
        // ... ostack, estack, dstack snapshots ...

        ctx.in_error_handler = true;
        ctx.e_stack.push(handler)?;
    } else {
        eprintln!("Error: {}", e);
    }
}
```

**Add to `PsError`**: A `plrm_name(&self) -> &'static [u8]` method that returns the PLRM error name as bytes (e.g., `PsError::TypeCheck` → `b"typecheck"`).

**Default error handlers in errordict**: During `build_system_dict`, register a default handler procedure for each error type. Each handler calls `stop`, which propagates to the nearest `stopped` context. This makes errors catchable: `{ some_code } stopped { (error caught) } if`.

**Default `handleerror`**: A procedure registered in systemdict that reads `$error` and prints the error info to stderr. Called by the `stop` handler at the outermost level.

**Tracking `current_operator`**: Before dispatching each operator in the eval loop, store its `NameId` in `ctx.current_operator`. Reset to `None` after successful dispatch.

**Tests (~6)**: error caught by stopped, error name in $error, handleerror prints to stderr, nested error handling, in_error_handler guard prevents infinite loop.

---

## Step 13: Operator Registration

**Modify**: `crates/xforge-ops/src/lib.rs`

Add `pub mod vm_ops;` and register all new operators:

```rust
// VM operators
register(ctx, "save", vm_ops::op_save);
register(ctx, "restore", vm_ops::op_restore);
register(ctx, "vmstatus", vm_ops::op_vmstatus);
register(ctx, "setglobal", vm_ops::op_setglobal);
register(ctx, "currentglobal", vm_ops::op_currentglobal);
register(ctx, "gcheck", vm_ops::op_gcheck);
register(ctx, "vmreclaim", vm_ops::op_vmreclaim);

// File I/O operators (new)
register(ctx, "file", file_ops::op_file);
register(ctx, "closefile", file_ops::op_closefile);
register(ctx, "read", file_ops::op_read);
register(ctx, "write", file_ops::op_write);
register(ctx, "readstring", file_ops::op_readstring);
register(ctx, "writestring", file_ops::op_writestring);
register(ctx, "readline", file_ops::op_readline);
register(ctx, "readhexstring", file_ops::op_readhexstring);
register(ctx, "writehexstring", file_ops::op_writehexstring);
register(ctx, "token", file_ops::op_token);
register(ctx, "bytesavailable", file_ops::op_bytesavailable);
register(ctx, "flushfile", file_ops::op_flushfile);
register(ctx, "currentfile", file_ops::op_currentfile);
register(ctx, "fileposition", file_ops::op_fileposition);
register(ctx, "setfileposition", file_ops::op_setfileposition);
register(ctx, "status", file_ops::op_status);
register(ctx, "deletefile", file_ops::op_deletefile);
register(ctx, "renamefile", file_ops::op_renamefile);
register(ctx, "filenameforall", file_ops::op_filenameforall);

// Also register stdin, stdout, stderr file objects in systemdict
```

---

## Step 14: Integration Tests + Cleanup

### PostScript Integration Tests

Port and adapt relevant tests from PostForge:

**`tests/ps/save_restore_test.ps`** — from `vm_operators_tests.ps` and `save_invalidation_tests.ps`:
- Basic save/restore round-trip
- Variable restoration after restore
- Nested save/restore
- Save invalidation (restoring N invalidates N+1)
- Array/dict mutation reverted after restore

**`tests/ps/file_io_test.ps`** — from `file_tests.ps` and `file_operators_tests.ps`:
- Write string to file, read it back
- readline reads up to newline
- readhexstring/writehexstring round-trip
- currentfile returns correct file
- File operations on stdin/stdout don't crash

**`tests/ps/error_test.ps`** — error dispatch:
- `stopped` catches typecheck
- `$error /errorname` contains correct name
- Nested stopped handlers work

### Cleanup
- `cargo fmt`
- `cargo clippy` — fix all warnings
- Verify all done-when criteria below

---

## Operator Count Summary

| Category | Phase 1 | Phase 2 New | Phase 2 Total |
|----------|---------|-------------|---------------|
| Stack | 11 | 0 | 11 |
| Math | 24 | 0 | 24 |
| Relational | 11 | 0 | 11 |
| Type/Conv | 14 | 0 | 14 |
| Control | 11 | 0 | 11 |
| Dict | 14 | 0 | 14 |
| Array | 5 | 0 | 5 |
| String | 3 | 0 | 3 |
| Composite | 5 | 0 | 5 |
| File/Output | 5 | 19 | 24 |
| VM | 0 | 7 | 7 |
| Misc | 2 | 0 | 2 |
| **Total** | **~85** | **~26** | **~111** |

---

## Test Count Target

| Source | Phase 1 | Phase 2 New | Phase 2 Total |
|--------|---------|-------------|---------------|
| Existing Rust unit tests | 98 | 0 | 98 |
| EntityTable | — | ~8 | 8 |
| Store refactors (String+Array+Dict) | — | ~12 | 12 |
| SaveStack | — | ~10 | 10 |
| COW integration | — | ~12 | 12 |
| VM operators | — | ~12 | 12 |
| FileStore | — | ~8 | 8 |
| File operators | — | ~16 | 16 |
| Error dispatch | — | ~6 | 6 |
| PS integration tests | — | ~3 files | 3 files |
| **Total** | **98** | **~84+** | **~182+** |

---

## File Inventory

### New Files (5)

| File | Purpose |
|------|---------|
| `crates/xforge-core/src/entity_table.rs` | EntityTable: EntityId → (offset, len, save_level, is_global) |
| `crates/xforge-core/src/save_stack.rs` | SaveStack, SaveRecord, SaveFrame, restore logic |
| `crates/xforge-core/src/file_store.rs` | FileStore, FileEntry, FileHandle enum |
| `crates/xforge-ops/src/vm_ops.rs` | 7 VM operators (save, restore, vmstatus, etc.) |
| `tests/ps/*.ps` | PostScript integration tests |

### Modified Files (10)

| File | Changes |
|------|---------|
| `crates/xforge-core/src/lib.rs` | `pub mod entity_table; save_stack; file_store;` |
| `crates/xforge-core/src/string_store.rs` | Add EntityTable, route all access through it |
| `crates/xforge-core/src/array_store.rs` | Add EntityTable, same pattern |
| `crates/xforge-core/src/dict.rs` | Add EntityTable, same pattern |
| `crates/xforge-core/src/context.rs` | Add save_stack, file_store, vm_alloc_mode, COW methods, error state |
| `crates/xforge-core/src/error.rs` | Add `plrm_name()` method |
| `crates/xforge-ops/src/lib.rs` | Register ~26 new operators, add `pub mod vm_ops` |
| `crates/xforge-ops/src/file_ops.rs` | Expand from 5 to 24 operators |
| `crates/xforge-ops/src/composite_ops.rs` | Add COW checks before string/array/dict mutations |
| `crates/xforge-engine/src/eval.rs` | File execution path, error dispatch |

---

## Risk Mitigations

| Risk | Mitigation |
|------|------------|
| **Borrow checker + COW** | Use temp vec copies (`data[range].to_vec()`) before extending. Separate entity table from data vec to enable split borrows. |
| **Entity table refactor breaks tests** | Refactor one store at a time. Run full test suite after each. Public API barely changes. |
| **Error dispatch infinite loops** | `in_error_handler` flag falls back to `eprintln!` when already handling an error. |
| **File execution without streaming tokenizer** | Buffer entire file for Phase 2. Streaming optimization in Phase 8. |
| **Dict COW is O(n)** | Cloning HashMap is expensive for large dicts (systemdict ~400 entries). But it only happens once per save level per dict modification. Acceptable for Phase 2. |

---

## Success Criteria

Phase 2 is complete when:

1. `cargo build` succeeds with zero warnings
2. `cargo test` passes all tests (target: ~182)
3. `cargo clippy` passes with zero warnings
4. These PostScript programs execute correctly:
   - `save /s exch def /x 42 def s restore /x where { pop (FAIL\n) print } { (PASS\n) print } ifelse` — prints `PASS` (x undefined after restore)
   - `save /s exch def 3 array dup 0 99 put /a exch def s restore a 0 get =` — prints `0` (array mutation reverted)
   - Write-and-read file round-trip prints correct content
   - `{ 1 0 div } stopped { (caught\n) print } if` — prints `caught`
   - `true setglobal 3 array gcheck =` — prints `true`
   - `vmstatus pop pop =` — prints an integer (save level)
