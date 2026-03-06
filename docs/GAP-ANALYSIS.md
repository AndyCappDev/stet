# stet vs PostForge: Gap Analysis

**Date**: 2026-03-05 (updated 2026-03-08)
**Purpose**: Comprehensive comparison of stet (Rust) against PostForge (Python) — the reference PostScript Level 3 interpreter — to identify all gaps that must be closed for 1:1 feature parity.

---

## Executive Summary

stet has strong foundations across core interpreter mechanics, graphics, fonts, images, shading, filters, and resources. However, significant gaps remain in several areas:

- ~~**9 missing operators** (userpath, path, misc, filter categories)~~ — userpath operators **RESOLVED** (2026-03-07)
- ~~**Encode filters** are stub pass-throughs~~ — **RESOLVED** (2026-03-06)
- ~~**CCITTFax filters** not implemented~~ — **CCITTFaxDecode RESOLVED** (2026-03-06); CCITTFaxEncode deferred
- ~~**Binary Object Sequences (BOS)** not implemented~~ — **RESOLVED** (2026-03-08)
- **Output devices**: stet has PNG + viewer; PF also has PDF, SVG, TIFF
- **Glyph caching** not implemented (plan exists)
- **No test suite failures** — all 47 suites pass

### Test Suite Status (stet unit_tests)

Results from running stet's own copy of the test suites (`~/Projects/stet/unit_tests/`).

| Status | Count | Suites |
|--------|-------|--------|
| **PASS** | 47 | arc_shading, arithmetic_and_math, array, **binary_token**, cff, clipping, color_operators, context_param, control, **dct_filter**, defined_ps_operator, device_operator, dictionary, **file**, file_operators, filter_chain, **filter_extended**, flate_filter, font, graphics_state_params, gstate, halftone_transfer, image, interpreter_param, job_control, job_control_tests_standalone, matrix, **misc**, nulldevice, operand_stack, packedarray, painting, path, **pattern_form**, print_integration, rel_bool_bitwise, resource, save_invalidation, show_variant, stdio, string, strokepath, type1_font, type_attrib_conv, **userpath**, vm_gstate_integration, vm_operators |
| ~~FAIL~~ | 0 | ~~All resolved~~ |
| **Total** | 47 | (excluding unittest.ps framework file) |

---

## 1. Missing Operators

### ~~1.1 Userpath Operators~~ — RESOLVED (2026-03-07)

All 11 userpath operators implemented: `setbbox`, `ucache`, `uappend`, `upath`, `ufill`, `ueofill`, `ustroke`, `ustrokepath`, `inufill`, `inueofill`, `inustroke`. Supports both ordinary (executable array) and encoded (data+opcode) userpath formats. All userpath_tests.ps tests pass.

### 1.2 Missing Individual Operators

| Operator | Description | In PF | In stet | Notes |
|----------|-------------|-------|---------|-------|
| `flushpage` | Flush page output | Yes | **No** | Simple no-op for non-printer devices |
| ~~`echo`~~ | ~~Toggle echo mode~~ | ~~Yes~~ | ~~Yes~~ | ~~Implemented (2026-03-06)~~ |
| `loopname` | Get current loop name | Yes | **No** | Debugging aid |
| `help` | Print help | Yes | **No** | Interactive REPL feature |
| `printostack` | Print operand stack | Yes | **No** | Debugging variant |
| `runlibfile` | Run library file | Yes | **No** | Like `run` but searches lib paths |
| `breaki` | Break with interrupt | Yes | **No** | Control flow |
| `createresourcecategory` | Create new resource category | Yes | **No** | Resource system extension |

---

## 2. Stubbed/Incomplete Operators

### ~~2.1 Pattern & Form Support~~ — RESOLVED (2026-03-05)

`makepattern`, `setpattern`, and `execform` fully implemented. Tiled patterns render via `DisplayElement::PatternFill` with tile replay in SkiaDevice. Form XObjects cached at identity CTM, replayed with path-point transformation through real CTM. All 6 pattern_form_tests pass; visual output matches PostForge.

### ~~2.2 Encode Filters~~ — RESOLVED (2026-03-06)

Encode filters fully implemented: ASCIIHexEncode, ASCII85Encode, RunLengthEncode, FlateEncode, LZWEncode, NullEncode, DCTEncode. Write-direction filters use swap-out pattern for `encode_write` dispatch, with finalization on `closefile` (flush remaining data, write EOD markers, close target). DCTEncode uses `jpeg-encoder` crate (buffered: collects all input, encodes on close). All 18 encode/decode roundtrip tests in filter_extended_tests.ps pass. Also fixed a pre-existing LZW decode bug with short streams. Additional fixes (2026-03-06): `filter` operator validates filter name before popping operands (prevents stack corruption on unknown filters like CCITTFaxDecode); SubFileDecode now includes EOD string in output per PLRM spec; RunLengthEncode converted from buffered to streaming.

### ~~2.3 CCITTFax Filters~~ — CCITTFaxDecode RESOLVED (2026-03-06)

| Filter | PF Status | stet Status |
|--------|-----------|-------------|
| `CCITTFaxDecode` | Full impl (271 lines) | **Implemented** (fax crate) |
| `CCITTFaxEncode` | Listed but not functional | **Not implemented** (deferred) |

CCITTFaxDecode implemented using the `fax` Rust crate (MIT, from pdf-rs). Supports all PLRM parameters: K (Group 3/4 selection), Columns, Rows, EndOfLine, EncodedByteAlign, EndOfBlock, BlackIs1. Buffered decode pattern (like DCTDecode). CCITTFaxEncode deferred — PostForge doesn't implement it either, no test coverage, rarely used.

---

## 3. Missing Core Features

### ~~3.1 Binary Object Sequences / Binary Tokens~~ — RESOLVED (2026-03-08)

Full binary token and BOS parser implemented in `binary_token.rs` (580 lines). Supports all token types 128-149: integers (132-136), fixed-point (137), reals (138-140), booleans (141), strings (142-144), system names (145-146), homogeneous number arrays (149), and binary object sequences (128-131). Includes 481-entry system name table per PLRM Appendix F. Both slice-based (fast path) and streaming parsing paths. BOS auto-execution at top level. Also fixed `putback_bytes` for real files (was silently discarding) and real serialization bug in `writeobject`. All 55 binary_token_tests.ps and 11 file_tests.ps BOS failures resolved.

### 3.2 Glyph Caching (NOT IMPLEMENTED)

PostForge has a glyph cache system (glyph_cache.py, system_font_cache.py — ~463 lines). stet has no implementation.

**Impact**: Performance only — no correctness impact. Every glyph is re-rendered from outlines on every use.

### 3.3 ICC Color Profile Support (NOT IMPLEMENTED)

PostForge has ICC profile parsing and default ICC profiles (icc_profile.py: 392 lines, icc_default.py: 245 lines). stet has no ICC support.

**Impact**: Some PS files embed ICC profiles for precise color management. Current CIE-based color space support covers most cases, but ICC profiles provide more accurate color reproduction.

### 3.4 Unicode/ToUnicode Mapping (NOT IMPLEMENTED)

PostForge has unicode mapping support (unicode_mapping.py: 361 lines) for generating text extraction data.

**Impact**: PDF output text searchability. Not critical for rasterization.

### 3.5 DSC (Document Structuring Conventions) Parsing

PostForge has a DSC parser (dsc_parser.py: 62 lines). stet has basic EPS BoundingBox parsing but no general DSC support.

**Impact**: Limited — mainly affects page-level metadata extraction.

### 3.6 System Font Discovery (IMPLEMENTED)

stet scans platform-specific font directories (Linux: `/usr/share/fonts`, `/usr/local/share/fonts`, `~/.local/share/fonts`, `~/.fonts`; macOS: `/System/Library/Fonts`, `/Library/Fonts`, `~/Library/Fonts`), extracts PostScript names from `.t1`/`.pfa`/`.pfb`/`.otf`/`.ttf` files, and caches the mapping to `~/.cache/stet/system_fonts.json` with directory mtime-based staleness checking. Three native operators (`.loadsystemfont`, `.loadbinarysystemfont`, `.loadbinaryfontfile`) load system fonts on demand: OTF+CFF via the CFF parser pipeline, TTF via Type 42 font dict construction (sfnts, Encoding from cmap, CharStrings GID mapping). `fontcategory.ps` searches `resources/Font/` with multiple extensions (`.t1`/`.pfa`/`.pfb`/`.ttf`/`.otf`), then falls back to system fonts before the default font substitution.

**Remaining gaps**: PFB system fonts (rare on modern systems), `.ttc` font collection files, WASM builds (no filesystem access — limited to bundled fonts).

---

## 4. Bugs — RESOLVED

All P0 bugs have been fixed. The following were fixed on 2026-03-05:

- **image PANIC** — u32 overflow in `width * height` (5 locations cast to usize before multiply)
- **image validation** — zero-dimension check, short string source check, dict-form ImageType/Width/Height strict validation
- **dict put** — now rejects array/packedarray/dict keys with TypeCheck
- **bind** — now raises TypeCheck for non-procedure operands (literal arrays still no-op)
- **realtime** — changed from epoch millis (overflows i32) to elapsed time since interpreter start

### ~~4.1 misc~~ — RESOLVED (2026-03-06)
- ~~`echo` operator implemented — all 5 failures resolved~~

### 4.2 ps (meta-runner)
- Meta-runner that executes all other test suites; its failures are the sum of sub-suite failures + test harness artifacts — not independent bugs

### 4.3 Suites Failing Due to Feature Gaps (NOT bugs)

These suites fail entirely because of unimplemented features, not defects:

| Suite | Failures | Missing Feature |
|-------|----------|----------------|
| ~~dct_filter~~ | ~~0~~ | ~~Fixed (2026-03-06): DCTEncode + DCTDecode param validation + real JPEG encoding~~ |
| ~~binary_token~~ | ~~0~~ | ~~Binary Object Sequences — RESOLVED (2026-03-08)~~ |
| ~~file~~ | ~~0~~ | ~~BOS round-trip tests — RESOLVED (2026-03-08)~~ |
| ~~userpath~~ | ~~0~~ | ~~Userpath operators — RESOLVED (2026-03-07)~~ |
| ~~filter_extended~~ | ~~0~~ | ~~Encode filters — RESOLVED (2026-03-06)~~ |

---

## 5. Output Device Gaps

| Device | PostForge | stet | Notes |
|--------|-----------|------|-------|
| **PNG** | Yes (Pillow) | Yes (tiny-skia) | Feature parity |
| **Interactive Viewer** | Yes (Qt) | Yes (egui) | Feature parity |
| **PDF** | Yes (7,874 lines) | **No** | Major output format |
| **SVG** | Yes (434 lines) | **No** | Vector output |
| **TIFF** | Yes (275 lines) | **No** | Multi-page raster |
| **WASM Viewport** | No | Yes | stet-only feature |

**Impact**: PDF output is a significant gap for production use. SVG and TIFF are lower priority.

---

## 6. Resource System Gaps

### Resource Files Comparison

| Resource Category | PostForge | stet | Gap |
|-------------------|-----------|------|-----|
| Init scripts | 4 files (sysdict, resourcecategories, fontcategory, fontmapping) | 4 files (same set) | None |
| Encoding | 3 (Standard, ISOLatin1, Symbol) | 3 (same set) | None |
| Font (.t1) | 36 | 35 | Minor (1 font) |
| FontSet (CFF) | 1 (NimbusRoman-Regular-CFF) | 1 (same) | None |
| ProcSet | 3 (CIDInit, FontSetInit, TestProcSet) | 2 (CIDInit, FontSetInit) | Missing TestProcSet |
| OutputDevice | 5 (png, qt, svg, pdf, tiff) | 2 (png, viewer) | Missing svg, pdf, tiff |
| CMap | 0 | 2 (Identity-H, Identity-V) | stet ahead |
| ColorSpace | 4 (DefaultGray/RGB/CMYK, Test) | 0 | Missing defaults |
| ColorRendering | 1 (Test) | 0 | Missing |
| Halftone | 1 (Test) | 0 | Missing |
| Pattern | 1 (Test) | 0 | Missing |
| Form | 1 (Test) | 0 | Missing |

The "Test" resources in PF are used by the test suite. The Default ColorSpace resources may be needed for proper color space defaulting per PLRM.

---

## 7. Font & Text Rendering Gaps

| Feature | PostForge | stet | Gap |
|---------|-----------|------|-----|
| Type 1 | Full | Full | None |
| Type 3 | Full | Full | None |
| Type 42 (TrueType) | Full | Full | None |
| CFF/Type 2 | Full | Full | None |
| CIDFont Type 0 (CFF) | Full | Full | None |
| CIDFont Type 2 (TT) | Full | Full | None |
| Type 0 Composite | Full | Full | None |
| CMap decoding | Full | Full | None |
| Glyph caching | Yes | **No** | Performance only |
| System font discovery | Yes | **No** | See 3.6 |
| `composefont` | Yes | Yes | None |

Font support is at feature parity for correctness. System font discovery is implemented. Remaining gap is performance (glyph caching).

---

## 8. Color Space Gaps

| Color Space | PostForge | stet | Gap |
|-------------|-----------|------|-----|
| DeviceGray | Full | Full | None |
| DeviceRGB | Full | Full | None |
| DeviceCMYK | Full | Full | None |
| CIEBasedABC | Full | Full | None |
| CIEBasedA | Full | Full | None |
| CIEBasedDEF | Full | Full | None |
| CIEBasedDEFG | Full | Full | None |
| Indexed | Full | Full | None |
| Separation | Full | Full | None |
| DeviceN | Full | Full | None |
| Pattern | Full | Full | makepattern/setpattern/execform implemented |
| ICC Profile | Full | **No** | See 3.3 |

---

## 9. Image Handling Gaps

| Feature | PostForge | stet | Gap |
|---------|-----------|------|-----|
| `image` (Type 1) | Full | Full | None |
| `imagemask` | Full | Full | None |
| `colorimage` | Full | Full | None |
| Image Type 3 (masked) | Full | Full | None |
| Image Type 4 (color key mask) | Full | Full | None |
| DCT (JPEG) decode | Full | Full | None |
| Data source: file | Full | Full | None |
| Data source: string | Full | Full | None |
| Data source: procedure | Full | Full | None |
| Multi-source (nproc=ncomp) | Full | Full | None |
| InterleaveType 1 (sample) | Full | Full | None |
| InterleaveType 2 (scanline) | Full | Partial? | Needs verification |
| InterleaveType 3 (separate) | Full | Full | None |

---

## 10. Shading / Gradient Gaps

All 7 shading types are implemented in both interpreters. **No gaps.**

---

## 11. Filter Gaps Summary

| Filter | Decode (PF) | Decode (stet) | Encode (PF) | Encode (stet) |
|--------|-------------|---------------|-------------|----------------|
| ASCIIHex | Full | Full | Full | Full |
| ASCII85 | Full | Full | Full | Full |
| RunLength | Full | Full | Full | Full (buffered) |
| Flate | Full | Full | Full | Full |
| LZW | Full | Full | Full | Full |
| DCT | Full | Full | Full | Full (buffered) |
| CCITTFax | Full | Full (fax crate) | Full | **Missing** (deferred) |
| SubFile | Full | Full | N/A | N/A |
| NullEncode | N/A | N/A | Full | Full |
| ReusableStreamDecode | Full | Full | N/A | N/A |
| Eexec | Full | Full | N/A | N/A |

---

## 12. Miscellaneous Gaps

| Feature | PostForge | stet | Notes |
|---------|-----------|------|-------|
| `pstack` (PS-defined) | Via sysdict.ps | Has native impl | Different approach, both work |
| Error handler dict | Full errordict | Full errordict | Parity |
| save/restore | Full COW | Full COW | Parity |
| Dual VM (local/global) | Full | Full | Parity |
| Display list rendering | Full | Full | Parity |
| Banded rendering | No | Yes | stet advantage |
| WASM support | No | Yes | stet advantage |
| Pipeline rendering | No | Yes | stet advantage |
| Stroke adjust | Full | Full | Parity |
| Strokepath algorithm | Full | Full | Parity |
| `=` and `==` operators | Via sysdict.ps | Via sysdict.ps | Parity |

---

## 13. Priority-Ordered Gap Summary

### P0 — Critical (Bugs in implemented features)
~~All P0 bugs resolved (2026-03-05).~~

### P1 — High (Missing functionality used by real-world PS files)
~~1. **Pattern/Form rendering** — RESOLVED (2026-03-05)~~
~~2. **Encode filters** — RESOLVED (2026-03-06): ASCIIHex, ASCII85, RunLength, Flate, LZW, NullEncode, DCTEncode implemented.~~
~~3. **Binary Object Sequences** — RESOLVED (2026-03-08): Full binary token/BOS parser, all token types 128-149, 481-entry system name table, slice and streaming paths, BOS auto-execution. Also fixed putback_bytes for real files and writeobject real serialization bug.~~
~~4. **Userpath operators** (11 operators) — RESOLVED (2026-03-07)~~
~~5. **DCTEncode** — RESOLVED (2026-03-06): Full JPEG encoding via jpeg-encoder crate + parameter validation~~
~~6. **Encode filter streaming debt** — RESOLVED (2026-03-06): RunLengthEncode converted to streaming encoder. DCTEncode buffering is inherent to JPEG (DCT transform requires complete image data); not technical debt.~~
~~6. **`echo` operator** — RESOLVED (2026-03-06)~~

### P2 — Medium (Feature completeness)
~~11. **CCITTFax filters** — RESOLVED (2026-03-06): CCITTFaxDecode implemented via fax crate; CCITTFaxEncode deferred~~
12. **System font discovery** — find system-installed fonts
13. **Missing misc operators** (flushpage, runlibfile, loopname, help, printostack, breaki, createresourcecategory)
14. **Default ColorSpace resources** (DefaultGray, DefaultRGB, DefaultCMYK)
15. **Glyph caching** — performance improvement

### P3 — Low (Nice-to-have)
12. **PDF output device** — major feature but separate from PS interpretation
13. **SVG output device**
14. **TIFF output device**
15. **ICC color profile support**
16. **DSC parsing** (beyond basic EPS BoundingBox)
17. **Unicode/ToUnicode mapping**

---

## Appendix A: Complete Operator Comparison

### Operators in PostForge but NOT in stet

```
breaki            echo              flushpage         help
loopname          printostack       runlibfile
createresourcecategory
```

### Operators in stet but NOT in PostForge

```
cleardictstack    letter            legal             pstack (native)
selectfont (native)  .showpage_continue  .copypage_continue
.error            .loadfont
```

### Operators in both — implemented identically (feature parity)

All standard PLRM operators for: stack, math, relational/boolean/bitwise, type/conversion, dictionary, control flow, composite, array, string, file I/O, filter (decode), VM, matrix, path construction, color, graphics state, painting, clipping, path query, insideness (infill/ineofill/instroke), userpath (setbbox/ucache/uappend/upath/ufill/ueofill/ustroke/ustrokepath/inufill/inueofill/inustroke), font management, text show, halftone/transfer, shading, image, pattern/form (makepattern/setpattern/execform), resource, interpreter parameters, device setup, CFF.

---

## Appendix B: Test Failure Details

Results from stet's own unit_tests directory (`~/Projects/stet/unit_tests/`).

| Test Suite | Failures | Root Cause Category |
|-----------|----------|-------------------|
| ~~dct_filter~~ | ~~0~~ | ~~Fixed (2026-03-06): DCTEncode/DCTDecode param validation + JPEG encoding~~ |
| ~~misc~~ | ~~0~~ | ~~Fixed (2026-03-06): `echo` operator implemented~~ |
| ~~binary_token~~ | ~~0~~ | ~~Fixed (2026-03-08): full binary token/BOS parser implemented~~ |
| ~~file~~ | ~~0~~ | ~~Fixed (2026-03-08): BOS round-trip tests pass; also fixed putback_bytes for real files~~ |
| ~~userpath~~ | ~~0~~ | ~~Fixed (2026-03-07): all 11 userpath operators implemented~~ |
| ~~filter_extended~~ | ~~0~~ | ~~Fixed (2026-03-06): encode filters implemented + LZW decode fix~~ |
| ~~pattern_form~~ | ~~0~~ | ~~Fixed (2026-03-05)~~ |
| ~~dictionary~~ | ~~0~~ | ~~Fixed (2026-03-05)~~ |
| ~~image~~ | ~~0~~ | ~~Fixed (2026-03-05)~~ |
| **Total** | **0** | All resolved |
