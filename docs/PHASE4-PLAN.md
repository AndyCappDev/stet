# stet Phase 4: Text & Fonts — Implementation Plan

## Context

Phase 3 is complete: 5-crate workspace, ~189 operators, 270 tests, zero clippy warnings. The interpreter renders PostScript graphics (paths, transforms, colors, clipping) to PNG via tiny-skia. `tiger.ps` renders correctly at any DPI.

Phase 4 adds text rendering — Type 1 font loading, charstring interpretation, font dictionary operators, and text show operators. The primary complexity is the Type 1 charstring interpreter (a binary opcode language separate from PostScript) and the font loading pipeline.

**Done when**: `cargo run -- test1.ps` produces a valid PNG with visible text labels. `golfer.ps` continues to render correctly.

---

## Architecture: Key Decisions

### 1. Fonts as PostScript Dictionaries

Font dictionaries are regular PostScript dicts (using existing `DictStore`). No separate `FontStore` — fonts are dicts containing `/FontName`, `/FontType`, `/FontMatrix`, `/FontBBox`, `/Encoding`, `/CharStrings`, `/Private`, etc. This reuses all existing dict infrastructure and matches PLRM semantics exactly.

`FontDirectory` is a dict in systemdict mapping font names to font dicts.

### 2. Rust-Native .t1 Parser (Not eexec Execution)

Type 1 `.t1` files ARE PostScript programs, but executing them requires `eexec` operator support + special byte-reading operators (`RD`/`-|`/`ND`/`|-`). For Phase 4, we parse `.t1` files directly in Rust:

1. Parse ASCII header → extract FontName, FontMatrix, FontBBox, Encoding, PaintType, FontType
2. Find `currentfile eexec` marker
3. Decrypt binary portion using eexec algorithm (R=55665, C1=52845, C2=22719)
4. In decrypted text: extract Private dict fields and CharStrings dict
5. Build PostScript dict objects from parsed data

This is faster, simpler, and more predictable than full eexec execution. The `eexec` operator can be added in Phase 6 when init scripts need it.

### 3. CharString Interpreter in Rust

Type 1 charstrings are a binary opcode language (NOT PostScript). The interpreter:
- Decrypts individual charstrings (R=4330, skip `lenIV` bytes, default 4)
- Executes opcodes → builds `PsPath` segments
- Returns glyph width for text measurement
- Handles subroutine calls (`callsubr`/`return`)
- Ignores hints (hstem/vstem) for Phase 4 — correct outlines, no hinting optimization

### 4. Font Name Mapping (Hardcoded)

Standard PostScript font names map to URW equivalents from PostForge's `resources/Font/`:
- Helvetica → NimbusSans-Regular, Helvetica-Bold → NimbusSans-Bold, etc.
- Times-Roman → NimbusRoman-Regular, Courier → NimbusMonoPS-Regular, etc.
- Full mapping from PostForge's `resources/Init/fontmapping.ps` (31 entries)

Fonts loaded lazily on first `findfont` access.

### 5. Show Pipeline

```
show(string)
  → for each byte in string:
      1. Look up char code in font's /Encoding → glyph name
      2. Look up glyph name in font's /CharStrings → encrypted bytes
      3. Decrypt charstring, execute → PsPath in glyph space
      4. Transform path: glyph space → user space (FontMatrix × scale × CTM)
      5. Fill path using device (same as regular fill)
      6. Advance currentpoint by glyph width × FontMatrix
```

### 6. GraphicsState Extension

Add `current_font: Option<PsObject>` to `GraphicsState`. The font is a dict object, cloned on gsave/grestore like all other gstate fields. No separate font stack needed.

---

## Implementation Steps (10 steps, always compiling)

### Step 1: GraphicsState + Context Changes
**Modify**: `crates/stet-core/src/graphics_state.rs`
- Add `current_font: Option<PsObject>` to `GraphicsState` (default: `None`)

**Modify**: `crates/stet-core/src/context.rs`
- Add `font_directory: EntityId` (a dict mapping font names → font dicts)
- Add `font_resource_path: Option<String>` (path to font directory on disk)
- Allocate `FontDirectory` dict in `Context::new()`
- Put `FontDirectory` in systemdict
- Pre-intern font-related names in `NameCache`: `FontName`, `FontType`, `FontMatrix`, `FontBBox`, `Encoding`, `CharStrings`, `Private`, `FID`, `PaintType`

**Modify**: `crates/stet-core/src/object.rs`
- Implement `PsObject::is_dict()` convenience method if not present

~0 new tests (structural changes only).

### Step 2: Standard Encoding Table
**New file**: `crates/stet-core/src/encoding.rs`

Define `STANDARD_ENCODING: [&str; 256]` — the PostScript StandardEncoding mapping byte values to glyph names. This is a static table (not a PostScript array).

Also define `ISO_LATIN1_ENCODING: [&str; 256]` for ISOLatin1Encoding.

Helper: `fn standard_encoding_name(code: u8) -> Option<&'static str>`

Register `StandardEncoding` and `ISOLatin1Encoding` as arrays in systemdict during `build_system_dict`.

~4 tests (spot-check encoding lookups: 'A' at 65, 'space' at 32, etc.).

### Step 3: Type 1 Font Parser
**New file**: `crates/stet-core/src/type1_parser.rs`

Parse `.t1` files and return structured data:

```rust
pub struct Type1Font {
    pub font_name: String,
    pub font_matrix: [f64; 6],
    pub font_bbox: [f64; 4],
    pub paint_type: i32,
    pub encoding: Vec<Option<String>>,  // 256 entries, glyph names
    pub charstrings: HashMap<String, Vec<u8>>,  // glyph name → encrypted bytes
    pub subrs: Vec<Vec<u8>>,  // subroutine charstrings (encrypted)
    pub private: PrivateDict,
}

pub struct PrivateDict {
    pub len_iv: usize,  // default 4
    pub blue_values: Vec<f64>,
    pub other_blues: Vec<f64>,
    pub std_hw: Vec<f64>,
    pub std_vw: Vec<f64>,
    // ... other hint fields (stored but not used in Phase 4)
}
```

**Key functions**:
- `pub fn parse_type1(data: &[u8]) -> Result<Type1Font, String>` — main entry point
- `fn parse_ascii_header(data: &[u8]) -> (header fields, eexec_offset)`
- `fn decrypt_eexec(data: &[u8]) -> Vec<u8>` — R=55665, C1=52845, C2=22719, skip 4 random bytes
- `fn parse_private_and_charstrings(decrypted: &[u8]) -> (PrivateDict, charstrings, subrs)`

The eexec decryption handles both binary and hex-encoded formats. The parser extracts charstrings as raw encrypted bytes (decryption happens at execution time in the charstring interpreter).

~8 tests (parse StandardSymbolsPS.t1, extract NimbusSans-Regular metadata, verify charstring count, encoding spot-checks).

### Step 4: CharString Interpreter
**New file**: `crates/stet-core/src/charstring.rs`

Execute Type 1 charstrings → path segments + width.

```rust
pub struct CharstringResult {
    pub path: PsPath,
    pub width_x: f64,
    pub width_y: f64,
    pub lsb_x: f64,  // left side bearing
    pub lsb_y: f64,
}

pub fn execute_charstring(
    charstring: &[u8],
    subrs: &[Vec<u8>],
    len_iv: usize,
    width_only: bool,  // true = skip path ops, just extract width
) -> Result<CharstringResult, String>
```

**Charstring decryption**: R=4330, C1=52845, C2=22719, skip `len_iv` random bytes.

**Number encoding**:
- 0–31: commands
- 32–246: single-byte integer (value = byte − 139)
- 247–250: two-byte positive (((byte − 247) × 256 + next) + 108)
- 251–254: two-byte negative (−((byte − 251) × 256 + next) − 108)
- 255: five-byte signed 32-bit integer

**Commands implemented**:
| Code | Name | Action |
|------|------|--------|
| 1 | hstem | Ignore (hint) |
| 3 | vstem | Ignore (hint) |
| 4 | vmoveto | Relative vertical moveto |
| 5 | rlineto | Relative lineto |
| 6 | hlineto | Horizontal lineto |
| 7 | vlineto | Vertical lineto |
| 8 | rrcurveto | Relative curveto (6 args) |
| 9 | closepath | Close subpath |
| 10 | callsubr | Call subroutine |
| 11 | return | Return from subroutine |
| 13 | hsbw | Set sidebearing + width (2 args: sbx, wx) |
| 14 | endchar | End character — signal completion |
| 21 | rmoveto | Relative moveto |
| 22 | hmoveto | Horizontal moveto |
| 30 | vhcurveto | Vertical-horizontal curve (4 args) |
| 31 | hvcurveto | Horizontal-vertical curve (4 args) |
| 12,0 | dotsection | Ignore (hint) |
| 12,1 | vstem3 | Ignore (hint) |
| 12,2 | hstem3 | Ignore (hint) |
| 12,6 | seac | Accent composition |
| 12,7 | sbw | Set sidebearing + width (4 args: sbx, sby, wx, wy) |
| 12,12 | div | Integer division |
| 12,16 | callothersubr | OtherSubrs (flex support) |
| 12,17 | pop | Pop from OtherSubrs stack |
| 12,33 | setcurrentpoint | Set current point |

**seac** (accent composition): Look up base and accent characters by StandardEncoding index, execute both charstrings, position accent with offset.

**callothersubr/pop**: OtherSubrs 0–3 handle flex curves. OtherSubr 0 = end flex (construct curveto from saved points). OtherSubr 1 = start flex. OtherSubr 2 = add flex point. OtherSubr 3 = replace hints (ignore). Implement basic flex support.

~15 tests (decrypt charstring, execute simple glyph like 'space'/'A'/'o', verify path segment count, verify width extraction, test subroutine call, test seac).

### Step 5: Font Loading Pipeline
**New file**: `crates/stet-core/src/font_loader.rs`

```rust
/// Load a Type 1 font from a .t1 file and register it as a PostScript dict.
pub fn load_type1_font(
    ctx: &mut Context,
    font_data: &[u8],
) -> Result<PsObject, String>
```

This function:
1. Calls `parse_type1()` to get `Type1Font` struct
2. Creates a PostScript dict with required entries:
   - `/FontName` → name object
   - `/FontType` → Int(1)
   - `/FontMatrix` → array of 6 reals
   - `/FontBBox` → array of 4 reals
   - `/Encoding` → array of 256 name objects
   - `/PaintType` → Int(0 or 2)
   - `/CharStrings` → dict mapping glyph names to string objects (encrypted bytes)
   - `/Private` → dict with lenIV and other hint data
   - `/Subrs` → array of string objects (encrypted subroutine bytes)
   - `/FID` → Int (unique font ID counter)
3. Registers font dict in `FontDirectory`
4. Returns the font dict object

**Font name mapping table** (hardcoded in this module):
```rust
const FONT_SUBSTITUTIONS: &[(&str, &str)] = &[
    ("Helvetica", "NimbusSans-Regular"),
    ("Helvetica-Bold", "NimbusSans-Bold"),
    ("Helvetica-Oblique", "NimbusSans-Italic"),
    ("Helvetica-BoldOblique", "NimbusSans-BoldItalic"),
    ("Times-Roman", "NimbusRoman-Regular"),
    ("Times-Bold", "NimbusRoman-Bold"),
    ("Times-Italic", "NimbusRoman-Italic"),
    ("Times-BoldItalic", "NimbusRoman-BoldItalic"),
    ("Courier", "NimbusMonoPS-Regular"),
    ("Courier-Bold", "NimbusMonoPS-Bold"),
    ("Courier-Oblique", "NimbusMonoPS-Italic"),
    ("Courier-BoldOblique", "NimbusMonoPS-BoldItalic"),
    ("Symbol", "StandardSymbolsPS"),
    ("ZapfDingbats", "D050000L"),
    ("Palatino-Roman", "P052-Roman"),
    ("Palatino-Bold", "P052-Bold"),
    ("Palatino-Italic", "P052-Italic"),
    ("Palatino-BoldItalic", "P052-BoldItalic"),
    ("NewCenturySchlbk-Roman", "C059-Roman"),
    ("NewCenturySchlbk-Bold", "C059-Bold"),
    ("NewCenturySchlbk-Italic", "C059-Italic"),
    ("NewCenturySchlbk-BoldItalic", "C059-BdIta"),
    ("Bookman-Light", "URWBookman-Light"),
    ("Bookman-LightItalic", "URWBookman-LightItalic"),
    ("Bookman-Demi", "URWBookman-Demi"),
    ("Bookman-DemiItalic", "URWBookman-DemiItalic"),
    ("AvantGarde-Book", "URWGothic-Book"),
    ("AvantGarde-BookOblique", "URWGothic-BookOblique"),
    ("AvantGarde-Demi", "URWGothic-Demi"),
    ("AvantGarde-DemiOblique", "URWGothic-DemiOblique"),
    ("ZapfChancery-MediumItalic", "Z003-MediumItalic"),
];
```

**Font search**: `findfont` checks FontDirectory first, then tries to load from disk:
1. Look up requested name in substitution table → get URW filename
2. Search `font_resource_path` for `{name}.t1`
3. Parse and register

~6 tests (load NimbusSans-Regular.t1, verify dict structure, verify encoding, verify charstring count).

### Step 6: Font Dictionary Operators
**New file**: `crates/stet-ops/src/font_ops.rs`

7 operators:

| Operator | Stack | Description |
|----------|-------|-------------|
| `definefont` | key font → font | Register font in FontDirectory, assign FID |
| `undefinefont` | key → — | Remove font from FontDirectory |
| `findfont` | key → font | Look up in FontDirectory, load from disk if needed |
| `scalefont` | font scale → font' | Copy font dict, scale FontMatrix by scalar |
| `makefont` | font matrix → font' | Copy font dict, compose matrix with FontMatrix |
| `setfont` | font → — | Set gstate.current_font |
| `currentfont` | — → font | Push gstate.current_font |

**scalefont detail**: Creates a shallow copy of the font dict. Composes `[scale 0 0 scale 0 0]` with existing FontMatrix using matrix multiplication. Removes `/FID` from copy (per PLRM).

**findfont detail**:
1. Check FontDirectory dict
2. If not found, check font substitution table
3. If substitution found, load .t1 file from disk
4. If still not found, return `/invalidfont` error

~10 tests (definefont/findfont roundtrip, scalefont matrix composition, makefont, setfont/currentfont, findfont with substitution, findfont error on unknown font).

### Step 7: Text Show Operators
**New file**: `crates/stet-ops/src/show_ops.rs`

Core text rendering operators:

| Operator | Stack | Description |
|----------|-------|-------------|
| `show` | string → — | Render each character, advance currentpoint |
| `ashow` | ax ay string → — | show with extra spacing per character |
| `widthshow` | cx cy char string → — | show with extra spacing for specific char |
| `awidthshow` | cx cy char ax ay string → — | Combined ashow + widthshow |
| `kshow` | proc string → — | show calling proc between each pair |
| `stringwidth` | string → wx wy | Measure text width without rendering |
| `charpath` | string bool → — | Append glyph outlines to current path |
| `setcachedevice` | wx wy llx lly urx ury → — | Set cache device (stub: just record width) |
| `setcharwidth` | wx wy → — | Set character width (for Type 3) |

**show implementation**:
```
fn render_show(ctx, string_bytes, extra_ax, extra_ay, width_char, cx, cy):
    font = ctx.gstate.current_font  // must be set
    font_matrix = read /FontMatrix from font dict
    encoding = read /Encoding array from font dict
    charstrings = read /CharStrings dict from font dict
    subrs = read /Subrs array from font dict
    private = read /Private dict from font dict
    len_iv = read /lenIV from private (default 4)

    for each byte in string:
        glyph_name = encoding[byte]  // name object
        charstring_bytes = charstrings[glyph_name]  // string object

        // Execute charstring → path + width
        result = execute_charstring(charstring_bytes, subrs, len_iv, false)

        // Transform path from glyph space to user space
        // Glyph space → character space via FontMatrix
        // Character space = user space (scalefont already modified FontMatrix)
        for segment in result.path:
            transform segment coordinates through font_matrix

        // Save current path, render glyph, restore path
        saved_path = ctx.gstate.path.clone()
        ctx.gstate.path = transformed_glyph_path
        device.fill_path(...)  // fill with current color + CTM
        ctx.gstate.path = saved_path

        // Advance currentpoint
        (wx, wy) = transform (result.width_x, result.width_y) through font_matrix
        currentpoint += (wx + extra_ax, wy + extra_ay)
        if byte == width_char:
            currentpoint += (cx, cy)
```

**stringwidth**: Same loop but with `width_only: true` — no path construction, no device calls.

**charpath**: Same as show but appends glyph path to current path instead of filling. The `bool` parameter controls stroke compatibility (true = suitable for stroke/clip, false = fill only).

~12 tests (show renders non-empty PNG, stringwidth returns positive width, ashow spacing, charpath appends to path, setcachedevice/setcharwidth stack behavior).

### Step 8: Page Size Convenience Operators + selectfont
**Modify**: `crates/stet-ops/src/lib.rs`

Define convenience operators as no-ops (page size already set by CLI):
- `letter`, `legal`, `a4`, `a3`, `b5` — all no-ops
- `selectfont` — equivalent to `exch findfont exch scalefont setfont`

~2 tests.

### Step 9: Copy Font Resources + Wire CLI
**Copy**: PostForge font files to `resources/Font/`
```
cp ~/Projects/postforge/postforge/resources/Font/*.t1 resources/Font/
```

**Modify**: `crates/stet-cli/src/main.rs`
- Set `ctx.font_resource_path` to locate `resources/Font/` directory
- Search relative to executable, then relative to CWD, then absolute path

**Modify**: `Cargo.toml` / workspace config if needed

### Step 10: Operator Registration + Integration Tests
**Modify**: `crates/stet-ops/src/lib.rs`
- Register all new operators (7 font + 9 show + convenience ops)
- Add `pub mod font_ops`, `pub mod show_ops`

**New/Modify**: `crates/stet-engine/tests/rendering.rs`
- Add font rendering integration tests:
  1. Simple show: `findfont scalefont setfont (Hello) show` → non-empty PNG
  2. stringwidth returns positive values
  3. Multiple fonts in same document
  4. charpath + stroke (outlined text)
  5. test1.ps renders correctly
  6. golfer.ps continues to render correctly

~6 integration tests.

---

## test1.ps Requirements

```postscript
letter                              % → no-op (page size preset)
/Helvetica findfont 40 scalefont setfont  % → load NimbusSans-Regular, scale, set
(Translated) show                   % → render text at currentpoint
```

Operators needed beyond Phase 3: `letter` (no-op), `findfont`, `scalefont`, `setfont`, `show`.

## golfer.ps Font Analysis

golfer.ps defines font operators (`z`, `Z`, `_s`/`_S`, `_E`) but only executes them in the error handler `_E` which fires if `{fill}stopped` catches an error. If all fills succeed, no font code runs. golfer.ps should already work without fonts. If it doesn't, the issue is likely `fill` on empty/degenerate paths — we should fix that in Phase 3 cleanup rather than requiring fonts.

---

## Files Summary

**New files (6):**
- `crates/stet-core/src/encoding.rs` — StandardEncoding, ISOLatin1Encoding tables
- `crates/stet-core/src/type1_parser.rs` — .t1 file parser with eexec decryption
- `crates/stet-core/src/charstring.rs` — Type 1 charstring interpreter
- `crates/stet-core/src/font_loader.rs` — Font loading pipeline, name mapping
- `crates/stet-ops/src/font_ops.rs` — 7 font dictionary operators
- `crates/stet-ops/src/show_ops.rs` — 9 text show operators

**Modified files (5):**
- `crates/stet-core/src/lib.rs` — add `encoding`, `type1_parser`, `charstring`, `font_loader` modules
- `crates/stet-core/src/graphics_state.rs` — add `current_font` to GraphicsState
- `crates/stet-core/src/context.rs` — add `font_directory`, `font_resource_path`, font name cache entries
- `crates/stet-ops/src/lib.rs` — register ~18 new operators, add 2 new modules
- `crates/stet-cli/src/main.rs` — set font resource path

**Copied resources:**
- `resources/Font/*.t1` — 35 Type 1 font files from PostForge

---

## Operator Count

Phase 3: ~189 → Phase 4: ~207 (+18 new)

| Category | New | Operators |
|----------|-----|-----------|
| Font Dict | 7 | definefont, undefinefont, findfont, scalefont, makefont, setfont, currentfont |
| Text Show | 9 | show, ashow, widthshow, awidthshow, kshow, stringwidth, charpath, setcachedevice, setcharwidth |
| Convenience | 2 | selectfont, letter (+ legal/a4/a3/b5 as no-ops) |

## Test Target

~63 new tests → ~333 total

---

## Verification

1. `cargo build` — compiles cleanly
2. `cargo test` — all tests pass (270 existing + ~63 new)
3. `cargo clippy` — zero warnings
4. `cargo run -- ~/Projects/postforge/samples/test1.ps` — produces PNG with visible text labels (boxes + "Translated", "Translated & Rotated", etc.)
5. `cargo run -- /tmp/tiger.ps` — still renders correctly (regression)
6. `cargo run -- ~/Projects/postforge/samples/golfer.ps` — renders golfer artwork

---

## Risk Mitigations

| Risk | Mitigation |
|------|------------|
| eexec decryption correctness | Test against known .t1 files, compare parsed charstring count with PostForge |
| Charstring path accuracy | Compare glyph outlines visually against PostForge renders |
| Font matrix composition | Test scalefont/makefont output against PostForge's `_compose_matrices()` |
| Missing glyphs (.notdef) | Handle gracefully — skip rendering, advance by default width |
| seac (accent composition) | Port from PostForge's charstring interpreter; test with accented chars |
| OtherSubrs/flex | Implement basic flex (OtherSubrs 0-3) from PostForge reference |
| Binary vs hex eexec | Support both formats — detect by checking first bytes after marker |

---

## Deferred to Later Phases

- **Type 3 fonts** (BuildGlyph/BuildChar execution) — Phase 6 with init scripts
- **CID/TrueType fonts** — Phase 6+
- **Glyph caching** — Performance optimization, later phase
- **eexec operator** (execute encrypted PS) — Phase 6 with init scripts
- **Font resources via findresource** — Phase 6 resource system
