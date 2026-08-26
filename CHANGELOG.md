# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

Five unbounded-recursion vectors in the PDF reader let a small crafted file
abort the process with a native stack overflow. A stack overflow is not a
panic, so none of these could be contained by `catch_unwind` — any program
rendering untrusted PDFs was exposed to an uncatchable denial of service.
This is the same vulnerability class as RUSTSEC-2026-0187 in `lopdf`.

Three were depth-based, and are now capped:

- **Nested arrays and dictionaries in an object body** (`lexer.rs`).
  `parse_object_from_token` and `parse_dict_body` are mutually recursive with
  no bound, so `[[[[…` or `<</A<</A…` in any object exhausted the stack. Both
  now thread a depth counter and stop at `MAX_OBJECT_DEPTH` (256), returning
  the new `PdfError::NestingTooDeep`. The existing `parse_object`,
  `parse_object_from_token`, and `parse_dict_body` signatures are unchanged
  and enter at depth 0; `parse_object_at_depth`,
  `parse_object_from_token_at_depth`, and `parse_dict_body_at_depth` are new.
- **Nested arrays in a content stream** (`content/mod.rs`). Content-stream
  operands go through a separate parser, `parse_inline_array`, which needed
  its own cap; it shares `MAX_OBJECT_DEPTH`.
- **Nested procedures in a Type 4 (PostScript calculator) function**
  (`resources/function.rs`). `parse_token_sequence` recurses once per `{`
  body; now capped at `MAX_CALC_DEPTH` (64).

Two were cycle-based, which no depth cap alone can fix — the recursion is
infinite, so file size is irrelevant (both reproduce in under 1 KB):

- **A Type 3 stitching function that reaches itself through `/Functions`**
  (`resources/function.rs`), directly or through a ring of siblings.
  `PdfFunction::parse` now carries a set of the object numbers on the current
  path and raises `PdfError::CircularReference` on re-entry. It is a path set,
  not a seen-set — entries are popped on the way out, so the legitimate shape
  `/Functions [7 0 R 7 0 R]` still parses and renders.
- **A Type 3 CharProc that shows its own glyph** (`content/mod.rs`), directly
  or through a pair of fonts naming each other. This path incremented the
  interpreter's `depth` field but never tested it: the only check lived in
  `handle_form_xobject`. Type 3 glyphs and soft-mask groups — which likewise
  re-enter `interpret_stream` without passing through the Form XObject path —
  now check it too. The bound, `MAX_CONTENT_NESTING`, is 20, the value the
  Form XObject and pattern guards already used, so nothing that renders today
  changes.

Added `PdfError::NestingTooDeep`. `PdfError` is `#[non_exhaustive]`, so this
is not a breaking change.

Separately, image dictionary integers are now validated before use.

- **`/Width` and `/Height` were cast with `as u32` and then multiplied in
  `u32`.** The product overflowed: 65537 x 65536 is `2^32 + 65536`, so
  `width * height` came back as 65536 and the buffer allocated from it was far
  smaller than the loops that filled it — an "attempt to multiply with
  overflow" panic in debug builds, a silently undersized allocation in
  release. The truncating cast was wrong on its own too: `/Width 4294967297`
  became a 1-pixel image rather than an error.
- **The loop counts alone were a denial of service.** Even where the
  arithmetic survived, an 800-byte file declaring a 65537 x 65536 image spent
  9-19 seconds in release. It now completes in 0.1 s.
- Both are fixed by validating at the four points where an image dictionary is
  read (image XObject, inline image, `/SMask`, `/Mask`): dimensions must be
  positive and at most `MAX_IMAGE_DIMENSION` (100,000), and their product at
  most `MAX_IMAGE_PIXELS` (400,000,000). The largest image in the sample
  corpus is 34862 x 4332 (151,022,184 pixels), so the caps sit roughly 3x and
  2.6x above anything real; all 691 sample files were checked and none has an
  image rejected by them.
- **`/BitsPerComponent` is validated too**, to 1..=16. It reaches
  `1u32 << bpc` in `expand_bits_to_bytes`, which panics in debug builds at 32
  or more. That function now also reserves its three-way
  `width * height * components` product in `usize`, which overflows a `u32`
  sooner than the two-way one does.

Filter and font parameters are now validated the same way.

- **`/Columns`, `/Colors`, and `/BitsPerComponent` in `/DecodeParms`** were
  cast straight to `usize` and multiplied. A zero in any of them drove
  `row_bytes` to zero and reached `slice::chunks(0)` — "chunk size must be
  non-zero", which panics in **release** builds, not only debug. A negative
  became astronomical under the cast and aborted the process on a 2.3-exabyte
  reservation. Both are now range-checked with the row-size products computed
  via `checked_mul`; a malformed `/DecodeParms` leaves the stream unchanged
  rather than failing it, which is what the caller would have had if
  `/Predictor` were absent.
- **PS CIDFont header counts** (`/CIDCount`, `/SubrCount`, `/FDBytes`,
  `/GDBytes`, `/SDBytes`). `/SubrCount` was passed to `Vec::with_capacity`
  *before* the bounds check that would have rejected it, so a bogus count
  panicked with "capacity overflow" in release as well as debug. Separately,
  `FDBytes + GDBytes == 0` made the CID map size zero for any `/CIDCount`, so
  the "binary data too short" check passed and an 8 TB reservation followed
  from a 700-byte file. Counts are now bounded against the binary segment
  actually present rather than against a fixed ceiling, the byte-widths are
  capped at 8, and the reservation happens after the check.

Neither bound rejects anything real: all 691 sample PDFs were re-rendered with
the predictor fallback instrumented, and none takes it.

The font parsers in `stet-fonts` got the same treatment. Font programs arrive
embedded in both PDF and PostScript input, so these are attacker controlled in
the same way a PDF object is.

- **TrueType composite glyph recursion** — a component naming its own glyph,
  directly or through a ring, recursed until the stack was gone. Now capped at
  depth 8 with a path set of glyph ids, popped on exit so a font that
  legitimately reuses one accent twice still renders both copies. A depth cap
  alone is not sufficient here: a composite naming many components, each itself
  such a composite, repeats no id on any path, and the work is
  `fan_out ^ depth` — 64 components at depth 8 is 2.8e14 expansions from a
  400-byte glyph. A shared expansion budget (4096) bounds the total work.
- **Type 1 `seac`** re-entered through `execute()`, which restarts the
  subroutine depth counter at 0, so the existing depth-10 guard never fired on
  a `seac` naming its own glyph. The depth is now threaded through.
- **`/Subrs N`** reserved `N` entries before reading any of them;
  `/Subrs 999999999` panicked with "capacity overflow" (≈24 GB in release).
  Clamped to the bytes remaining after the marker, since each entry needs at
  least a `dup i n RD ` introducer.
- **cmap format 12** walked `for code in start..=end` over raw u32 — 4.3
  billion iterations for a full-range group — and computed
  `start_gid + (code - start_char)` as an unchecked u32 add. The span is now
  clamped to 0xFFFF (past which no glyph id can land in the 16-bit range
  anyway, so nothing mappable is lost) and the add is checked.
- **Type 2 `callsubr` / `callgsubr`** computed `idx + bias` as an unchecked
  i32 add. The number encodings top out at 32767, but Type 2 implements `add`,
  `sub`, `mul`, and `div`, so a charstring can multiply past `i32::MAX`, where
  the `as i32` cast saturates and the bias add overflows. Now `checked_add`.
- **`read_u16` / `read_i16` / `read_u32`** are now internally bounds-checked,
  returning 0 past the end of the slice. No caller changes: the ~40 call sites
  already pre-check (confirmed by probing every truncation of a synthetic font
  and 408 mutations of its offset and count fields, with zero panics), but the
  invariant was manual and unenforced.

All 691 sample PDFs render byte-identically before and after these font
changes.

### Added

- **`[profile.hardened]`** — release codegen with `overflow-checks` and
  `debug-assertions` left on, for finding silent arithmetic wraps at release
  speed. Build with `cargo build --profile hardened`. It is a testing profile,
  not a shipping one: published binaries stay on `release`, since a trapped
  overflow is a panic and that is not what a renderer should do to a user over
  a malformed file. Gated in CI by a new `Overflow checks` job. The vendored
  `stet-tiny-skia` forks are excluded per-package — their SIMD-lane emulation
  is modular arithmetic by definition, matching the hardware instructions the
  aarch64 paths use.

### Fixed

- **Octal escapes in PDF literal strings** (`\ddd`) accumulated into a `u8`,
  so a three-digit escape above `\377` overflowed the accumulator. The
  rendered byte was already correct — PDF 32000-1 7.3.4.2 specifies that
  high-order overflow is ignored, which is what the release build's silent
  wrap produced — but the arithmetic was wrong and panicked under overflow
  checks. Found by sweeping the sample corpus under the new `hardened`
  profile (`pdf_samples/142.pdf`).
- Shading color-stop sampling now sorts with `f64::total_cmp` instead of
  `partial_cmp().unwrap()`, and clamps its own sample count so the divisor in
  `i / (n - 1)` cannot be zero. No crafted file was found that reaches either
  path — the discontinuity filter excludes NaN and callers already clamp the
  count — so this is hardening against a future caller, not a live fix.

## [0.5.0] — 2026-08-25

Minor release. PostScript integers are now 64-bit, which fixes the standard
LCG idiom that programs use for pseudo-randomness and is the reason this is a
breaking release rather than a patch. Three Type 3 font defects and a
`clippath` coordinate-space bug are also fixed, and the CLI gains `--page`.

### Breaking

Downstream Rust code that reads PostScript integers needs attention; nothing
in the PostScript language surface changed incompatibly.

- **`PsValue::Int` now carries `i64` instead of `i32`.** A
  `match obj.value { PsValue::Int(v) => … }` binds an `i64`, so any use site
  that needs an `i32` no longer compiles. `DictKey::Int` and `Token::Int`
  widened with it, as did `Context::rand_seed`.
- **`PsObject::as_i32()` now range-checks.** It returns `None` for a value
  outside `i32`, where before it always returned `Some` for an integer. This
  is the one change with no compiler error behind it — audit call sites that
  treat `None` as "not an integer". Use the new `as_i64()` for the full
  range; keep `as_i32()` where the value is genuinely bounded (array and
  string indices, character codes), since a too-large value should fail those
  callers' range checks rather than wrap into a valid-looking index.
- **`PsObject::int()` takes `impl Into<i64>`.** Calls are unaffected; only
  code coercing it to a `fn(i32) -> PsObject` pointer breaks.

### Fixed

- **Type 3 fonts supplying only `BuildGlyph` raised `invalidfont`.** The show
  path required `BuildChar` unconditionally and pushed the character code.
  PLRM 5.7 lists `BuildGlyph` as preferred and makes `BuildChar` required only
  "for LanguageLevel 1 or if `BuildGlyph` is absent", so such a font is
  well-formed and must be handed the character *name* from `Encoding`.
  Ghostscript renders these; stet refused them. `xshow`/`yshow`/`xyshow` had
  the identical defect.
- **`stringwidth` raised `invalidfont` on every Type 3 font**, `BuildChar`
  ones included. It branched for font types 2, 0 and 42 and then fell through
  to the Type 1 path, which looks for `CharStrings` — a Type 3 font has none.
  There is no width table to consult: the width is whatever the build
  procedure hands `setcachedevice`/`setcharwidth`, so the procedure now runs
  inside a `gsave`/`grestore` with its marks drained, and measuring paints
  nothing.
- **`glyphshow` raised `invalidfont` on every Type 3 font.** It read
  `FontType` but never branched on 3, going straight to the `CharStrings`
  lookup. Per the PLRM it now invokes `BuildGlyph` with the name directly —
  bypassing `Encoding`, which is what lets `glyphshow` reach glyphs no
  character code maps to — or, with only `BuildChar`, reverse-searches
  `Encoding` for the name and pushes the array index, retrying with
  `/.notdef` and raising `invalidfont` only when neither is encoded.
- **PostScript integers are now 64-bit**, matching Ghostscript, which fixes the
  standard LCG idiom PostScript programs use for pseudo-randomness:
  `/seed seed 1103515245 mul 12345 add 2147483648 mod def`. On 32-bit integers
  the product overflowed, promoted to a real, and `mod` — which is
  integer-only — raised `typecheck`. Widening only the real fallback would not
  have fixed it: the product needs 55 bits and a real carries 53, so the seed
  would have come out one too high and every later draw would have diverged
  silently from what other interpreters produce. PLRM Appendix B's 32-bit
  range is listed under "Typical Limits" for interpreters "running on 32-bit
  machines" which "do not necessarily apply to all PostScript
  implementations", so this is not a conformance change. Overflow past the
  64-bit range still promotes to a real. `bitshift` is correspondingly 64-bit
  wide, and `cvi` accepts the wider range.
- **`clippath` returned the page in device space instead of current user
  space**, so a program that had transformed its coordinate system got a clip
  rectangle dragged along with the transform. The `clippath fill` idiom for
  painting a background then filled an offset region and left part of the page
  bare — visible in the tiger EPS, whose grey backdrop was displaced by its
  `%%BoundingBox` origin. The default clip is a fixed region of the device, so
  it is now derived with the default CTM; `pathbbox` and `fill` map it back
  through the current CTM, which is what puts it in user space for the caller.
  Ghostscript's values now match exactly under translate, scale and rotate.
- **`cvi` on a long integer-valued string was off by one.** The string scanner
  returned `f64`, so `(22358003463039195) cvi` came back as `...196` after the
  round trip through a 53-bit mantissa. Integer literals now stay integral.

### Added

- **The CLI reports a page that was painted but never shown.** A program that
  paints marks and then ends without a matching `showpage` leaves them on a
  page the device is never asked to emit, and the page is discarded. That is
  correct — it is what the PLRM specifies and what Ghostscript's file devices
  do — but it was indistinguishable from a broken renderer: no file appeared
  and nothing said why. The warning distinguishes a program that produced no
  output at all from one that lost only its trailing page.
- **`Interpreter::warnings()`** surfaces the same diagnostic to library
  callers, where the silence was worse: `render()` returned `Ok(vec![])`, an
  empty page list that reads as a legitimate result. New public types
  `ExecWarning` and `ExecWarningKind` in `stet::diagnostics`; the CLI shares
  the detector, so the two cannot drift. Programs that install `nulldevice`
  are exempt — that is the PLRM-sanctioned way to ask for no output, so marks
  left unemitted are the point rather than a mistake.
- **`--page` sets the page size for PostScript/EPS input** — a named size
  (`letter`, `legal`, `tabloid`, `ledger`, `executive`, `a0`-`a6`, `b4`, `b5`)
  or `WIDTHxHEIGHT` in points, with an optional `-landscape` / `-portrait`
  suffix that swaps the dimensions. There was previously no way to render a
  plain `%!PS` program whose artwork is larger than the default page:
  `%%BoundingBox` sets the page only for EPS — for a non-EPS document DSC
  makes it a description of the artwork's extent, not a page-size request, so
  both stet and Ghostscript fall back to US Letter and clip. `--page`
  overrides an EPS `%%BoundingBox` when both apply, and is rejected for PDF
  input, whose pages carry their own size.
- `GlyphCache::by_type3_name`, a name-keyed Type 3 glyph cache. `glyphshow`
  can name a glyph that no character code maps to, which leaves nothing for
  the existing code-keyed cache to key on.

### WebAssembly

- **`stet-wasm` 0.2.0.** Its JavaScript API is unchanged, but the browser
  build inherits everything above, so rendering output moves: `clippath`
  backgrounds fill the page, Type 3 fonts that previously raised
  `invalidfont` render, and PostScript programs using the standard LCG for
  pseudo-randomness run instead of failing. A minor rather than a patch
  because the pixels change, not because anything you call does.

## [0.4.1] — 2026-08-15

Patch release. Two `currentsystemparams` values were wrong in every release
up to and including 0.4.0, and PDFs now record which build wrote them. No API
changes; no rendering changes.

### Fixed

- **`/PrinterName` returned `(stetIE)` instead of `(stet)`.** The string was
  allocated with the four bytes of `stet` but declared six bytes long, so
  reading it ran two bytes into the next allocation — which happened to be
  `/RealFormat`. Not memory-unsafe (the arena is a single buffer), but it put
  a neighbouring allocation's bytes into a value any PostScript program can
  read, and the value would have changed as soon as allocation order did.
- **`/RealFormat` returned `(IEE)` instead of `(IEEE)`** — a missing `E` in
  the literal, independent of the overrun above. The PLRM specifies this key
  as naming the internal real representation, and Ghostscript reports
  `(IEEE)`.

  Both lengths are now derived from the literal rather than written out
  twice, which is what allowed them to disagree. The other three
  allocate-then-declare sites in `Context::new` were audited and are correct.
  Regression coverage in `unit_tests/interpreter_param_tests.ps` asserts
  lengths as well as contents — the contents alone read plausibly, and it was
  the overrun that made them wrong.

### Changed

- **PDF `/Producer` now carries the version**, e.g. `stet 0.4.1`, where it
  previously wrote a bare `stet`. Every other producer does this —
  Ghostscript writes `GPL Ghostscript 10.05.1`, Distiller
  `Acrobat Distiller 20.0` — and it is the first thing checked when a
  prepress shop is chasing a rendering difference between two files. A
  `pdfmark` `/DOCINFO /Producer` override still takes precedence; this
  changes only the default. Note that this alters bytes in the `/Info` dict
  of every PDF stet writes, at every release.
- **Documented MSRV corrected to Rust 1.88.** The README badge had claimed
  1.85 since it was added — a number inferred from `edition = "2024"` and
  never compiled against. The real floor is 1.88: first-party code uses
  let-chains in 282 places across nine crates, `jpeg-encoder` declares 1.87,
  and `fearless_simd` declares 1.86. Nothing about what stet requires has
  changed; only the claim is now true. `rust-version = "1.88"` is declared in
  `[workspace.package]` and inherited by all eleven first-party crates, so
  cargo now reports a clear "requires rustc 1.88" instead of failing with a
  confusing edition parse error on an older toolchain.
- A pinned `MSRV 1.88` CI job builds the workspace on exactly that toolchain
  on every push, and `scripts/check-release-versions.sh` now ties the README
  badge, the README prose, and the CI job's pin to `rust-version` so the four
  cannot drift apart. The script also asserts every publishable crate
  declares an MSRV, so none can reach crates.io without one.
- **Switched to `resolver = "3"`** (MSRV-aware dependency resolution). Cargo
  now prefers dependency versions compatible with the declared
  `rust-version` rather than always taking the newest, so a routine
  `cargo update` can no longer silently break the floor. The lockfile was
  byte-identical on adoption, but this is already doing work: it holds back
  `hayro-jpeg2000` 0.4.0 (needs 1.92) and `moxcms` 0.9.0 (needs 1.89).

### Note for downstream users

Releases up to and including 0.4.0 published with no `rust-version` in their
manifests, so crates.io and docs.rs show no MSRV for them and cargo cannot
warn an old toolchain before it fails to compile. Published versions are
immutable; this release is the first to carry the metadata.

## [0.4.0] — 2026-08-14

Minor release focused on **memory and PostScript conformance**. `restore`
now reclaims local VM instead of only reverting values, several
long-standing PLRM 3.7.2/3.7.3 violations in the interpreter's own writes
are fixed, and a 6384-file PostScript corpus sweep drove the job-abort
count from 1158 to 147 — of which 116 fail identically in Ghostscript,
leaving 31 that are genuinely ours.

This is a `0.x` minor bump. No public API was removed; `Context` gained
fields, which is source-breaking only for code constructing one
literally (it has no public constructor other than `Context::new`).

### Highlights

- **`restore` reclaims local VM.** Allocations made above a `save`'s
  high-water mark are released rather than left resident. A
  save/restore loop that peaked at 2108 MB now peaks at 74 MB.
- **`restore` actually reverts what it is supposed to.** Several
  interpreter-internal writes bypassed copy-on-write, so `restore` had
  no backup to revert to: `defineresource`, `FontDirectory`, `reverse`,
  `execstack` and `dictstack`. This was a live PLRM 3.7.3 violation, not
  a theoretical one.
- **Global/local VM enforcement is no longer silently disabled.**
  `.error` did not restore `setglobal`, so any caught error left the
  interpreter in whatever VM mode the failing code had set.
- **Eleven interpreter defects found by a 6384-file corpus sweep**, each
  A/B'd against the previous sweep with no regressions: procedure data
  sources, the Pattern colour space, `bind` on nested procedures,
  array-form colour spaces, `cvi`/`cvr` string conversion, CIDFontType 0
  `StartData`, 16-bit image samples, EOI-less JPEG, `shareddict`/`scheck`,
  self-registering resource files, `rectclip`, `cshow`, and `charpath` on
  Type 3 fonts.

### Added

- `charpath` support for Type 3 fonts: the glyph procedure runs without
  marking the page and the paths it would have painted become part of
  the current path.
- The `Pattern` colour space — `setcolorspace`/`setcolor`/`currentcolor`
  with a pattern, including the uncoloured (PaintType 2) base-space form.
- `shareddict` and `scheck` in `systemdict`.
- 16 bits per component for `image` / `imagemask` / `colorimage`.
- `stet_core::vm_audit` and `--example audit_vm`: a machine check for
  dangling references and PLRM 3.7.2 global/local violations.
- PostScript corpus build and sweep tooling under `scripts/`.

### Fixed

- `restore` now releases local VM allocated above the save mark, and
  copy-on-writes the dictionaries and arrays it is required to revert.
- Every allocation is stamped with its save level and VM mode, so
  `restore` can tell a surviving reference from a dangling one and raise
  `invalidrestore` when PLRM requires it.
- `.error` restores the VM allocation mode; page-device arrays are
  allocated in the page device's own VM and deep-copied on promotion.
- `filter` accepts procedure data sources everywhere, runs them when the
  data is read rather than at `filter` time, and honours SubFileDecode's
  EOD semantics.
- `bind` marks nested procedures read-only per PLRM, and terminates on
  cyclic procedure graphs.
- `setcolorspace` accepts array-form base and alternate colour spaces.
- `cvi` and `cvr` convert strings through the scanner, as PLRM specifies.
- `CIDInit`'s `StartData` consumes its charstring blob instead of
  leaving it to be scanned as tokens.
- DCTDecode accepts a JPEG stream that ends before its EOI marker.
- Resource files that register themselves are loaded once, through a
  shared `.LoadResource`, so `composefont` can find a CMap on disk.
- `rectclip` takes every rectangle in a multi-rectangle argument, and
  accepts an empty array.
- `cshow` hands its procedure the character code, not the CID.
- CIE decode tables are memoised, and 8-bit image samples are no longer
  copied in `unpack_samples` — together these took the corpus from 24
  out-of-memory jobs to none.

## [0.3.0] — 2026-08-12

Minor release adding **PDF→PDF round-trip** through the PDF output
device. A PDF parsed by `stet-pdf-reader` into the display list can now
be re-emitted as PDF with its prepress semantics preserved — spot
(Separation/DeviceN) colors, ICCBased spaces, overprint, soft masks,
transparency groups, optional-content layers, and `/OutputIntents` all
survive the round-trip rather than collapsing to flat process color.

This is a `0.x` minor bump. The document-structure IR moved from
`stet-core` into `stet-graphics` (still re-exported by `stet-core`), so
code that reaches those types through `stet-core` is unaffected.

### Highlights

- **PDF→PDF round-trip** — `--device pdf` on a PDF input now routes
  through `PdfDocument` → `PdfDevice`, so a PDF can be read to the
  display list and written back out as PDF (previously only PS/EPS
  input reached `PdfDevice`).
- **Prepress color preserved** — Separation/DeviceN spot colors (and
  their base spaces, with DeviceGray promotion), ICCBased fill/stroke
  spaces, and ICCBased bases inside Indexed image spaces all round-trip;
  `/Catalog /OutputIntents` is carried through so the CMYK-driving ICC
  profile is retained.
- **Overprint preserved** — `/OP`/`/op` forced on the first paint of
  each content stream, `/OPM` carried through the display list, and
  overprint state emitted for Image and Shading paints.
- **Transparency & layers emitted from the display list** —
  `DisplayElement::Group` as a Form XObject, `SoftMasked` with per-paint
  alpha/blend, and `DisplayElement::OcgGroup` with `/OCProperties`
  optional-content groups.

### Added

- PDF output: Separation/DeviceN spot color + base round-trip,
  Separation/DeviceN shadings and spot imagemasks, and CMYK imagemask
  fill preservation.
- PDF output: ICCBased fill/stroke color-space round-trip and ICCBased
  base preservation inside Indexed image color spaces.
- PDF output: `/Catalog /OutputIntents` round-trip through PDF→PDF.
- PDF output: `Group` → Form XObject, `SoftMasked` + per-paint
  alpha/blend, `OcgGroup` → `/OCProperties`, per-paint alpha on the
  Image and Shading writer arms, overprint state on Image/Shading, and
  `/OPM` round-trip.

### Changed

- Document-structure IR lifted from `stet-core` into `stet-graphics`
  (re-exported by `stet-core`).
- PDF writer: graphics-state tracker restored across `q`/`Q`
  boundaries; replayed clips collapsed so a round-trip no longer grows
  the display list; implicit page-box clip skipped on PDF→PDF.
- `stet-pdf-reader`: spot tint-transform table cached per content
  stream.
- README: added a Commercial Support section.

### Fixed

- Removed a dead `emit_fill_color_rgb` helper, superseded by the
  DeviceColor-aware imagemask fill path (cleared a `dead_code` warning).

### Crates published at 0.3.0

`stet`, `stet-cli`, `stet-fonts`, `stet-graphics`, `stet-core`,
`stet-ops`, `stet-engine`, `stet-render`, `stet-viewer`,
`stet-pdf-reader`, `stet-pdf`. The vendored `stet-tiny-skia` /
`stet-tiny-skia-path` forks remain at `0.11.4`. `stet-wasm` remains at
`0.1.1` (excluded from crates.io, independent cadence).

## [0.2.1] — 2026-05-09

Patch release focused on PDF/X CMYK rendering correctness against the
[Ghent PDF Output Suite](https://gwg.org/pdf-output-suite/) (GWG)
test corpus. Fixes a family of bugs where ICCBased / Lab / DeviceN
fills, images, and transparency groups didn't round-trip through the
document's `/OutputIntents` profile correctly, producing visible "X"
markers in calibration swatches that should render uniform.

This is an **additive, non-breaking** release. Cargo will auto-bump
`stet = "0.2"` to `0.2.1`; downstream code does not need to change.
New public API on `stet-graphics::IccCache` and a new
`rendering_intent: u8` field on `stet-graphics::ImageParams` are
documented under "Added" below.

### Highlights

- **GWG 13.3** — ICCBased RGB paints with `/OP true` no longer route
  into the custom-spot overprint path; per PDF 1.7 §11.7.4.5 they
  paint as if `/OP` were false.
- **GWG 16.1** — per-intent PDF/X proofing chain (`source A2B → PCS
  → OI B2A → CMYK`) is built for every registered ICCBased RGB
  profile, threaded through `op_ri` / ExtGState `/RI`.
- **GWG 16.4** — transparency groups with no `/CS` (inherit) now
  resolve correctly to the parent's CMYK compositing space when
  the parent is a `/CS DeviceCMYK` group.
- **GWG 17.2** — ICCBased images now go through the proofing chain
  via `convert_image_8bit_with_intent` (was bypassing the
  OutputIntent roundtrip and rendering via direct source→sRGB).
- **GWG 22.1** — Lab fills populate `DeviceColor::native_cmyk` via a
  direct `Lab → PCS → OI B2A → CMYK` chain (matches Adobe ACE),
  and the OutputIntent install path pre-warms the sRGB→CMYK
  reverse transform so the parallel CMYK buffer never falls back
  to the PLRM `(1−r, 1−g, 1−b, 0)` formula.
- **WASM viewer** — `open_pdf` now applies the document's
  OutputIntent before storing the cached state, so PDF/X documents
  render in the browser the same way they do in the CLI.

### Added — public API (additive, non-breaking)

`stet-graphics`:

- `IccCache::convert_to_oi_cmyk(hash, components, intent)` — run an
  RGB ICC color through the proofing chain at the given intent and
  return the intermediate OutputIntent CMYK.
- `IccCache::convert_lab_to_oi_cmyk(l, a, b, intent)` — direct
  `Lab → OI CMYK` via the OI's per-intent B2A LUT.
- `IccCache::convert_image_8bit_with_intent(hash, samples,
  pixel_count, intent)` — bulk image conversion with explicit
  rendering intent.
- `IccCache::convert_color_with_intent` and
  `convert_color_readonly_with_intent` — per-intent single-color
  conversion.
- `IccCache::prepare_lab_to_oi_cmyk()` — pre-build per-intent
  Lab→OI samplers; pair with `prepare_reverse_cmyk()`.
- `IccCache::intent_from_pdf_byte(b: u8)` — map PDF rendering-intent
  bytes (`0..3`) to `IccRenderingIntent`.
- `pub use moxcms::RenderingIntent as IccRenderingIntent`.
- `pub struct LabToCmykSampler` (in `icc::perceptual`) with
  `pub fn sample_pdf_lab(l, a, b)`.
- New field `ImageParams::rendering_intent: u8`. Default is `0`
  (Perceptual). Per the documented "be a reader, not a writer"
  policy for param structs (CLAUDE.md), this is additive and not
  treated as a SemVer break.

`stet-pdf-reader`:

- `PdfDocument::apply_output_intent_as_default_cmyk()` now also
  pre-warms the sRGB→CMYK reverse and per-intent Lab→OI samplers
  in addition to its previous behaviour. No signature change.
- Image XObjects with `/Intent` now propagate the per-image
  rendering intent into `ImageParams.rendering_intent`, overriding
  the gstate `/RI` per ISO 32000 §11.3.4.

`stet-render`:

- `build_icc_cache_for_list` now also pre-warms the per-intent
  Lab→OI samplers when proofing is enabled.

### Fixed

- DeviceGray painted in a PDF/X DeviceCMYK page group now routes
  through the K plate (matches DeviceCMYK 0/0/0/(1−g) byte-for-byte).
- DeviceN images with a non-CMYK alternate space go through the
  overprint path so process plates aren't disturbed.
- Paired `/OP true /op true` ExtGStates are now treated as a
  "strict overprint" signal (matches Adobe Illustrator's emit).
- The custom-spot overprint dispatch and the parallel CMYK buffer's
  `is_custom_spot` heuristic both now require
  `process_cmyk.is_some()` so proofing-chain ICCBased RGB stays out.

### Crates published at 0.2.1

`stet`, `stet-cli`, `stet-fonts`, `stet-graphics`, `stet-core`,
`stet-ops`, `stet-engine`, `stet-render`, `stet-viewer`,
`stet-pdf-reader`, `stet-pdf`. The vendored `stet-tiny-skia` /
`stet-tiny-skia-path` forks remain at `0.11.4`. `stet-wasm` is
excluded from crates.io and bumped to `0.1.1` independently.

## [0.2.0] — 2026-05-01

This release lands a substantial expansion of the `stet-pdf-reader`
structural API, the PDF imaging-extension operators (transparency,
soft masks, optional content), and the `pdfmark` PostScript-to-PDF
authoring bridge. Several public match-surface enums are now
`#[non_exhaustive]` to lock in additive evolution — the breaking
changes are deliberate and documented per-crate below.

### ⚠ Breaking changes

This is a **breaking release**. Cargo treats the `0.1 → 0.2` bump as
incompatible (per the SemVer rules for `0.x`), so existing users
pinned at `stet = "0.1"` won't be auto-upgraded.

The breaking surface is concentrated in two places:

1. **`#[non_exhaustive]` markers** were added to ~40 public
   match-surface enums across `stet-graphics`, `stet-core`, and
   `stet-pdf-reader`. Any downstream `match` over `DisplayElement`,
   `PsError`, `Destination`, `AnnotationKind`, the various pdfmark
   record enums, etc. now requires a `_ => { ... }` wildcard arm.
   See the "Changed — public API breaking changes" subsection below
   for the complete list.

2. **`stet-pdf` no longer emits PDF/X-3 OutputIntents.** PDF output
   is now plain PDF 1.7. `PdfDevice::set_output_profile()` is
   `#[deprecated]` as a no-op; existing call sites compile but stop
   producing the (previously broken) PDF/X-3 conformance label.

For a typical downstream renderer that pattern-matches on
`DisplayElement`, the migration is one wildcard arm per `match`
site:

```diff
 match element {
     DisplayElement::Fill { .. } => { /* … */ }
     DisplayElement::Stroke { .. } => { /* … */ }
     DisplayElement::Image { .. } => { /* … */ }
+    _ => { /* fall through; new variants in 0.2.x are additive */ }
 }
```

The `#[non_exhaustive]` ratchet is intentional: it makes future
variant additions non-breaking, so 0.2.x → 0.3.x will be smaller.

### Added — `stet-pdf-reader` structural API

A read-only structural-content API for PDF inspection and tooling.
Every accessor parses lazily on first call and caches its result.

- `metadata()` — `/Info` dict (title, author, dates, …) and the
  catalog's `/Metadata` XMP stream.
- `viewer_preferences()` — page layout, page mode, print preferences,
  and reading direction hints.
- `outline()` — bookmark tree as `OutlineItem`s with
  destination/action resolution.
- `destinations()`, `resolve_named_destination(name)` — named
  destination table merged from `/Catalog /Dests` (legacy) and the
  `/Names /Dests` name tree.
- `page_annotations(page)` — typed `Annotation` list with
  destination/action resolution.
- `form()`, `form_fields()` — AcroForm field tree (text, choice,
  button, signature) with widget cross-references.
- `page_boxes(page)` — MediaBox / CropBox / BleedBox / TrimBox /
  ArtBox.
- `embedded_files()`, `embedded_file_bytes(name)` — `/EmbeddedFiles`
  name-tree walker.
- `layers()`, `layer(ocg_id)`, `configurations()`,
  `default_configuration()`, `layer_tree()`, `layer_set_for(intent)`
  — Optional Content Group (OCG) metadata, hierarchy, render-intent
  rules, and a runtime `LayerSet` for visibility overrides.
- `parse_warnings()` — diagnostic sink for non-fatal parse issues
  (broken outlines, bad name trees, malformed `/VE` expressions, …).
- New `stet inspect <file.pdf>` CLI subcommand surfaces the structural
  API at the command line.

See `docs/PDF-READER-API.md` and `docs/PDF-LAYERS.md` for full
references.

### Added — PDF imaging extensions

Display-list-level support for the PDF transparency and optional-content
imaging models, layered on top of the PostScript interpreter.

- **Alpha and blend modes**: `setblendmode`, `setfillalpha`,
  `setstrokealpha`, `setalphaisshape`. All 16 PDF blend modes.
- **Transparency groups**: `begintransparencygroup` /
  `endtransparencygroup` with `Knockout`, `Isolated`, and group
  colour space (`DeviceGray` / `DeviceRGB` / `DeviceCMYK` / ICC).
- **Soft masks**: `begintransparencymaskgroup` /
  `endtransparencymaskgroup` with `Alpha` and `Luminosity` subtypes,
  transfer functions, and backdrop-colour handling.
- **Optional Content (OCG)**: `setocg` / `endocg` operators wrap
  display-list content in `OcgGroup` elements with
  `OcgVisibility::Single` / `Membership` / `Expression` predicates.
  `LayerSet` (in `stet-graphics`) is the consumer's per-render override
  map; `render_page_to_rgba_with_layers` honours it.
- **Filters**: `JBIG2Decode` and `JPXDecode` for embedded image
  streams.

See `docs/PDF-EXTENSIONS.md` for the full reference and
`docs/PDF-LAYERS.md` for the runtime layer-visibility model.

### Added — `pdfmark` PostScript-to-PDF authoring

`pdfmark` operator dispatch in `stet-ops` (gated behind
`register_pdf_authoring_ops` so it's only visible to systemdict on the
PDF output path) plus matching emitters in `stet-pdf`. Five phases of
authoring support:

- `/DOCINFO` — document info dictionary (title, author, subject,
  keywords, creator, producer, dates, trapped).
- `/OUT` — outline (bookmark) tree authoring with destination /
  action targets.
- `/ANN` — Link, Text, FreeText annotations.
- `/DEST`, `/PAGE`, `/PAGES` — named destinations and per-page-box
  overrides.
- `/VIEWERPREFERENCES`, `/Metadata` — viewer preferences and
  document-level XMP metadata.
- `/Widget` and `/FORM` — AcroForm widget annotations and field-tree
  emission.
- `/EMBED`, JavaScript / Named actions, page-level `/AA` triggers.

See `docs/PDFMARK-AUTHORING.md` for the full reference.

### Added — colour management

- **Hand-rolled colorimetric A2B1 CLUT sampler**
  (`stet-graphics::icc::perceptual`). moxcms 0.8's `create_transform`
  pipeline over-saturates CMYK→sRGB output relative to lcms2 / Acrobat
  / Ghostscript on midtone colours; this module bypasses it for v2
  `lut16Type` CMYK profiles and matches lcms2 RelCol output to ±1 RGB
  level on a 17⁴ sweep against ISO Coated v2 300% (ECI). Out-of-gamut
  colours clip to the sRGB boundary (matching lcms2 / GS) so pure
  process primaries remain saturated. BPC is calibrated against the
  sampler's own (1, 1, 1, 1) output so K-heavy CMYK lands at the
  correct darkness. Profiles whose tables are mAB / mft1 fall back
  to the moxcms-driven bake.
- Soft-mask CMYK-domain blend gate widened to accept Group-wrapped
  flat CMYK fills (GWG 16.11 "Gradient Feather"). The GWG 16.10
  outer-glow protection still rejects on the inner Fill's blend-mode
  check.

### Changed — public API breaking changes

These match-surface enums are now `#[non_exhaustive]` so adding
variants is non-breaking for any consumer that includes a `_ =>` arm.
Existing consumers must add wildcard arms (or update their match
expressions) to keep building.

- `stet-graphics`: `DisplayElement`, `ImageColorSpace`,
  `ShadingColorSpace`, `SpotColorSpace`, `LineCap`, `LineJoin`,
  `FillRule`.
- `stet-core`: `PsError`, `FilterKind`, `RleState`.
- `stet-core::pdfmark`: `PdfMarkRecord`, `AnnotationSubtype`,
  `AnnotationTarget`, `OutlineDestination`, `OutlineAction`,
  `GoToTarget`, `ViewSpec`, `FieldType`, `FieldValue`, `DocDate`,
  `TrappedState`, `TzSign`, `LinkHighlight`, `TextAnnotationIcon`,
  `PageOverrideScope`.
- `stet-pdf-reader`: `PdfError`, `Destination`, `ViewSpec`, `Action`,
  `AnnotationDate`, `AnnotationKind`, `AnnotationColor`,
  `AnnotationKindData`, `FieldKind`, `ButtonType`, `FieldValue`,
  `TrappedFlag`, `PageLayout`, `PageMode`, `ReadingDirection`,
  `PrintScaling`, `Duplex`, `AfRelationship`, `ParsePhase`,
  `LocationHint`, `Severity`, `RenderIntent`, `LayerIntent`,
  `UsageState`, `PageElementSubtype`, `LayerTreeNode`, `BaseState`,
  `ListMode`, `AutoStateEvent`.

Param **structs** (`FillParams`, `StrokeParams`, `ImageParams`, the
pdfmark record structs, `Annotation`, `FormField`, `Layer`, etc.) are
**not** marked `#[non_exhaustive]` — adding fields lands additively
and consumers should pattern-match with `..` for forward
compatibility.

A `scripts/check-non-exhaustive.sh` audit runs in the local pre-push
hook; new public enums in the listed files must either carry the
marker or be allow-listed with a one-line justification. See the
"Stable extension points" section of CLAUDE.md and the per-doc
"Stability" sections of `docs/DISPLAY-LIST.md`,
`docs/PDF-READER-API.md`, and `docs/PDFMARK-AUTHORING.md`.

### Changed — other

- **`stet-pdf`**: removed the PDF/X-3 OutputIntent emission. The writer
  was emitting soft-mask transparency (prohibited by PDF/X-3) while
  labelling output as `PDF/X-3:2003` — a conformance conflict any
  preflight tool would flag. PDF output is now plain PDF 1.7 with no
  PDF/X conformance claim. A correct PDF/X-4 implementation is
  planned.
- **`stet-pdf`**: `PdfDevice::set_output_profile()` is `#[deprecated]`
  as a no-op. Retained for forward API compatibility with the planned
  PDF/X-4 work.
- **`stet-cli`**: `--width` / `--height` flags for PDF input override
  the page's MediaBox at render time.

### Added — documentation

- `docs/PDF-READER-API.md` — full reference for the structural API.
- `docs/PDF-LAYERS.md` — full reference for the OCG / layer API.
- `docs/PDF-EXTENSIONS.md` — full reference for the imaging extension
  operators and the JBIG2 / JPX filters.
- `docs/PDFMARK-AUTHORING.md` — full reference for the pdfmark
  authoring bridge.
- New **Rendering Correctness** section in the root README covering
  seam-free rendering on adjacent clipped regions and full overprint
  simulation.

## [0.1.0] — 2026-04-18

Initial public release.

### PostScript interpreter

- Level 3 interpreter with ~320 operators covering stack, math, type,
  dict, array, string, control, file, graphics state, path construction,
  painting, clipping, colour, font, show, image, halftone/transfer,
  pattern/device, resource, and param categories.
- Arena + entity-indirection memory model with full save/restore (COW).
- Dual VM (local/global) with unified stores and `vm_alloc_mode`.
- Name interning via `NameTable`; dict version cache for O(1) name
  resolution on the hot path.
- Full Type 1, CFF/Type 2, Type 3, TrueType, and Type 42 (CID) font
  support with URW substitutions for the 35 standard PostScript fonts.
- Eexec, ASCIIHex, ASCII85, RLE, Flate, LZW, DCT, SubFile, and their
  encode counterparts as streaming filters.
- CIE-based colour spaces (A/ABC/DEF/DEFG) and ICC-based via moxcms.
- Smooth shading types 1–7 (function, axial, radial, triangle meshes,
  Coons/tensor patches) with native PS function evaluation.

### PDF reader (`stet-pdf-reader`)

- Self-contained PDF parser: xref (including xref streams), decryption
  (RC4/AES), object-stream decompression, page tree, resource
  resolution.
- Content-stream interpreter producing the same `DisplayList` type the
  PostScript interpreter produces — **no dependency on `stet-core`**.
- Transparency groups, soft masks (alpha & luminosity), tiling
  patterns, shadings 1–7.
- All standard stream filters including Flate (with PNG predictors),
  LZW, DCT (two backends), CCITT, JBIG2, JPEG 2000, ASCII85, ASCIIHex.
- Optional Content Groups (OCG) captured in the display list for future
  layer toggling.
- PDF OutputIntent profile honoured by default for PDF/X documents.
- CJK CMap loading (poppler-data / `STET_CMAP_DIR`).

### Rendering (`stet-render`)

- tiny-skia–based rasterizer (vendored as `stet-tiny-skia`) producing
  RGBA output.
- Banded rendering sized to L2 cache; rayon-parallel band processing.
- Clip fast path with rect detection, mask caching, and spare mask
  recycling.
- Viewport rendering: render any rectangular region of a display list
  at any zoom without re-interpreting the source.
- ICC-aware CMYK path with black-point compensation and per-pixel
  consistency checks for transparency-group blending.
- Overprint simulation (OPM 0/1), including strict OPM-1 "preserve
  zero components" semantics.
- Hairline and stroke-adjust handling for thin lines.

### PDF output (`stet-pdf`)

- Display list → PDF with embedded fonts (Type 1, TrueType, CFF),
  image compression, shadings, and transparency groups.
- Preserves native CMYK and spot colour spaces (Separation, DeviceN)
  without lossy RGB round-tripping.
- Pre-sampled transfer, halftone, and black-generation/UCR tables
  carried per paint element.
- Print-workflow quality output suitable for pre-press.

### Viewer & frontends

- `stet-viewer`: egui desktop viewer with pan/zoom, minimap,
  multi-page navigation, and drag-and-drop.
- On-demand viewport rendering: zoom/pan without re-interpretation.
- WASM frontend (`stet-wasm`, excluded from the main workspace):
  browser-side PDF viewer with viewport rendering and SIMD-enabled
  tiny-skia.

### Public library API (`stet` facade)

- `Interpreter::new()` / `Interpreter::builder()` for batteries-included
  PostScript rendering.
- `render()` → RGBA pages, `render_to_display_list()` → display lists,
  `render_to_pdf()` → PDF bytes, `exec()` → side-effects only.
- All 53 resources (fonts, encodings, CMaps, ICC profile) embedded in
  the binary via `include_bytes!`.
- Example programs: `render_ps`, `render_pdf`, `display_list`.

### Workspace

- 13 crates under Apache-2.0 OR MIT, plus two vendored tiny-skia forks
  (`stet-tiny-skia`, `stet-tiny-skia-path`) under BSD-3-Clause.
- `stet-pdf-reader` is intentionally independent of `stet-core` — it
  can be used as a standalone PDF parser/renderer without pulling in
  the PostScript VM.

[0.5.0]: https://github.com/AndyCappDev/stet/compare/v0.4.1...v0.5.0
[0.2.0]: https://github.com/AndyCappDev/stet/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/AndyCappDev/stet/releases/tag/v0.1.0
