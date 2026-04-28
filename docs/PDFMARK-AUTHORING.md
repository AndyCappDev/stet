# pdfmark Authoring

`pdfmark` is the standard Adobe / GhostScript bridge from PostScript code
into PDF structural features — document metadata, outlines, annotations,
named destinations, viewer preferences, and so on. PostScript code issues
`pdfmark` calls during interpretation, the interpreter parks the parsed
records on a buffer hanging off the runtime context, and the PDF output
device drains that buffer at end-of-job.

This document covers the operators currently implemented. The plan in
`docs/PLAN-PDFMARK-AUTHORING.md` lists the phases still to come — outlines
(`/OUT`), annotations (`/ANN`), destinations (`/DEST`), page boxes
(`/PAGE` / `/PAGES`), viewer preferences, XMP metadata, embedded files,
and AcroForm widgets.

## Quick start

```postscript
% Set document Info entries from PostScript code.
[ /Title (Annual Report)
  /Author (Acme Corp.)
  /Subject (Q4 results)
  /Keywords (annual report, finance, Q4)
  /Creator (Acme Reporting Tool 3.2)
  /CreationDate (D:20261015093000-04'00')
  /Trapped /False
  /DOCINFO pdfmark
```

Render this file with `stet --device pdf input.ps`. The resulting PDF
has the `/Info` dictionary entries above. Render the same file with
`stet --device png input.ps` and the `pdfmark` operator is intentionally
**not** registered: `systemdict /pdfmark known` returns `false`, and any
prologue that branches on its presence takes the non-Distiller path. See
[Activation model](#activation-model).

## Operator surface

### `pdfmark`

```
mark arg1 ... argN typetag pdfmark → —
```

One operator, many type-tags. `pdfmark` scans the operand stack down to
the most recent `[` mark, reads the topmost name (the *type-tag*), and
hands the items between the mark and the type-tag to the per-type
handler. Stack effect: pops everything from the mark up through the
type-tag (inclusive); pushes nothing.

| Error class | Behaviour |
|---|---|
| No `[` mark on stack | `unmatchedmark` |
| Type-tag missing or not a name | `typecheck` |
| Unknown type-tag | Silent no-op (Adobe convention so PS code that targets newer PDF features stays runnable on older interpreters) |
| Malformed payload inside a known type-tag | Handler-specific. Most handlers ignore unrecognised entries silently; required-key failures emit a diagnostic to stderr and skip the record |

Adobe pdfmark is deliberately permissive about payload shape — `/PUT`,
`/OBJ`, `/EMBED` and other type-tags interleave positional arguments
with dict-style pairs. The `pdfmark` operator does **not** pre-validate
"alternating /Name value pairs"; that validation belongs in the per-type
handler when the shape is known.

### `currentdistillerparams`

```
— currentdistillerparams → dict
```

Returns a small dict carrying the entries production PostScript
prologues actually inspect:

| Key | Value |
|---|---|
| `/CoreDistVersion` | `4000` (Distiller 6.0 era — high enough to satisfy `>= 2000` checks in older FrameMaker prologues, low enough that newer Adobe Illustrator prologues which gate distiller-only stream injection on `< 5000` route their binary metadata streams to a path stet can actually handle) |
| `/CompatibilityLevel` | `1.7` |

### `setdistillerparams`

```
dict setdistillerparams → —
```

Adobe-Distiller-compatibility stub — validates that the operand is a
dict, then no-ops. Real Distiller stores the entries; stet doesn't yet
track the parameters or react to them.

## Type-tag reference

### `/DOCINFO` — document Info dictionary

```postscript
[ /Title (...)
  /Author (...)
  /Subject (...)
  /Keywords (...)
  /Creator (...)
  /Producer (...)
  /CreationDate (...)
  /ModDate (...)
  /Trapped /True | /False | /Unknown
  /DOCINFO pdfmark
```

Every key is optional. Recognised keys merge into the output PDF's
`/Info` dictionary; absent keys leave any device default in place. When
multiple `/DOCINFO pdfmark` blocks appear, later records override earlier
ones key-by-key (matching GhostScript's pdfwrite behaviour).

| Key | Type | PDF Info entry |
|---|---|---|
| `/Title` | string | `/Title` (overrides the device's filename-derived default) |
| `/Author` | string | `/Author` |
| `/Subject` | string | `/Subject` |
| `/Keywords` | string | `/Keywords` |
| `/Creator` | string | `/Creator` |
| `/Producer` | string | `/Producer` (overrides the device's `(stet)` default) |
| `/CreationDate` | string in PDF date format | `/CreationDate` (overrides the device's "now in UTC" default) |
| `/ModDate` | string in PDF date format | `/ModDate` |
| `/Trapped` | name `/True`, `/False`, or `/Unknown` | `/Trapped` |

Date strings are passed through verbatim. Date format per PDF spec:
`D:YYYYMMDDHHmmSSOHH'mm'` where `O` is one of `+`, `-`, or `Z` (UTC);
the parts after the year are optional.

## Activation model

Whether `pdfmark` appears in `systemdict` is a function of the active
output device. PostScript prologues in the wild routinely branch on
`systemdict /pdfmark known` to decide whether they're talking to Adobe
Distiller (or an equivalent PDF writer) versus a screen / display
device. FrameMaker 5.0 prologues for example switch from
CMYK→`setcmykcolor` to RGB→`setrgbcolor` when pdfmark is present,
because Distiller is happier with RGB. For stet's PNG / viewer / WASM
rendering paths that swap produces a brighter blue (direct RGB→sRGB)
than the document was designed for (CMYK→ICC→sRGB). For PDF output the
RGB swap is what FrameMaker intended.

So:

| Output mode | `systemdict /pdfmark known` |
|---|---|
| `--device pdf` | `true` (and `currentdistillerparams` returns the version dict above) |
| `--device png`, viewer, WASM | `false` (`pdfmark`, `currentdistillerparams`, `setdistillerparams` are all undefined) |

Internally this is the `register_pdf_authoring_ops` function in
`stet-ops`. The PDF rendering paths in `stet::Interpreter::render_to_pdf`
and the CLI's `run_pdf_mode` call it after `build_system_dict`. Screen
rendering paths leave it out.

## Buffer lifecycle

`pdfmark` records live on `Context::pdfmark_buffer`
(`stet_core::pdfmark::PdfMarkBuffer`):

- Records are **document-global**, not VM-level. `save` / `restore` do
  not roll the buffer back; pdfmark records issued before a `restore`
  survive into the post-`restore` document.
- The buffer also tracks a 1-based `current_page` counter incremented
  per `showpage`. Phase 1 doesn't consume this; later phases (annotations,
  page boxes) use it to scope records to the page being assembled when
  the `pdfmark` was issued and `/Page` was omitted.
- `PdfDevice::build_info_dict()` (in the PDF output device) reads the
  buffer at end-of-job and merges every `DocInfo` record into the
  effective `/Info` dictionary.

Non-PDF output devices simply never read the buffer, so on the screen
path pdfmark calls are pure no-ops even when the prologue has somehow
arranged for `pdfmark` to be defined (it shouldn't be — see above).

## Non-goals

- **Rendering authored annotations back to screen output.** That belongs
  to the read side (`stet-pdf-reader`'s structural API,
  `docs/PDF-READER-API.md`).
- **Implementing Adobe Distiller's full parameter set.** Only the
  entries production prologues actually inspect are returned by
  `currentdistillerparams`.
- **3D content (`/RichMedia`, U3D/PRC), movies, sound, geospatial
  annotations.** Out of the pdfmark spec or deprecated in modern PDFs.

See also:

- `docs/PLAN-PDFMARK-AUTHORING.md` — phase plan including outlines, annotations, named destinations, viewer preferences, AcroForm widgets, embedded files, JavaScript actions, and the deferred Tagged-PDF work.
- `docs/PDF-EXTENSIONS.md` — companion reference for the PDF *imaging* extensions (transparency, blend modes, soft masks, OCG layers).
- `docs/PDF-READER-API.md` — the read-side structural API; pdfmark on the write side feeds the same shapes the reader exposes (`metadata()`, `outline()`, `page_annotations()`, …).
