# stet vs PostForge: Gap Analysis

**Date**: 2026-03-05
**Purpose**: Comprehensive comparison of stet (Rust) against PostForge (Python) — the reference PostScript Level 3 interpreter — to identify all gaps that must be closed for 1:1 feature parity.

---

## Executive Summary

stet has strong foundations across core interpreter mechanics, graphics, fonts, images, shading, filters, and resources. However, significant gaps remain in several areas:

- **12 missing operators** (userpath, pattern/form, path, misc, filter categories)
- **Encode filters** are stub pass-throughs
- **CCITTFax filters** not implemented
- **Binary Object Sequences (BOS)** not implemented
- **Pattern rendering** is stubbed (makepattern/execform/setpattern)
- **Output devices**: stet has PNG + viewer; PF also has PDF, SVG, TIFF
- **Glyph caching** not implemented (plan exists)
- **Feature gaps** causing 8 test suites to fail (~127 total failures)

### Test Suite Status (stet unit_tests)

Results from running stet's own copy of the test suites (`~/Projects/stet/unit_tests/`).

| Status | Count | Suites |
|--------|-------|--------|
| **PASS** | 40 | arc_shading, arithmetic_and_math, array, cff, clipping, color_operators, context_param, control, defined_ps_operator, device_operator, **dictionary**, file_operators, filter_chain, flate_filter, font, graphics_state_params, gstate, halftone_transfer, **image**, interpreter_param, job_control, job_control_tests_standalone, matrix, nulldevice, operand_stack, packedarray, painting, path, print_integration, rel_bool_bitwise, resource, save_invalidation, show_variant, stdio, string, strokepath, type1_font, type_attrib_conv, vm_gstate_integration, vm_operators |
| **FAIL** | 8 | binary_token(55), dct_filter(9), file(11), filter_extended(13), misc(5), pattern_form(6), ps(16), userpath(12) |
| **Total** | 48 | (excluding unittest.ps framework file) |

---

## 1. Missing Operators

### 1.1 Userpath Operators (12 ops — COMPLETELY MISSING)

PostForge implements the full userpath operator set. stet has none of these.

| Operator | Description | PLRM |
|----------|-------------|------|
| `uappend` | Append user path to current path | L2 |
| `ucache` | Declare user path cacheable | L2 |
| `ueofill` | Even-odd fill user path | L2 |
| `ufill` | Fill user path | L2 |
| `upath` | Create user path from current path | L2 |
| `ustroke` | Stroke user path | L2 |
| `ustrokepath` | Convert user path stroke to path | L2 |
| `inufill` | Test point inside user path fill | L2 |
| `inueofill` | Test point inside user path eofill | L2 |
| `inustroke` | Test point inside user path stroke | L2 |
| `setbbox` | Set path bounding box | L2 |

**Impact**: 12 test failures in userpath_tests.ps. Some real-world PS files use user paths.

### 1.2 Missing Individual Operators

| Operator | Description | In PF | In stet | Notes |
|----------|-------------|-------|---------|-------|
| `setpattern` | Set pattern color | Yes | **No** | Registered but no-op in PF's pattern_form; not in stet at all |
| `flushpage` | Flush page output | Yes | **No** | Simple no-op for non-printer devices |
| `echo` | Toggle echo mode | Yes | **No** | Interactive REPL feature |
| `loopname` | Get current loop name | Yes | **No** | Debugging aid |
| `help` | Print help | Yes | **No** | Interactive REPL feature |
| `printostack` | Print operand stack | Yes | **No** | Debugging variant |
| `runlibfile` | Run library file | Yes | **No** | Like `run` but searches lib paths |
| `breaki` | Break with interrupt | Yes | **No** | Control flow |
| `createresourcecategory` | Create new resource category | Yes | **No** | Resource system extension |
| `setbbox` | Set bounding box hint | Yes | **No** | Path construction hint |

---

## 2. Stubbed/Incomplete Operators

### 2.1 Pattern & Form Support (STUBBED)

Both `makepattern` and `execform` exist in stet but are **no-op stubs** that just pop arguments. PostForge has full implementations (701 lines in pattern_form.py):

- **`makepattern`**: PF creates pattern dict with PaintProc, matrix, bounding box, tiling. stet pops matrix and returns dict unchanged.
- **`execform`**: PF renders Form XObjects with caching. stet just pops the dict.
- **`setpattern`**: PF sets pattern color space. stet doesn't have this operator at all.

**Impact**: 6 failures in pattern_form_tests.ps. Any PS file using tiled patterns or Form XObjects won't render correctly.

### 2.2 Encode Filters (ALL STUBBED)

stet's filter operator handles encode filter names but returns the source file unchanged (pass-through):

| Filter | PF Status | stet Status |
|--------|-----------|-------------|
| `ASCIIHexEncode` | Full impl | **Stub (pass-through)** |
| `ASCII85Encode` | Full impl | **Stub (pass-through)** |
| `RunLengthEncode` | Full impl | **Stub (pass-through)** |
| `FlateEncode` | Full impl | **Stub (pass-through)** |
| `LZWEncode` | Full impl | **Stub (pass-through)** |
| `DCTEncode` | Full impl | **Stub (pass-through)** |
| `NullEncode` | Full impl | **Stub (pass-through)** |

**Impact**: 13 failures in filter_extended_tests.ps. Any PS program that writes encoded data (e.g., generating PS output, PDF generation, font embedding) will produce wrong results.

### 2.3 CCITTFax Filters (NOT IMPLEMENTED)

| Filter | PF Status | stet Status |
|--------|-----------|-------------|
| `CCITTFaxDecode` | Full impl (271 lines) | **Not implemented** |
| `CCITTFaxEncode` | Full impl | **Not implemented** |

**Impact**: CCITT Group 3/4 fax compression is used in some scanned document PS files and older fax-originated documents.

---

## 3. Missing Core Features

### 3.1 Binary Object Sequences / Binary Tokens (NOT IMPLEMENTED)

PostForge has a full binary token parser (718 lines in `binary_token.py`). stet has no binary token support.

Binary Object Sequences (BOS) are a compact binary representation of PS objects defined in PLRM Section 3.14. They are used by:
- Some RIP workflows
- NeXT-generated PS files
- Binary-encoded PS programs for performance

**Impact**: 55 failures in binary_token_tests.ps. Files using `\x92` and other binary token prefixes won't parse.

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

### 3.6 System Font Discovery (NOT IMPLEMENTED)

PostForge has system font loading from OS font directories (system_font_loader.py: 550 lines, system_font_cache.py: 463 lines). stet only loads fonts from its bundled `resources/Font/` directory.

**Impact**: PS files referencing fonts not in the bundled URW set will fail to find them even if installed on the system. Note: system font discovery is only applicable to native CLI/viewer builds — the WASM viewer runs in a browser sandbox with no filesystem access and will always be limited to bundled fonts.

---

## 4. Bugs — RESOLVED

All P0 bugs have been fixed. The following were fixed on 2026-03-05:

- **image PANIC** — u32 overflow in `width * height` (5 locations cast to usize before multiply)
- **image validation** — zero-dimension check, short string source check, dict-form ImageType/Width/Height strict validation
- **dict put** — now rejects array/packedarray/dict keys with TypeCheck
- **bind** — now raises TypeCheck for non-procedure operands (literal arrays still no-op)
- **realtime** — changed from epoch millis (overflows i32) to elapsed time since interpreter start

### 4.1 misc (5 failures — feature gap only)
- `echo` operator not implemented (5 failures, lines 47-55) — **feature gap, not bug**

### 4.2 ps (16 failures)
- Meta-runner that executes all other test suites; its failures are the sum of file(11) + misc(5) — not independent bugs

### 4.3 Suites Failing Due to Feature Gaps (NOT bugs)

These suites fail entirely because of unimplemented features, not defects:

| Suite | Failures | Missing Feature |
|-------|----------|----------------|
| binary_token | 55 | Binary Object Sequences (P1) |
| filter_extended | 13 | Encode filters (P1) |
| userpath | 12 | Userpath operators (P1) |
| file | 11 | All failures are BOS-related — bos_rt.bin etc. (P1) |
| dct_filter | 9 | DCTEncode parameter validation (P1) |
| pattern_form | 6 | Pattern/Form rendering stubs (P1) |

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

Font support is at feature parity for correctness. Gaps are performance (caching) and discovery (system fonts).

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
| Pattern | Full | **Stubbed** | makepattern/setpattern are stubs |
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
| ASCIIHex | Full | Full | Full | **Stub** |
| ASCII85 | Full | Full | Full | **Stub** |
| RunLength | Full | Full | Full | **Stub** |
| Flate | Full | Full | Full | **Stub** |
| LZW | Full | Full | Full | **Stub** |
| DCT | Full | Full | Full | **Stub** |
| CCITTFax | Full | **Missing** | Full | **Missing** |
| SubFile | Full | Full | N/A | N/A |
| NullEncode | N/A | N/A | Full | **Stub** |
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
1. **Pattern/Form rendering** (makepattern, setpattern, execform) — 701 lines in PF
2. **Encode filters** (ASCIIHex, ASCII85, RunLength, Flate, LZW, DCT, NullEncode)
3. **Binary Object Sequences** — needed for some workflows
4. **Userpath operators** (11 operators) — used in some PS programs
5. **DCTEncode parameter validation** — validate params even though encode is a stub
6. **Missing `echo` operator** — trivial but needed for test suite

### P2 — Medium (Feature completeness)
11. **CCITTFax filters** — used in scanned documents
12. **System font discovery** — find system-installed fonts
13. **Missing misc operators** (flushpage, runlibfile, loopname, help, printostack, breaki, createresourcecategory, setbbox)
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
loopname          printostack       runlibfile        setpattern
createresourcecategory               setbbox
uappend           ucache            ueofill           ufill
upath             ustroke           ustrokepath
inufill           inueofill         inustroke
```

### Operators in stet but NOT in PostForge

```
cleardictstack    letter            legal             pstack (native)
selectfont (native)  .showpage_continue  .copypage_continue
.error            .loadfont
```

### Operators in both — implemented identically (feature parity)

All standard PLRM operators for: stack, math, relational/boolean/bitwise, type/conversion, dictionary, control flow, composite, array, string, file I/O, filter (decode), VM, matrix, path construction, color, graphics state, painting, clipping, path query, insideness (infill/ineofill/instroke), font management, text show, halftone/transfer, shading, image, resource, interpreter parameters, device setup, CFF.

---

## Appendix B: Test Failure Details

Results from stet's own unit_tests directory (`~/Projects/stet/unit_tests/`).

| Test Suite | Failures | Root Cause Category |
|-----------|----------|-------------------|
| binary_token | 55 | Missing feature (BOS) |
| ps | 16 | Sum of file + misc failures |
| filter_extended | 13 | Missing encode filters |
| userpath | 12 | Missing operators |
| file | 11 | File I/O edge cases (BOS) |
| dct_filter | 9 | DCT param handling |
| pattern_form | 6 | Stubbed operators |
| misc | 5 | Missing `echo` operator |
| ~~dictionary~~ | ~~0~~ | ~~Fixed (2026-03-05)~~ |
| ~~image~~ | ~~0~~ | ~~Fixed (2026-03-05)~~ |
| **Total** | **~127** | |
