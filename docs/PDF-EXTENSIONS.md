# PDF-Imaging Extensions

stet's PostScript interpreter ships eighteen operators and two stream
filters that expose the imaging features PDF has but Adobe PostScript
Level 3 doesn't: constant alpha and blend modes, transparency groups,
soft masks, optional content (OCG / layers), and the PDF-only
JBIG2Decode / JPXDecode filters.

These are **stet-specific** extensions — they aren't part of the PLRM.
They use spec-style names (`setfillopacity`, `setblendmode`, …) rather
than the leading-dot convention GhostScript adopted; a thin
compatibility shim in `resources/Init/pdfextensions.ps` aliases the
GS-flavoured names so existing code that targeted GhostScript keeps
working without changes.

The on-screen renderer already consumed the relevant display-list
elements from the PDF reader path (transparency groups, soft masks,
OCG groups); this surface gives PostScript code the producer side.

## Quick start

```postscript
% Translucent red square overlapping a translucent blue square,
% composited via the page's parent group.
0.5 setfillopacity
1 0 0 setrgbcolor
100 100 200 200 rectfill

/Multiply setblendmode
0 0 1 setrgbcolor
180 180 200 200 rectfill

% Group everything under an OCG so the consumer can toggle it.
<< /Name (Highlights) /DefaultVisible true >> defineocg
beginoptionalcontent
  newpath 50 50 moveto 550 50 lineto 550 550 lineto 50 550 lineto closepath
  0.3 setfillopacity 1 1 0 setrgbcolor fill
endoptionalcontent
showpage
```

---

## Alpha and blend modes (Phase 1)

Five graphics-state knobs and their `current*` readers. Defaults
preserve PostScript's pre-extension behaviour: opacity 1.0, blend mode
`/Normal`, alpha-is-shape `false`, text knockout `true`.

| Operator | Stack effect | PDF analogue |
|----------|--------------|--------------|
| `setfillopacity` | `num → —` | `ca` (range \[0,1\], `rangecheck` otherwise) |
| `currentfillopacity` | `— → num` | — |
| `setstrokeopacity` | `num → —` | `CA` |
| `currentstrokeopacity` | `— → num` | — |
| `setblendmode` | `name → —` | `BM` |
| `currentblendmode` | `— → name` | — |
| `setalphaisshape` | `bool → —` | `AIS` |
| `currentalphaisshape` | `— → bool` | — |
| `settextknockout` | `bool → —` | `TK` |
| `currenttextknockout` | `— → bool` | — |

Blend-mode names accepted: `/Normal`, `/Multiply`, `/Screen`,
`/Overlay`, `/Darken`, `/Lighten`, `/ColorDodge`, `/ColorBurn`,
`/HardLight`, `/SoftLight`, `/Difference`, `/Exclusion`, `/Hue`,
`/Saturation`, `/Color`, `/Luminosity`, plus `/Compatible` (alias for
`/Normal`). Anything else raises `rangecheck`.

`gsave` / `grestore` and `save` / `restore` round-trip all five fields
because `GraphicsState` is `Clone`. PDF reader paths populate the
matching gstate fields when an `ExtGState` dict carries `/ca`, `/CA`,
`/BM`, `/AIS`, or `/TK`, so a PDF round-tripped through stet sees the
same values its `q ... Q` blocks would have set.

```postscript
gsave
  0.4 setfillopacity
  /Multiply setblendmode
  100 100 200 200 rectfill
grestore
% opacity and blend mode are back to 1.0 / /Normal here.
```

---

## Transparency groups (Phase 2)

Two operators bracket a region whose contents are composited offscreen
and then composited onto the parent at the gstate's overall alpha and
blend mode.

| Operator | Stack effect |
|----------|--------------|
| `begintransparencygroup` | `dict → —` |
| `endtransparencygroup` | `— → —` |

Recognised dict keys (all optional):

- `/Isolated` — boolean, default `false`. When true, the group renders
  against a transparent backdrop instead of the parent's accumulated
  pixels.
- `/Knockout` — boolean, default `false`. When true, children
  composite against the group's initial backdrop rather than the
  running result of earlier siblings.
- `/CS` — name or `[/ICCBased <stream>]` array; accepted values are
  `/DeviceGray`, `/DeviceRGB`, `/DeviceCMYK`, `/CalGray`, `/CalRGB`,
  and `/ICCBased` (treated as inherited because PostScript can't
  describe the embedded profile inline). Other names raise
  `rangecheck`.
- `/BBox` — 4-element array `[llx lly urx ury]` in user space. The
  CTM transforms it to device space at begin time. Omitted ⇒ default
  is the active device-clip bbox if any, otherwise the page bbox.

While a group is open, paint operators emit into the frame's display
list rather than the page-level list. `endtransparencygroup` emits a
`DisplayElement::Group` carrying the captured children and reads the
compositing alpha and blend mode from the gstate active at end time —
matching PDF's "the q/Q around `Do` controls the group composite"
model.

Guards:

- `endtransparencygroup` raises `rangecheck` when no group is open or
  when an unbalanced `gsave` inside the group hasn't been matched by
  a `grestore` before close.
- `grestore` is a no-op rather than popping a graphics state created
  before the group opened (it would orphan the group's capture frame).
- `restore` raises `invalidrestore` when the save and the restore
  straddle different group nesting depths.
- `showpage` and `copypage` raise `rangecheck` while a group is open;
  the captured content would never composite onto the page output.

Groups nest:

```postscript
0.3 setfillopacity
<< /Isolated true /Knockout true >> begintransparencygroup
  newpath 100 100 200 200 rectfill
  << /Isolated false >> begintransparencygroup
    1 0 0 setrgbcolor
    newpath 150 150 100 100 rectfill
  endtransparencygroup
endtransparencygroup
```

---

## Soft masks (Phase 3)

Three operators capture a mask form, then a content scope, and emit a
`DisplayElement::SoftMasked` that attenuates the content by the mask.

| Operator | Stack effect |
|----------|--------------|
| `beginsoftmask` | `dict → —` |
| `endsoftmask` | `— → —` |
| `clearsoftmask` | `— → —` |

Required dict keys:

- `/Subtype` — `/Alpha` (use the mask form's alpha channel directly)
  or `/Luminosity` (convert the rendered mask to grayscale).
  `rangecheck` for any other name.
- `/BBox` — 4-element user-space array; the CTM transforms it to
  device space at begin time. Missing `/BBox` raises `undefined`.

Optional dict keys:

- `/BC` — backdrop colour for luminosity masks. 1, 3, or 4 components
  (gray / RGB / CMYK→RGB approximation); collapsed to RGB for the
  renderer.
- `/TR` — transfer function. Only the canonical `{ 1 exch sub }` invert
  procedure is recognised explicitly (sets a fast `transfer_invert`
  flag the renderer reads); other procedures fall back to identity.

Lifecycle:

1. `beginsoftmask` opens a mask-builder frame. Subsequent paint emits
   into the frame as the mask form.
2. `endsoftmask` transmutes the frame in place: the captured display
   list moves into a `Masked` variant as the mask, and the frame's
   own list resets so the implicit content scope can capture into it.
3. `clearsoftmask` closes the content scope and emits
   `DisplayElement::SoftMasked { mask, content, params, … }`.

The same gsave-depth and group-routing guards from Phase 2 apply. An
`endsoftmask` issued without a matching `beginsoftmask`, or a
`clearsoftmask` outside a `Masked` scope, raises `rangecheck`.

```postscript
% Linear-gradient luminosity mask attenuating a solid blue rectangle.
<< /Subtype /Luminosity /BBox [100 100 400 300] >> beginsoftmask
  100 100 400 300 << >> setbbox      % gradient over the bbox
  0 0 0 setrgbcolor 100 100 moveto 100 300 lineto stroke
  1 1 1 setrgbcolor 400 100 moveto 400 300 lineto stroke
endsoftmask
  0 0 1 setrgbcolor 100 100 300 200 rectfill
clearsoftmask
```

---

## Optional content / layers (Phase 4)

Three operators register OCGs and bracket layer scopes. The emitted
`DisplayElement::OcgGroup` uses the same `OcgVisibility::Single`
predicate the PDF reader path emits — the renderer's `LayerSet`
machinery from `docs/PDF-LAYERS.md` works against PS-defined layers
identically.

| Operator | Stack effect |
|----------|--------------|
| `defineocg` | `dict → name` |
| `beginoptionalcontent` | `name → —` |
| `endoptionalcontent` | `— → —` |

`defineocg` recognised dict keys:

- `/Name` — string or name (the human-readable layer label).
  `undefined` if absent. Strings are interned to a `name`.
- `/DefaultVisible` — boolean, default `true`. The visibility used
  when no `LayerSet` override exists for this OCG. `typecheck` if
  the entry is present but not a boolean.
- `/Intent` and `/Usage` — accepted but otherwise ignored at this
  layer; usage/intent-driven visibility lives in
  `stet_pdf_reader::layers` (see `docs/PDF-LAYERS.md`).

`defineocg` allocates a monotonic OCG id, stores it in the context's
registry under the interned `/Name`, and returns the name literal.
`beginoptionalcontent` looks up the registered name (raises
`undefined` for unregistered names) and opens a capture frame.
`endoptionalcontent` emits the `OcgGroup` element. Same gsave-depth
and group-routing guards as Phases 2 and 3.

```postscript
<< /Name (HiddenAnnotations) /DefaultVisible false >> defineocg
beginoptionalcontent
  /Helvetica 12 selectfont
  100 100 moveto (Drawing notes — hidden by default) show
endoptionalcontent
```

A consumer (renderer or PDF writer) that wants to toggle PS-defined
layers reads `DisplayElement::OcgGroup` and applies the same
`LayerSet` evaluation it already uses for the PDF reader path.

---

## JBIG2Decode and JPXDecode filters (Phase 5)

Two PDF-only stream filters wired into the existing `filter` operator.
Both decode-only, both buffered (read all bytes from the source on
first read, decode once via `hayro-jbig2` / `hayro-jpeg2000`, serve
the uncompressed bytes thereafter). The same shape `DCTDecode` and
`CCITTFaxDecode` use.

```postscript
% bilevel scan delivered as a JBIG2 stream
data_source <</JBIG2Globals globals_source>> /JBIG2Decode filter
% pixel data delivered as JPEG 2000
data_source /JPXDecode filter
```

`/JBIG2Decode` recognised parameters:

- `/JBIG2Globals` — optional file or string carrying the shared
  globals segment. Multiple JBIG2 streams in the same document
  typically reference one globals stream; pass the same source on
  each filter creation.

JBIG2 output is row-packed 1-bit-per-pixel DeviceGray (8 pixels /
byte, MSB-first; `0` = black, `1` = white per PDF convention).

`/JPXDecode` is parameterless. The output is hayro's interleaved
pixel buffer; the caller declares the colour space via
`setcolorspace` before consuming the bytes (the JPX-internal colour
space is informational only, matching PDF's rule).

Decode failures (malformed streams, unsupported variants) surface as
`ioerror` at the PostScript level, catchable with `stopped`.

Encode variants are not implemented — no standard PostScript encoders
exist for either format and they're rare in the wild.

---

## GhostScript-compatible aliases

Loaded automatically from `resources/Init/pdfextensions.ps` after
font initialisation. Code written against GhostScript's leading-dot
convention works unchanged.

| GS alias | stet operator |
|----------|---------------|
| `.setopacityalpha` | `dup setfillopacity setstrokeopacity` |
| `.currentopacityalpha` | `currentfillopacity` |
| `.setshapealpha` | `true setalphaisshape dup setfillopacity setstrokeopacity` |
| `.setfillconstantalpha` | `setfillopacity` |
| `.setstrokeconstantalpha` | `setstrokeopacity` |
| `.setblendmode` | `setblendmode` |
| `.currentblendmode` | `currentblendmode` |
| `.setalphaisshape` | `setalphaisshape` |
| `.currentalphaisshape` | `currentalphaisshape` |
| `.settextknockout` | `settextknockout` |
| `.currenttextknockout` | `currenttextknockout` |
| `.begintransparencygroup` | `begintransparencygroup` |
| `.endtransparencygroup` | `endtransparencygroup` |
| `.beginsoftmask` | `beginsoftmask` |
| `.endsoftmask` | `endsoftmask` |
| `.clearsoftmask` | `clearsoftmask` |
| `.begintransparencymaskgroup` | `beginsoftmask` |
| `.endtransparencymask` | `endsoftmask` |
| `.defineocg` | `defineocg` |
| `.beginoptionalcontent` | `beginoptionalcontent` |
| `.endoptionalcontent` | `endoptionalcontent` |

---

## Non-goals

These extensions cover graphics features only. Out of scope:

- Annotations and form fields (`/Annot`, `/AcroForm`)
- Bookmarks and document outlines
- Metadata streams (`/Metadata`, XMP)
- 3D content (`/3D`)
- Movies, sounds, JavaScript actions
- Measurement and geospatial features

These belong to PDF's interactive layer; consult
`docs/PDF-READER-API.md` for stet's read-side coverage of those
structures.
