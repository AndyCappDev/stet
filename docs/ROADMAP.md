# xforge — Rust PostScript Level 3 Interpreter Roadmap

## Context

PostForge is a complete PostScript Level 3 interpreter in Python (345 operators, 49 test suites, 5 output devices). It works well as a reference implementation but Python's inherent overhead limits production throughput. **xforge** is a ground-up Rust reimplementation targeting production-grade speed while using PostForge as the specification oracle and test source.

The architecture draws from xpost's proven C patterns (arena/entity indirection, dual VM, save/restore via entity swapping) translated into idiomatic Rust (enums for tagged objects, `Vec`-backed arenas, `Result` error propagation).

**Project**: xforge
**License**: AGPL-3.0-or-later
**Location**: ~/Projects/xforge (separate repo)
**Reference implementation**: PostForge (~/Projects/postforge)
**Architectural inspiration**: xpost (~/Projects/xpost)

---

## Reusable Assets from PostForge

These files transfer directly to xforge with zero or minimal modification:

| Asset | Path (in PostForge) | Notes |
|-------|---------------------|-------|
| Init scripts | `resources/Init/*.ps` (1,506 lines) | sysdict.ps, fontcategory.ps, resourcecategories.ps, fontmapping.ps |
| Type 1 fonts | `resources/Font/*.t1` (35 fonts) | Nimbus, URW, C059, P052 families |
| Encodings | `resources/Encoding/*.ps` | ISOLatin1, Standard, Symbol, etc. |
| CID fonts/CMaps | `resources/CIDFont/`, `resources/CMap/` | Multi-byte character support |
| Color resources | `resources/ColorSpace/`, `resources/ColorRendering/` | Color space definitions |
| ProcSets | `resources/ProcSet/*.ps` | Utility procedure collections |
| Test suites | `unit_tests/*.ps` (49 files) | Integration test oracle |
| Sample files | `samples/*.ps` | Visual regression testing |

---

## Phase 1: Foundation (Size: L) — COMPLETE

**Goal**: Minimal Rust project that can tokenize PostScript, represent all object types, and execute basic stack/math/control operations.

**Done when**: Can execute `3 4 add 7 eq { (YES) } { (NO) } ifelse =` and print `YES`.

**Status**: Complete (2026-02-25). 4-crate workspace, ~85 operators, 98 tests passing, zero clippy warnings. Target program prints `YES` correctly.

### 1.1 Project Scaffolding

```
xforge/
├── Cargo.toml              # Workspace root
├── LICENSE                  # AGPL-3.0
├── README.md
├── crates/
│   ├── xforge-core/        # Type system, VM, arena, tokenizer
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── object.rs       # PsObject enum, ObjectTag, AccessLevel
│   │   │   ├── arena.rs        # Arena allocator + entity table
│   │   │   ├── vm.rs           # Dual VM (global/local), save/restore
│   │   │   ├── stack.rs        # Stack operations (Vec-backed)
│   │   │   ├── dict.rs         # Dictionary (HashMap-backed)
│   │   │   ├── name.rs         # Name interning (HashMap<String, u32>)
│   │   │   ├── tokenizer.rs    # PostScript tokenizer
│   │   │   ├── context.rs      # Execution context
│   │   │   ├── error.rs        # PostScript error types
│   │   │   └── array.rs        # Array/PackedArray storage
│   │   └── Cargo.toml
│   ├── xforge-ops/         # All PostScript operators
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── stack_ops.rs    # pop, dup, exch, roll, index, etc.
│   │   │   ├── math_ops.rs     # add, sub, mul, div, mod, etc.
│   │   │   ├── dict_ops.rs     # def, begin, end, load, where, etc.
│   │   │   ├── control_ops.rs  # if, ifelse, for, repeat, loop, exec, etc.
│   │   │   ├── type_ops.rs     # type, cvx, cvlit, cvi, cvr, cvs, etc.
│   │   │   ├── relational_ops.rs # eq, ne, lt, gt, le, ge, and, or, not
│   │   │   ├── string_ops.rs
│   │   │   ├── array_ops.rs
│   │   │   ├── file_ops.rs
│   │   │   └── ... (one file per operator group)
│   │   └── Cargo.toml
│   ├── xforge-engine/      # Execution engine (exec_exec equivalent)
│   │   └── ...
│   └── xforge-cli/         # Binary entry point
│       └── ...
├── resources/              # Copied from PostForge (PS scripts, fonts, encodings)
└── tests/                  # Integration tests (PostForge test suites)
    └── ps/                 # *.ps test files
```

### 1.2 Object Representation

```rust
/// 16-byte PostScript object
#[derive(Clone, Copy)]
pub struct PsObject {
    pub value: PsValue,
    pub flags: ObjFlags,  // access (2 bits), executable (1 bit), global (1 bit)
}

#[derive(Clone, Copy)]
pub enum PsValue {
    Null,
    Mark,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(NameId),                        // interned name index
    String(EntityId, u32),               // arena entity + length
    Array(EntityId, u32),                // arena entity + length
    PackedArray(EntityId, u32),
    Dict(EntityId),                      // arena entity
    Operator(OpCode),                    // index into operator table
    File(EntityId),
    Save(SaveLevel),
}
```

**Design notes**:
- `PsObject` will be larger than xpost's 8 bytes (Rust enums carry discriminant + largest variant), likely 24 bytes. This is acceptable — modern CPUs handle 24-byte objects efficiently, and the ergonomic/safety gains of proper enums are worth it.
- `EntityId` and `NameId` are newtypes wrapping `u32` for type safety.
- `OpCode` is a newtype wrapping `u16` (supports 65K operators, far more than needed).

### 1.3 Arena + Entity Table

```rust
pub struct Arena {
    heap: Vec<u8>,                    // Raw byte storage
    entities: Vec<EntityEntry>,       // Entity metadata table
}

pub struct EntityEntry {
    offset: u32,          // Byte offset into heap
    size: u32,            // Allocated size
    used: u32,            // Used size
    save_level: u16,      // For save/restore COW
    gc_mark: u8,          // For garbage collection
    obj_type: u8,         // Type tag for GC traversal
}
```

### 1.4 Execution Engine

The core eval loop, equivalent to PostForge's `exec_exec`:

```rust
fn eval(ctx: &mut Context) -> Result<(), PsError> {
    loop {
        let obj = match ctx.e_stack.pop() {
            Some(obj) => obj,
            None => return Ok(()),  // exec stack empty = done
        };

        if obj.is_literal() {
            ctx.o_stack.push(obj);
            continue;
        }

        match obj.value {
            PsValue::Int(_) | PsValue::Real(_) | PsValue::Bool(_)
            | PsValue::Null | PsValue::Mark => {
                ctx.o_stack.push(obj);
            }
            PsValue::Name(id) => {
                // Look up in dict stack, push result
                let val = ctx.dict_stack_load(id)?;
                if val.is_executable() {
                    ctx.e_stack.push(val);
                } else {
                    ctx.o_stack.push(val);
                }
            }
            PsValue::Operator(opcode) => {
                ctx.dispatch_operator(opcode)?;
            }
            PsValue::Array(ent, len) => {
                // Executable array = procedure body
                // Push elements onto exec stack in reverse order
                self.exec_procedure(ctx, ent, len)?;
            }
            PsValue::String(ent, len) => {
                // Executable string = tokenize and execute
                self.exec_string(ctx, ent, len)?;
            }
            PsValue::File(ent) => {
                // Executable file = read token and execute
                self.exec_file(ctx, ent)?;
            }
            _ => {
                ctx.o_stack.push(obj);
            }
        }
    }
}
```

### 1.5 Phase 1 Operators (minimum viable set: ~96 operators)

| Group | Count | Operators |
|-------|-------|-----------|
| Stack | 11 | pop, dup, exch, copy, index, roll, clear, count, mark, cleartomark, counttomark |
| Dict | 17 | dict, begin, end, def, load, store, get, put, known, where, length, maxlength, currentdict, countdictstack, dictstack, undef, cleardictstack |
| Math | 24 | add, sub, mul, div, idiv, mod, abs, neg, ceiling, floor, round, truncate, sqrt, exp, ln, log, sin, cos, atan, rand, srand, rrand, max, min |
| Relational | 11 | eq, ne, gt, ge, lt, le, and, or, xor, not, bitshift |
| Type/Conv | 14 | type, cvx, cvlit, cvn, cvs, cvrs, cvi, cvr, xcheck, executeonly, noaccess, readonly, rcheck, wcheck |
| Control | 14 | exec, if, ifelse, for, repeat, loop, forall, exit, stop, stopped, countexecstack, execstack, quit, currentfile |
| Array | 5 | array, aload, astore, get, put (+ forall via control) |
| String | 5 | string, length, get, put, anchorsearch, search |
| File I/O | 12 | file, closefile, read, write, readstring, writestring, readline, token, bytesavailable, flush, flushfile, print |
| Misc | 8 | bind, null, version, languagelevel, =, ==, pstack, run |

### 1.6 Key Rust Dependencies (Phase 1)

```toml
# Cargo.toml (workspace)
[workspace.dependencies]
thiserror = "2"    # Error derive macros
```

No external rendering dependencies in Phase 1. Pure Rust.

### 1.7 Testing Strategy (Phase 1)

- Write Rust unit tests for each operator (stack behavior, error conditions)
- Port PostForge's `unittest.ps` framework and adapt relevant test files:
  - `arithmetic_tests.ps`, `stack_tests.ps`, `dict_tests.ps`
  - `control_flow_tests.ps`, `array_tests.ps`, `string_tests.ps`
- Integration test runner: execute `.ps` files and compare output to PostForge
- CI: GitHub Actions with `cargo test` + PS integration tests

---

## Phase 2: VM & Persistence (Size: M) — COMPLETE

**Goal**: Dual VM (global/local), save/restore, file I/O, error dispatch.

**Done when**: Can execute programs using `save`/`restore`, `setglobal`/`currentglobal`, file read/write, and error dispatch via `errordict`.

**Status**: Complete (2026-02-25). ~111 operators, 161 tests passing, zero clippy warnings. All done-when criteria verified by integration tests.

### Key Components (Implemented)
- **Entity table indirection**: `EntityTable` mapping `EntityId → (offset, len, save_level, is_global)` in each store, enabling save/restore via offset swapping
- **Copy-on-write save/restore**: `save` records level; first mutation after save copies entity data to new region and records `SaveRecord(src, copy)`; `restore` swaps offsets to revert mutations
- **Dual VM (lightweight)**: `is_global` flag per entity table entry, single unified stores, `vm_alloc_mode` flag on Context; global entities skip local save/restore COW
- **File I/O**: `FileStore` with pre-allocated stdin/stdout/stderr; 19 new operators — `file`, `closefile`, `read`, `write`, `readstring`, `writestring`, `readline`, `readhexstring`, `writehexstring`, `token`, `bytesavailable`, `flushfile`, `currentfile`, `fileposition`, `setfileposition`, `status`, `deletefile`, `renamefile`, `filenameforall`
- **VM operators**: 7 ops — `save`, `restore`, `vmstatus`, `setglobal`, `currentglobal`, `gcheck`, `vmreclaim`
- **Error dispatch**: Errors look up handler in `errordict`, populate `$error` dict, handler calls `stop` which propagates to enclosing `stopped` context; `in_error_handler` flag prevents infinite recursion; `handleerror` operator for default reporting
- **Garbage collection**: Deferred to future phase (entity table reserves flags for GC)

### New Files
- `crates/xforge-core/src/entity_table.rs` — EntityMeta + EntityTable
- `crates/xforge-core/src/save_stack.rs` — SaveRecord, SaveLevel, SaveStack
- `crates/xforge-core/src/file_store.rs` — FileHandle, FileEntry, FileStore
- `crates/xforge-ops/src/vm_ops.rs` — VM operators + VM-aware allocation helpers

### Operators Added: ~26 (total: ~111)
### Tests Added: 63 (total: 161)

---

## Phase 3: Graphics Foundation (Size: XL)

**Goal**: Complete graphics state, path construction, matrix operations, basic color, painting operators. First raster output (PNG).

**Done when**: Can render PostForge's `samples/tiger.ps` to PNG.

### Key Components
- **Graphics state**: 28 operators (gsave, grestore, setlinewidth, setlinecap, setlinejoin, setdash, setmiterlimit, etc.)
- **Path construction**: 12 operators (moveto, lineto, curveto, arc, arcn, arcto, closepath, newpath, currentpoint, rmoveto, rlineto, rcurveto)
- **Matrix ops**: 16 operators (matrix, currentmatrix, setmatrix, translate, scale, rotate, concat, transform, itransform, dtransform, idtransform, invertmatrix, identmatrix, defaultmatrix, initmatrix, concatmatrix)
- **Basic color**: DeviceGray, DeviceRGB, DeviceCMYK only (7 device color + 4 colorspace operators)
- **Painting**: 7 operators (fill, eofill, stroke, fillstroke, erasepage, showpage, copypage)
- **Clipping**: 7 operators (clip, eoclip, clippath, initclip, rectclip, pathbbox, clipsave/cliprestore)
- **Path query**: 6 operators (pathbbox, flattenpath, reversepath, strokepath, pathforall, currentpoint)
- **Display list**: Build during execution, consume for rendering
- **PNG device**: Cairo-based raster output via `cairo-rs` crate

### New Rust Dependencies
```toml
cairo-rs = "0.20"       # 2D rendering (paths, fills, strokes, text)
png = "0.17"             # PNG encoding (alternative to cairo PNG surface)
```

### Operators Added: ~83
### Test Suites: `graphics_state_tests.ps`, `path_tests.ps`, `matrix_tests.ps`, `color_tests.ps`, `clipping_tests.ps`

---

## Phase 4: Text & Fonts (Size: XL)

**Goal**: Complete font system — Type 1, Type 3, CID/TrueType. Text rendering to all devices.

**Done when**: Can render documents with mixed fonts, composite fonts, and produce searchable text output.

### Key Components
- **Font operators**: 7 ops (definefont, undefinefont, findfont, scalefont, makefont, setfont, currentfont)
- **Text show**: 13 ops (show, ashow, widthshow, awidthshow, kshow, xshow, yshow, xyshow, glyphshow, cshow, stringwidth, charpath, +internal)
- **Show variants**: 2 ops (stringwidth, charpath)
- **Charstring interpreter**: Type 1 font decryption + charstring execution (hint processing, subroutine calls)
- **Type 3 font execution**: PS procedure-based glyphs (setcachedevice, setcharwidth)
- **CID font support**: CIDFont dictionaries, CMap processing, TrueType sfnts parsing
- **Glyph caching**: Cache rendered glyphs for performance
- **Font resources**: Load from PostForge's `resources/Font/` directory

### New Rust Dependencies
```toml
freetype-rs = "0.37"    # Font rasterization (optional, for hinting)
```

### Operators Added: ~22
### Test Suites: `font_tests.ps`, `text_show_tests.ps`, `type3_font_tests.ps`, `cid_font_tests.ps`

---

## Phase 5: Filters, Images & Advanced Color (Size: XL)

**Goal**: Complete filter framework, image operators, full color space system including CIE and ICC.

**Done when**: Can process PostScript documents with embedded JPEG images, compressed streams, ICC color profiles, and all color spaces.

### Key Components
- **Filter framework**: 1 operator entry point (`filter`) + 14 filter implementations
  - ASCII: ASCII85Decode/Encode, ASCIIHexDecode/Encode, NullEncode
  - Compression: FlateDecode/Encode, LZWDecode/Encode, RunLengthDecode/Encode
  - Fax: CCITTFaxDecode
  - Image: DCTDecode/Encode (JPEG)
- **Image operators**: 3 ops (image, imagemask, colorimage) + Type 3 masked images
- **Advanced color spaces**: CIEBasedA/ABC/DEF/DEFG, ICCBased, Separation, DeviceN, Indexed, Pattern
- **Halftone/transfer**: 9 operators (sethalftone, currenthalftone, setscreen, currentscreen, settransfer, currenttransfer, etc.)
- **Pattern/form**: 3 operators (makepattern, setpattern, execform)
- **Shading**: Types 1-7 (function-based, axial, radial, free-form/lattice/Coons/tensor-product mesh)

### New Rust Dependencies
```toml
flate2 = "1"             # Flate/zlib compression
jpeg-decoder = "0.3"     # JPEG decoding
jpeg-encoder = "0.6"     # JPEG encoding
weezl = "0.1"            # LZW compression
lcms2 = "6"              # ICC color management
```

### Operators Added: ~16 (but massive infrastructure underneath)
### Test Suites: `filter_tests.ps`, `image_tests.ps`, `dct_tests.ps`, `color_space_tests.ps`, `shading_tests.ps`, `pattern_tests.ps`

---

## Phase 6: Resource System & Init Scripts (Size: M)

**Goal**: Named resource system, PS init scripts running, interpreter fully self-hosting.

**Done when**: `sysdict.ps` and all init scripts execute successfully. `findresource` loads resources from disk. All 345 operators registered. Interactive prompt works.

### Key Components
- **Resource operators**: 12 ops (findresource, defineresource, undefineresource, resourcestatus, resourceforall, etc.)
- **Resource categories**: 11 standard categories (Font, Encoding, Form, Pattern, ProcSet, ColorSpace, CMap, CIDFont, FontSet, OutputDevice, IdiomSet)
- **Filesystem loading**: On-demand resource loading from `resources/` directory
- **Init script execution**: Run sysdict.ps → fontcategory.ps → resourcecategories.ps → fontmapping.ps at startup
- **Remaining operators**: Device output (6), interpreter params (14), job control (3), userpath (10), packed arrays (3), insideness (3), strokepath (1), misc stragglers
- **Interactive REPL**: Command-line prompt with readline-style editing
- **CLI**: Argument parsing, device selection, file input, stdin piping

### New Rust Dependencies
```toml
rustyline = "14"         # Interactive line editing
clap = "4"               # CLI argument parsing
```

### Operators Added: ~52 (remaining operators to reach 345)
### Test Suites: Run ALL 49 PostForge test suites — full regression

---

## Phase 7: Output Devices (Size: XL)

**Goal**: All production output formats — PDF (native), SVG, TIFF, interactive display.

**Done when**: Can produce identical output to PostForge for all sample files across all devices.

### Key Components
- **PDF device** (largest effort — 8,500+ lines in PostForge):
  - Native PDF generation (no Cairo dependency for PDF)
  - Content stream generation (paths, text, images, shadings)
  - Type 1 font reconstruction + subsetting
  - CID/TrueType font embedding
  - CFF font embedding
  - ToUnicode CMap generation (searchable text)
  - CMYK/Gray color space preservation
  - Image XObject construction
- **SVG device**: Cairo-based vector with selectable text, `--text-as-paths` option
- **TIFF device**: Cairo + image encoding, multi-page, CMYK support
- **Interactive display**: GUI window with zoom/pan (egui, winit, or similar Rust GUI)

### New Rust Dependencies
```toml
# PDF generation (consider building native, or:)
lopdf = "0.34"           # Low-level PDF manipulation
image = "0.25"           # Image encoding (TIFF, etc.)
tiff = "0.9"             # TIFF encoding
resvg = "0.44"           # SVG (alternative to cairo SVG surface)
# OR continue using cairo-rs for SVG/TIFF and build PDF natively
```

### Test Strategy
- Visual regression: Render all `samples/*.ps` files, compare pixel-by-pixel against PostForge output
- PDF validation: Verify structure with external PDF tools
- Text searchability: Verify ToUnicode CMap in PDF output

---

## Phase 8: Optimization & Production Hardening (Size: L)

**Goal**: Production-grade performance, robustness, and packaging.

**Done when**: Benchmarks show significant speedup over PostForge. All PostForge test suites pass. Handles malformed input gracefully.

### Key Components
- **Performance profiling**: Identify and optimize hot paths
  - Inline name lookup caching (similar to xpost's opcode shortcuts)
  - Dict lookup acceleration (specialized small-dict fast path)
  - Stack operation optimization
  - Arena allocation tuning
- **Robustness**: Fuzz testing for tokenizer/filters, resource limits, timeout handling
- **Packaging**: Release binaries for Linux/macOS/Windows, `cargo install` support
- **Documentation**: User guide, man page, API docs
- **Benchmarks**: Comparative benchmarks against PostForge and GhostScript
- **CI/CD**: GitHub Actions matrix (Linux, macOS, Windows), automated test runs

### New Rust Dependencies
```toml
criterion = "0.5"       # Benchmarking
arbitrary = "1"          # Fuzz testing
```

---

## Cross-Cutting Concerns

### Error Handling Strategy
```rust
#[derive(Debug, thiserror::Error)]
pub enum PsError {
    #[error("stackunderflow")]
    StackUnderflow,
    #[error("typecheck")]
    TypeCheck,
    #[error("rangecheck")]
    RangeCheck,
    // ... all 18+ PLRM error types
    #[error("internal: {0}")]
    Internal(String),
}
```

All operator functions return `Result<(), PsError>`. The eval loop catches errors and dispatches to the PostScript `$error` handler, matching PLRM semantics.

### Operator Implementation Pattern
```rust
/// add: num1 num2 → sum
fn op_add(ctx: &mut Context) -> Result<(), PsError> {
    if ctx.o_stack.len() < 2 {
        return Err(PsError::StackUnderflow);
    }
    let b = ctx.o_stack.peek(0)?;
    let a = ctx.o_stack.peek(1)?;
    let result = match (a.value, b.value) {
        (PsValue::Int(a), PsValue::Int(b)) => match a.checked_add(b) {
            Some(sum) => PsValue::Int(sum),
            None => PsValue::Real(a as f64 + b as f64),
        },
        (PsValue::Real(a), PsValue::Real(b)) => PsValue::Real(a + b),
        (PsValue::Int(a), PsValue::Real(b)) => PsValue::Real(a as f64 + b),
        (PsValue::Real(a), PsValue::Int(b)) => PsValue::Real(a + b as f64),
        _ => return Err(PsError::TypeCheck),
    };
    ctx.o_stack.pop();
    ctx.o_stack.pop();
    ctx.o_stack.push(PsObject::literal(result));
    Ok(())
}
```

Note: Following PostForge's pattern — validate ALL operands BEFORE popping.

### Git Configuration
- Remotes: GitHub (primary for xforge)
- CI: GitHub Actions (Linux, macOS, Windows matrix)
- Branch strategy: `main` + feature branches

---

## Summary: Phase Dependency Graph

```
Phase 1: Foundation
    ↓
Phase 2: VM & Persistence
    ↓
Phase 3: Graphics Foundation ← first visual output (PNG)
    ↓
Phase 4: Text & Fonts
    ↓
Phase 5: Filters, Images & Advanced Color
    ↓
Phase 6: Resource System & Init Scripts ← interpreter self-hosts
    ↓
Phase 7: Output Devices (PDF, SVG, TIFF, GUI)
    ↓
Phase 8: Optimization & Production Hardening
```

Phases 3-5 have some parallelism potential (filters are independent of fonts), but the dependency chain is generally linear because later phases build on earlier infrastructure.
