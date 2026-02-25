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
│   ├── xforge-engine/      # Execution engine (eval loop)
│   │   └── ...
│   ├── xforge-render/      # Rendering backend (tiny-skia) [Phase 3]
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

## Phase 3: Graphics Foundation (Size: XL) — COMPLETE

**Goal**: Complete graphics state, path construction, matrix operations, basic color, painting operators. First raster output (PNG).

**Done when**: Can render PostForge's `samples/tiger.ps` to PNG.

**Status**: Complete (2026-02-25). 5-crate workspace (new: xforge-render), ~189 operators, 270 tests passing, zero clippy warnings. tiger.ps renders correctly at any DPI. Release build renders 300 DPI tiger (2550×3300) in ~110ms.

### Key Components (Implemented)
- **Graphics state**: `GraphicsState` struct on Context with `gstate_stack: Vec<GraphicsState>` for gsave/grestore. 18 operators — gsave, grestore, grestoreall, setlinewidth, currentlinewidth, setlinecap, currentlinecap, setlinejoin, currentlinejoin, setmiterlimit, currentmiterlimit, setdash, currentdash, setflat, currentflat, setstrokeadjust, currentstrokeadjust, initgraphics
- **Path construction**: Paths stored in user space as `Vec<PathSegment>` (MoveTo, LineTo, CurveTo, ClosePath). Arc-to-bezier conversion ported from PostForge (≤90° segments). 13 operators — newpath, currentpoint, moveto, rmoveto, lineto, rlineto, curveto, rcurveto, closepath, arc, arcn, arcto, arct
- **Matrix ops**: `Matrix` struct (6 f64s, Clone+Copy). PostScript row-vector convention: `concat` does `self.multiply(other)` in column-vector convention = `M × CTM` in row-vector. 16 operators — matrix, identmatrix, currentmatrix, setmatrix, defaultmatrix, initmatrix, translate, scale, rotate, concat, concatmatrix, invertmatrix, transform, itransform, dtransform, idtransform
- **Color**: `DeviceColor { r, g, b: f64 }` with conversions from gray, RGB, CMYK, HSB. CMYK→RGB: `r = 1-(c+k)` clamped. All colors convert to RGB internally. 12 operators — setgray, currentgray, setrgbcolor, currentrgbcolor, setcmykcolor, currentcmykcolor, sethsbcolor, currenthsbcolor, setcolorspace, currentcolorspace, setcolor, currentcolor
- **Painting**: Immediate-mode rendering (no display list). fill/stroke call device methods directly with CTM passed at paint time. 7 operators — fill, eofill, stroke, rectfill, rectstroke, erasepage, showpage
- **Clipping**: Device clip via tiny-skia Mask. Clip intersection via `Mask::intersect_path`. Path NOT cleared after clip (unlike fill/stroke). 7 operators — clip, eoclip, clippath, initclip, rectclip, clipsave, cliprestore
- **Path query**: Conservative control-point hull for pathbbox. De Casteljau recursive subdivision for flattenpath. 5 operators — pathbbox, flattenpath, reversepath, strokepath, pathforall
- **Renderer**: `RasterDevice` trait in xforge-core, `SkiaDevice` implementation in new xforge-render crate using tiny-skia 0.11. All internal math f64, convert to f32 only at tiny-skia boundary
- **CLI**: `--dpi` flag for rendering resolution (default 72). Output PNG derived from input filename

### Architecture Decisions
- **Immediate-mode rendering** (no display list): fill/stroke invoke device directly. Simpler for Phase 3; display list can be layered in Phase 7
- **Paths in user space**: Unlike PostForge (device space at construction), xforge stores paths in user space. CTM captured at paint time and passed to tiny-skia as Transform. This gives correct currentpoint/pathbbox without inverse CTM and handles anisotropic strokes naturally
- **Trait-based device**: `RasterDevice` trait enables future backend swaps without changing operator code
- **Deferred flag**: ObjFlags bit 6 marks nested executable arrays in procedure bodies so the eval loop pushes them to o_stack (preserving executable flag for `if`/`ifelse`) rather than executing them

### New Crate
- **xforge-render** — tiny-skia 0.11 device implementation (`SkiaDevice`)

### New Files (12)
- `crates/xforge-core/src/graphics_state.rs` — Matrix, PsPath, GraphicsState, DeviceColor
- `crates/xforge-core/src/device.rs` — RasterDevice trait, FillParams, StrokeParams, ClipParams
- `crates/xforge-ops/src/matrix_ops.rs` — 16 operators
- `crates/xforge-ops/src/path_ops.rs` — 13 operators
- `crates/xforge-ops/src/color_ops.rs` — 12 operators
- `crates/xforge-ops/src/graphics_state_ops.rs` — 18 operators
- `crates/xforge-ops/src/paint_ops.rs` — 7 operators
- `crates/xforge-ops/src/clip_ops.rs` — 7 operators
- `crates/xforge-ops/src/path_query_ops.rs` — 5 operators
- `crates/xforge-render/src/lib.rs` + `src/skia_device.rs` — tiny-skia device
- `crates/xforge-engine/tests/rendering.rs` — 10 integration tests

### New Dependencies
```toml
tiny-skia = "0.11"      # 2D rasterization (paths, fills, strokes, clipping)
```

### Operators Added: ~78 (total: ~189)
### Tests Added: 109 (total: 270)

---

## Phase 4: Text & Fonts (Size: XL) — COMPLETE

**Goal**: Type 1 font loading, charstring interpretation, font dictionary operators, text show operators.

**Done when**: `cargo run -- test1.ps` produces a valid PNG with visible text labels. `golfer.ps` continues to render correctly.

**Status**: Complete (2026-02-25). ~211 operators, 310 tests passing, zero clippy warnings. test1.ps renders all four text labels (Helvetica 40pt) with correct transforms. tiger.ps and golfer.ps continue to render correctly.

### Key Components (Implemented)
- **Rust-native .t1 parser**: Parses ASCII header (FontName, FontMatrix, FontBBox, Encoding) then decrypts binary eexec section (R=55665, C1=52845, C2=22719) to extract CharStrings and Subrs. Handles both hex and binary eexec encoding. No eexec operator needed — faster and simpler than executing .t1 as PostScript
- **CharString interpreter**: Decrypts individual charstrings (R=4330, skip lenIV bytes), executes binary opcodes → PsPath segments + glyph width. Handles all standard commands: hsbw, sbw, rmoveto, hmoveto, vmoveto, rlineto, hlineto, vlineto, rrcurveto, vhcurveto, hvcurveto, closepath, callsubr/return, endchar. Escape opcodes: div, seac (stub), callothersubr/pop (flex support via OtherSubrs 0-3). Ignores hints (hstem, vstem, hstem3, vstem3, dotsection) for this phase
- **Font loading pipeline**: 35-entry font name substitution table (Adobe standard → URW equivalents, e.g. Helvetica → NimbusSans-Regular). Lazy loading on first `findfont` access. Fonts are regular PostScript dicts containing /FontName, /FontType, /FontMatrix, /FontBBox, /Encoding, /CharStrings, /Private, /Subrs, /FID. FontDirectory dict in systemdict maps font names → font dicts
- **Font dictionary operators**: 8 ops — definefont, undefinefont, findfont, scalefont, makefont, setfont, currentfont, selectfont. scalefont/makefont copy font dict and compose matrix with FontMatrix, removing /FID per PLRM
- **Text show operators**: 9 ops — show, ashow, widthshow, awidthshow, kshow, stringwidth, charpath, setcachedevice (stub), setcharwidth (stub). Show pipeline: encoding lookup → charstring execution → FontMatrix transform → fill with current color
- **Page size convenience ops**: 5 no-ops — letter, legal, a4, a3, b5 (page size set by CLI)
- **Encoding tables**: StandardEncoding and ISOLatin1Encoding (256 entries each) as static Rust arrays
- **Font resources**: 35 Type 1 font files (.t1) copied from PostForge to `resources/Font/`
- **CLI font path**: Searches for `resources/Font/` relative to executable and CWD

### Architecture Decisions
- **Fonts as PostScript dicts**: No separate FontStore — fonts are regular dicts in DictStore, reusing all existing dict infrastructure. Matches PLRM semantics exactly
- **Rust-native parser, not eexec execution**: Parsing .t1 files directly in Rust is faster, simpler, and more predictable than implementing the eexec operator + RD/ND byte-reading operators. eexec can be added in Phase 6 for init scripts
- **CharString interpreter in Rust**: Type 1 charstrings are a binary opcode language (not PostScript), so a native Rust interpreter is the only option
- **current_font on GraphicsState**: `Option<PsObject>` field, cloned on gsave/grestore like all other gstate fields

### New Files (6 source + 35 resources)
- `crates/xforge-core/src/encoding.rs` — StandardEncoding, ISOLatin1Encoding tables
- `crates/xforge-core/src/type1_parser.rs` — .t1 file parser with eexec decryption
- `crates/xforge-core/src/charstring.rs` — Type 1 charstring interpreter
- `crates/xforge-core/src/font_loader.rs` — Font loading pipeline, name substitution table
- `crates/xforge-ops/src/font_ops.rs` — 8 font dictionary operators + 5 page size no-ops
- `crates/xforge-ops/src/show_ops.rs` — 9 text show operators
- `resources/Font/*.t1` — 35 Type 1 font files

### Operators Added: 22 (total: ~211)
### Tests Added: 40 (total: 310)

### Deferred to Later Phases
- Type 3 fonts (BuildGlyph/BuildChar execution) — Phase 6 with init scripts
- CID/TrueType fonts — Phase 6+
- Glyph caching — performance optimization phase
- eexec operator (execute encrypted PS) — Phase 6 with init scripts
- Font resources via findresource — Phase 6 resource system
- Hint processing — future quality improvement
- seac accent composition — currently stubbed

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
- **SVG device**: Vector output with selectable text, `--text-as-paths` option
- **TIFF device**: Raster + image encoding, multi-page, CMYK support
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
