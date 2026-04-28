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

### `/OUT` — outline / bookmark entry

```postscript
[ /Title (Chapter 1)
  /Page 5
  /View [/XYZ 100 700 1.5]
  /Count 3
  /Color [0.0 0.0 0.5]
  /F 2
  /OUT pdfmark
```

Each `/OUT pdfmark` records one bookmark entry; the writer assembles the
flat sequence into the document's outline tree at end-of-job. Bookmarks
without a `/Title` are dropped silently.

| Key | Type | Meaning |
|---|---|---|
| `/Title` | string | Required — user-visible label |
| `/Page` | integer | 1-based page target. Combine with optional `/View` |
| `/View` | array | PDF view spec — `[/XYZ left top zoom]`, `[/Fit]`, `[/FitH top]`, `[/FitV left]`, `[/FitR left bottom right top]`, `[/FitB]`, `[/FitBH top]`, `[/FitBV left]`. `null` components mean "keep current value" |
| `/Dest` | name or string | Reference to a named destination (registered via `/DEST pdfmark` in a later phase) |
| `/Action` | dict | Action to fire on click. Currently recognised: `<< /S /URI /URI (string) >>` and `<< /S /GoTo /D <name-or-array> >>` |
| `/Count` | integer | Adobe nesting hint. Positive: this entry is *expanded* with `count` direct children that immediately follow. Negative: collapsed with `\|count\|` children. Zero / absent: leaf |
| `/OutlineLevel` | integer ≥ 1 | **stet extension.** Explicit nesting level (1 = top-level). When *any* record uses this key, the whole batch switches to level-based parenting and `/Count` is ignored for topology (the sign is still honoured for the open/closed display state). Not compatible with GhostScript pdfwrite |
| `/Color` | 3-array of `0..=1` reals | Bookmark text colour (PDF 1.4 outline `/C` entry) |
| `/F` | integer | Style flags; bit 0 = italic, bit 1 = bold (PDF 1.4 outline `/F` entry) |

`/Action` wins over `/Dest`, `/Dest` wins over `/Page`. A bookmark
without any of the three becomes a non-navigable label.

#### Authoring conventions

**Count-based** — Adobe's native shape; pdfwrite-compatible. Each parent
declares `/Count N` and the next *N* records become its direct children:

```postscript
[ /Title (Part I)  /Count 2 /Page 1 /OUT pdfmark
[ /Title (Chapter 1)         /Page 2 /OUT pdfmark
[ /Title (Chapter 2)         /Page 9 /OUT pdfmark
[ /Title (Part II) /Count 1 /Page 20 /OUT pdfmark
[ /Title (Chapter 3)         /Page 21 /OUT pdfmark
```

**Level-based** — stet extension, easier to author programmatically.
Each entry says how deep it is; the builder figures out parents:

```postscript
[ /Title (Part I)    /OutlineLevel 1 /Page 1  /OUT pdfmark
[ /Title (Chapter 1) /OutlineLevel 2 /Page 2  /OUT pdfmark
[ /Title (Chapter 2) /OutlineLevel 2 /Page 9  /OUT pdfmark
[ /Title (Part II)   /OutlineLevel 1 /Page 20 /OUT pdfmark
[ /Title (Chapter 3) /OutlineLevel 2 /Page 21 /OUT pdfmark
```

The two conventions don't mix in a single document — when *any* record
carries `/OutlineLevel`, the whole batch uses the level-based builder.

When the writer encounters an out-of-range `/Page` (zero, or larger
than the last `showpage`), it drops just that entry's destination —
the bookmark itself still emits as a non-navigable label so the user
can see something went wrong instead of the catalog silently
disappearing.

The catalog also gets `/PageMode /UseOutlines` whenever any `/OUT`
records exist, so PDF viewers open the bookmark pane by default.

### `/ANN` — page annotations (Link, Text, FreeText)

```postscript
[ /Rect [72 720 540 750]
  /Subtype /Link
  /Border [0 0 1]
  /Action << /S /URI /URI (https://example.org) >>
  /ANN pdfmark
```

Each `/ANN pdfmark` records one annotation (`/Annot`) attached to a
specific page. The writer assembles per-page `/Annots` arrays at
end-of-job. Annotations whose `/Subtype` is unrecognised, or whose
`/Page` is out of range, are dropped silently.

#### Shared keys

| Key | Type | Meaning |
|---|---|---|
| `/Subtype` | name | Required — `/Link`, `/Text`, `/FreeText`. Other subtypes (Stamp, Widget, ...) ship in later phases |
| `/Rect` | 4-array | Required — `[llx lly urx ury]` in default user space |
| `/Page` | integer | 1-based page target. Defaults to the page being assembled (= `current_page + 1` from the buffer's showpage counter) |
| `/SrcPg` | integer | stet alias for `/Page`; same semantics |
| `/Color` | 3-array | Optional `[r g b]` in `0..=1` — flows to `/C` |
| `/Border` | 3-array or 4-array | `[Hradius Vradius Width]` or `[Hradius Vradius Width [dash...]]` |
| `/Title` | string | Annotator name — flows to `/T` |
| `/Contents` | string | Body text — flows to `/Contents` |

#### `/Subtype /Link`

| Key | Type | Meaning |
|---|---|---|
| `/Action` | dict | Action to fire on click. Same shape as `/OUT`'s `/Action`: `<< /S /URI /URI (string) >>` or `<< /S /GoTo /D <name-or-array> >>` |
| `/Page` + `/View` | int + array | Internal jump target; same view-spec syntax as `/OUT` |
| `/Dest` | name or string | Named destination (resolved against the document's name tree once `/DEST pdfmark` lands) |
| `/H` | name | Highlight mode: `/N` none, `/I` invert, `/O` outline, `/P` push |

`/Action` wins over `/Dest` wins over `/Page`. A link with none of the
three becomes a non-clickable region (still emits, viewers ignore it).

#### `/Subtype /Text` (sticky note)

| Key | Type | Default |
|---|---|---|
| `/Open` | boolean | `false` |
| `/Name` | name | `/Note` (also accepts `/Comment`, `/Key`, `/Help`, `/NewParagraph`, `/Paragraph`, `/Insert` — anything else falls back to `/Note`) |

#### `/Subtype /FreeText`

| Key | Type | Default |
|---|---|---|
| `/DA` | string | `(0 0 0 rg /Helv 10 Tf)` — black 10-pt Helvetica |
| `/Q` | integer | None — viewer chooses; 0=left, 1=center, 2=right |

The default `/DA` matters because most PDF viewers won't render
`/FreeText` at all without one. If you supply `/Contents` but no `/DA`,
stet fills in the default so the annotation is visible.

#### Page scoping

Pdfmark records remember "the page being assembled at the time the
mark fired". Inside the interpreter, `Context::pdfmark_buffer` keeps a
`current_page` counter incremented by every `showpage`; when an `/ANN`
record omits `/Page` (or `/SrcPg`), it scopes to `current_page + 1` —
i.e. the page that has not yet been finalised.

```postscript
% Annotation A → page 1
[ /Rect [...] /Subtype /Text /Contents (a) /ANN pdfmark
showpage
% Annotation B → page 2
[ /Rect [...] /Subtype /Text /Contents (b) /ANN pdfmark
showpage
```

Multiple annotations on the same page accumulate in that page's
`/Annots` array in declaration order.

### `/DEST` — named destinations

```postscript
[ /Dest /chap1 /Page 5 /View [/XYZ 100 700 1.5] /DEST pdfmark
```

Registers a named destination in the document's `/Names /Dests` name
tree. Bookmarks (`/OUT`) and link annotations (`/ANN /Subtype /Link`)
can then reference the destination by name (`/Dest /chap1`) and PDF
viewers resolve it through the name tree to land on the right page
+ view.

| Key | Type | Meaning |
|---|---|---|
| `/Dest` | name or string | Required — destination name |
| `/Page` | integer | Required — 1-based target page |
| `/View` | array | Optional view spec; default `[/XYZ null null null]`. Same eight variants as `/OUT`'s `/View` |

Records targeting a `/Page` outside the rendered range are dropped
silently. Records that share a name resolve "last wins" (matches the
`/DOCINFO` and other type-tags' "later record overrides earlier").

The catalog's `/Names` entry is only emitted when at least one valid
record exists. The current implementation uses a single-leaf name tree
which is fine for documents with up to ~hundreds of dests; if a use
case ever needs more, the leaf can be split into `/Kids` without
changing the operator surface.

### `/PAGE` and `/PAGES` — page-box overrides

```postscript
[ /CropBox [36 36 576 756] /Page 2 /PAGE pdfmark
[ /TrimBox [9 9 603 783] /PAGES pdfmark
```

`/PAGE` overrides keys on a single page. `/PAGES` provides
document-wide defaults for keys that aren't already set on a specific
page. Both share the same key set:

| Key | Type | Meaning |
|---|---|---|
| `/Page` (only on `/PAGE`) | integer | 1-based target page. Defaults to the page being assembled (= `current_page + 1`) when omitted |
| `/SrcPg` | integer | Alias for `/Page` |
| `/CropBox`, `/BleedBox`, `/TrimBox`, `/ArtBox` | 4-array | `[llx lly urx ury]` in default user space |
| `/Rotate` | integer | 0, 90, 180, 270 (or their negative equivalents). Other values dropped silently |

Resolution rule (per key, per page): the most recent `/PAGE` for that
specific page wins; if none exists, the most recent `/PAGES` for that
key applies; otherwise the device default.

```postscript
% /PAGES sets a doc-wide CropBox; /PAGE for page 1 overrides it.
% Page 1 → [200 200 400 400]; pages 2..N → [10 10 100 100].
[ /CropBox [10 10 100 100] /PAGES pdfmark
[ /CropBox [200 200 400 400] /Page 1 /PAGE pdfmark
```

A `/PAGE` record with neither any box nor a `/Rotate` is dropped —
there's nothing to apply.

### `/VIEWERPREFERENCES` — catalog viewer preferences

```postscript
[ /HideToolbar true
  /FitWindow true
  /PageMode /FullScreen
  /PageLayout /TwoColumnLeft
  /VIEWERPREFERENCES pdfmark
```

Catalog-level UI hints. The keys split into two groups: most live
under the `/ViewerPreferences` indirect object on `/Catalog`, while
`/PageLayout` and `/PageMode` are catalog-level entries proper. Adobe
pdfmark groups them all under `/VIEWERPREFERENCES`; stet does the same
and writes them to the right places automatically.

Multiple `/VIEWERPREFERENCES pdfmark` blocks merge — later records
override earlier ones key-by-key.

#### `/ViewerPreferences` dict (boolean entries)

| Key | Type |
|---|---|
| `/HideToolbar` | bool |
| `/HideMenubar` | bool |
| `/HideWindowUI` | bool |
| `/FitWindow` | bool |
| `/CenterWindow` | bool |
| `/DisplayDocTitle` | bool |
| `/NonFullScreenPageMode` | name: `UseNone`, `UseOutlines`, `UseThumbs`, `UseOC` |
| `/Direction` | name: `L2R`, `R2L` |

Unknown name values are dropped silently.

#### Catalog-level entries

| Key | Type | Allowed values |
|---|---|---|
| `/PageLayout` | name | `SinglePage`, `OneColumn`, `TwoColumnLeft`, `TwoColumnRight`, `TwoPageLeft`, `TwoPageRight` |
| `/PageMode` | name | `UseNone`, `UseOutlines`, `UseThumbs`, `FullScreen`, `UseOC`, `UseAttachments` |

`/PageMode` from `/VIEWERPREFERENCES` wins over the `UseOutlines`
default the writer applies when `/OUT` records exist. Invalid name
values are dropped silently. The catalog gets a `/ViewerPreferences`
ref only when at least one nested boolean / name entry is set.

### `/Metadata` — XMP metadata stream

```postscript
[ /Metadata (<?xpacket begin='?'?><x:xmpmeta>...</x:xmpmeta><?xpacket end='w'?>)
  /Metadata pdfmark
```

Attaches an XMP packet to the document. The writer wraps the bytes in
a stream object with `/Type /Metadata` / `/Subtype /XML` and references
it from `/Catalog /Metadata`. The XMP is round-tripped byte-for-byte
and written uncompressed (PDF spec requires this for grep-friendly
extraction).

When multiple `/Metadata pdfmark` blocks appear, the last one wins —
the XMP packet is a single stream, not a merged document. Records with
no string value are dropped silently.

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
