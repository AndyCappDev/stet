# PDF Reader Structural API

`stet-pdf-reader` exposes the structural content of a PDF — metadata,
bookmarks, annotations, form fields, embedded files, page geometry,
parse diagnostics — as typed, read-only Rust data. This is in addition
to the rendering API (`render_page`, `render_page_to_rgba`); the two
are independent, and using one does not pay the cost of the other.

The `stet inspect <file.pdf>` CLI subcommand exercises the full API as
a human-readable summary; see the bottom of this document for sample
output.

## Quick start

```rust
use stet_pdf_reader::PdfDocument;

let data = std::fs::read("document.pdf")?;
let doc = PdfDocument::from_bytes(&data)?;

println!("Title:  {:?}", doc.metadata().title);
println!("Pages:  {}",   doc.page_count());
println!("Layers: {}",   doc.outline().len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Every accessor below parses lazily on first call and caches its
result. A document the caller only renders pays nothing for the
structural API surface.

---

## Document metadata

`metadata() -> &DocumentMetadata`

```rust
use stet_pdf_reader::{PdfDocument, TrappedFlag};

# let data = vec![];
let doc = PdfDocument::from_bytes(&data)?;
let m = doc.metadata();

println!("Title:    {:?}", m.title);
println!("Author:   {:?}", m.author);
println!("Subject:  {:?}", m.subject);
println!("Keywords: {:?}", m.keywords);
println!("Creator:  {:?}", m.creator);  // source authoring app
println!("Producer: {:?}", m.producer); // PDF-writing app

if let Some(d) = &m.creation_date {
    println!("Created: {}-{:02}-{:02}", d.year, d.month, d.day);
}
match m.trapped {
    Some(TrappedFlag::True)    => println!("Trapped for press"),
    Some(TrappedFlag::False)   => println!("Not trapped"),
    Some(TrappedFlag::Unknown) => println!("Trap state unknown"),
    None                       => {}
}

// XMP metadata stream (PDF 1.4+) as raw XML
if let Some(xmp) = &m.xmp_xml {
    println!("XMP length: {} bytes", xmp.len());
}

// Non-standard /Info entries the document carried
for (key, value) in &m.custom {
    println!("{key} = {value}");
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

String decoding handles PDFDocEncoding (default), UTF-16BE with BOM
(`FE FF`), and UTF-8 with BOM (`EF BB BF`, PDF 2.0). Date strings come
in PDF's own format `D:YYYYMMDDHHmmSSOHH'mm'`; truncated forms (just
year, year+month, etc.) are accepted.

## Viewer preferences

`viewer_preferences() -> &ViewerPreferences`

```rust
use stet_pdf_reader::{PageMode, PrintScaling};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
let prefs = doc.viewer_preferences();
if prefs.hide_toolbar     { println!("hide toolbar"); }
if prefs.fit_window       { println!("fit window to first page"); }
if prefs.display_doc_title { println!("show /Info /Title in title bar"); }

match prefs.page_mode {
    PageMode::FullScreen => println!("opens in full-screen"),
    PageMode::UseOutlines => println!("opens with outline panel"),
    _ => {}
}

match prefs.print_scaling {
    PrintScaling::None       => println!("default: print at 100%"),
    PrintScaling::AppDefault => println!("default: viewer chooses"),
}
```

All fields default per spec (ISO 32000-2 §12.2 Table 147 / §7.7.2
Table 28) when the corresponding entries are absent.

## Outline (bookmarks)

`outline() -> &[OutlineItem]`

```rust
use stet_pdf_reader::{Destination, OutlineItem};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
fn print_outline(items: &[OutlineItem], depth: usize) {
    let pad = "  ".repeat(depth);
    for item in items {
        let target = match &item.destination {
            Some(Destination::PageView { page: Some(p), .. }) => format!(" → page {}", p + 1),
            Some(Destination::NamedDest(name)) => format!(" → /{name}"),
            _ => String::new(),
        };
        println!("{pad}- {}{target}", item.title);
        print_outline(&item.children, depth + 1);
    }
}

print_outline(doc.outline(), 0);
```

Each item carries `title`, `destination` and/or `action`, recursive
`children`, optional RGB `color`, `style` (italic/bold flags), and
`open` (from the sign of `/Count`).

The walker bounds traversal at 64 levels and 100 000 nodes; cycles are
detected via a visited set and result in a `ParseWarning` rather than
infinite recursion.

## Destinations and actions

```rust
use stet_pdf_reader::{Action, Destination, ViewSpec};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
// Every named destination in the document, merged from /Catalog /Dests
// and /Catalog /Names /Dests (legacy entries override name-tree entries
// per ISO 32000-2 §12.3.2.3).
for (name, dest) in doc.destinations() {
    if let Destination::PageView { page: Some(p), view, .. } = dest {
        println!("{name} → page {}", p + 1);
        match view {
            ViewSpec::Xyz { zoom, .. } => println!("  zoom: {:?}", zoom),
            ViewSpec::Fit              => println!("  fit page"),
            _                          => {}
        }
    }
}

// Direct lookup
if let Some(dest) = doc.resolve_named_destination("Chapter1") {
    // ...
}
```

`Action` is an enum covering URI, GoTo, GoToR (remote), GoToE
(embedded), Launch, Named, JavaScript, SubmitForm, ResetForm, Hide,
Sound, Movie, Thread, and Other for unknown subtypes. JavaScript is
exposed as raw source — stet does not execute.

## Page annotations

`page_annotations(page) -> Result<&[Annotation], PdfError>`

```rust
use stet_pdf_reader::{Annotation, AnnotationKind, AnnotationKindData, Action};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
for page in 0..doc.page_count() {
    for annot in doc.page_annotations(page)? {
        match (&annot.kind, &annot.kind_data) {
            (AnnotationKind::Link, AnnotationKindData::Link(link)) => {
                if let Some(Action::Uri { uri, .. }) = &link.action {
                    println!("page {}: link → {uri}", page + 1);
                }
            }
            (AnnotationKind::Highlight, AnnotationKindData::Markup(m)) => {
                println!("page {}: highlight over {} regions", page + 1, m.quad_points.len());
            }
            (AnnotationKind::Text, AnnotationKindData::Text(t)) => {
                println!(
                    "page {}: sticky note ({:?}) — {:?}",
                    page + 1,
                    t.icon,
                    annot.contents
                );
            }
            _ => {}
        }
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Per-page lazy parsing: each page caches independently in a
`Vec<OnceCell<Vec<Annotation>>>`, so a 1000-page document with
annotations only on a handful of pages doesn't pay to parse the rest.

Common fields (`rect`, `contents`, `name`, `modified`, `title`,
`subject`, `flags`, `color`, `border`, `has_appearance`) live on
`Annotation`; subtype-specific fields live in `AnnotationKindData`
variants:

- `Link` — action, destination, highlight mode, quad points
- `Text` — open state, icon name, review state
- `FreeText` — default appearance, quadding, callout line
- `Markup` (Highlight / Underline / Squiggly / StrikeOut) — quad points
- `Line` — endpoints, line endings, leader length, caption
- `Shape` (Square / Circle) — interior color, border padding
- `Polygon` (Polygon / PolyLine) — vertices, line endings
- `Ink` — strokes
- `Stamp` — icon name
- `Caret` — symbol
- `FileAttachment` — filename, icon
- `Popup` — open state, parent annotation reference
- `Minimal` — for unhandled or rare subtypes (Screen, PrinterMark,
  TrapNet, Watermark, Sound, Movie, Widget, 3D, RichMedia, Other)

## Form fields (AcroForm)

`form() -> Option<&FormCatalog>`

```rust
use stet_pdf_reader::{ButtonType, FieldKind, FieldValue};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
let Some(form) = doc.form() else {
    return; // document has no AcroForm
};

if form.sig_flags.signatures_exist {
    println!("document carries digital signatures");
}

fn walk(fields: &[stet_pdf_reader::FormField]) {
    for f in fields {
        match &f.kind {
            FieldKind::Text(tf) => println!(
                "Text  {} = {:?} (max {:?}, multiline={})",
                f.name, f.value, tf.max_length, tf.multiline
            ),
            FieldKind::Button(bf) => match bf.button_type {
                ButtonType::Checkbox  => println!("Check {} = {:?}", f.name, f.value),
                ButtonType::Radio     => println!("Radio {} (options: {:?})", f.name, bf.options),
                ButtonType::Pushbutton => println!("Push  {}", f.name),
            },
            FieldKind::Choice(cf) => println!(
                "{} {} = {:?} ({} options)",
                if cf.combo { "Combo" } else { "List " },
                f.name, f.value, cf.options.len()
            ),
            FieldKind::Signature(_) => println!("Sig   {}", f.name),
            FieldKind::Container    => {} // non-terminal namespace node
            FieldKind::Other { ft } => println!("Other ({}) {}", ft, f.name),
        }
        walk(&f.children);
    }
}

walk(&form.fields);
```

Field names are fully qualified — `/T` partials joined with `.` from
the root, so `order.shipping.street` is one field nested two levels
deep under containers `order` then `shipping`.

Each terminal field also carries `widget_obj_nums: Vec<u32>` —
the object numbers of its widget annotations. To fetch the renderable
widget data for a field:

```rust
# use stet_pdf_reader::{AnnotationKind, FormField};
# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
# let field: FormField = unimplemented!();
for page in 0..doc.page_count() {
    for annot in doc.page_annotations(page).unwrap_or(&[]) {
        if annot.kind == AnnotationKind::Widget {
            // Match by obj_num via your own bookkeeping; the reader
            // does not currently expose obj_num on Annotation directly
            // — Phase 4's annotations are deduplicated against the
            // page's /Annots array.
        }
    }
}
```

## Page boxes

`page_boxes(page) -> Result<PageBoxes, PdfError>`

```rust
# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
let pb = doc.page_boxes(0)?;
println!("MediaBox:   {:?}", pb.media_box);
if let Some(b) = pb.crop_box  { println!("CropBox:    {:?}", b); }
if let Some(b) = pb.bleed_box { println!("BleedBox:   {:?}", b); }
if let Some(b) = pb.trim_box  { println!("TrimBox:    {:?}", b); }
if let Some(b) = pb.art_box   { println!("ArtBox:     {:?}", b); }
println!("Rotate:     {}", pb.rotate);
println!("UserUnit:   {}", pb.user_unit);
if pb.has_transition         { println!("page transition declared"); }
if pb.has_additional_actions { println!("page-AA dict declared"); }
# Ok::<(), Box<dyn std::error::Error>>(())
```

Each box is `Option<[f64; 4]>` so callers can distinguish "explicitly
set" from "inherits MediaBox per spec default". Rotation is normalized
to 0/90/180/270 (negative values wrap; non-multiples of 90 coerce to 0).

## Embedded files

```rust
# use stet_pdf_reader::{AfRelationship};
# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
for (name, ef) in doc.embedded_files() {
    let size = ef.size.map(|n| format!("{n} B")).unwrap_or_default();
    let rel  = match &ef.relationship {
        Some(AfRelationship::Source)   => "source",
        Some(AfRelationship::Data)     => "data",
        Some(AfRelationship::FormData) => "form-data",
        _ => "?",
    };
    println!("{name} ({size}, {rel}, {:?})", ef.mime_type);
}

// Read the bytes of a specific attachment on demand.
let bytes = doc.embedded_file_bytes("data.csv")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Embedded files come from the catalog's `/Names /EmbeddedFiles` name
tree. Stream bytes are decoded on demand; the table itself only holds
metadata.

## Layers (Optional Content)

PDFs can mark slices of their content for selective visibility — CAD
layers, watermarks, multilingual annotations, print-only or
screen-only overlays. `stet-pdf-reader` exposes the full Optional
Content Group (OCG) model: per-layer metadata, hierarchy, alternate
configurations, runtime visibility overrides via [`LayerSet`], OCMD
membership policies and `/VE` boolean expressions, and intent-driven
rendering.

```rust
# use stet_pdf_reader::{PdfDocument, RenderIntent, layers};
# let doc: PdfDocument = unimplemented!();
for layer in doc.layers() {
    println!(
        "{:>3}  {:<32} default_visible={} locked={}",
        layer.ocg_id, layer.name, layer.default_visible, layer.locked
    );
}

// Render the page hiding any /AS-marked print-off layers.
let print_set = doc.layer_set_for(RenderIntent::Print);
let (rgba, w, h) = doc.render_page_to_rgba_with_layers(0, 150.0, &print_set)?;

// Or build a custom override set and toggle one layer.
let mut custom = layers::layer_set_from_document(&doc);
custom.set(/* ocg_id */ 42, false);
let (rgba2, _, _) = doc.render_page_to_rgba_with_layers(0, 150.0, &custom)?;
# let _ = (rgba, rgba2, w, h);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Full reference (types, methods, OCMD semantics, intent-driven
rendering, `/VE` grammar): see
[`docs/PDF-LAYERS.md`](PDF-LAYERS.md).

[`LayerSet`]: https://docs.rs/stet-graphics/latest/stet_graphics/layer_set/struct.LayerSet.html

## Parse warnings

`parse_warnings() -> Ref<'_, [ParseWarning]>`

```rust
use stet_pdf_reader::{ParsePhase, Severity};

# let doc: stet_pdf_reader::PdfDocument = unimplemented!();
for w in doc.parse_warnings().iter() {
    let sev = match w.severity {
        Severity::Info    => "info",
        Severity::Warning => "warn",
        Severity::Error   => "error",
    };
    let phase = match &w.phase {
        ParsePhase::Outline                => "outline".to_string(),
        ParsePhase::Annotations { page }   => format!("annotations(p{})", page + 1),
        ParsePhase::Form                   => "form".to_string(),
        // ...
        other => format!("{:?}", other),
    };
    eprintln!("[{sev}] {phase}: {}", w.message);
}
```

Recoverable malformations — outline cycles, annotations missing
`/Rect`, form-field trees exceeding the depth cap — push a
`ParseWarning` rather than failing the whole parse. The list grows as
accessors are called for the first time; cached subsequent calls
don't re-emit.

The returned `Ref` wraps a `RefCell` borrow of the underlying
storage. Drop it before invoking other accessors that might push more
warnings (iterating through it is fine; calling `doc.outline()`
mid-iteration is not).

---

## CLI: `stet inspect`

The `stet inspect <file.pdf>` subcommand exercises every accessor in
this document and pretty-prints the result. Sections appear only
when the document has the corresponding data:

```
$ stet inspect document.pdf
document.pdf

Metadata:
  Title: Annual Report 2026
  Author: Scott Bowman
  Producer: stet 0.1.2
  Created: 2026-04-27 12:00:00 UTC

Pages: 4
  Page 1 size: 612.0 × 792.0 pt (8.50 × 11.00 in)

Outline (3 entries):
  - Chapter 1 → page 1 (fit)
    - Section 1.1 → page 2 (xyz)
  - Chapter 2 → page 3 (fit)

Named destinations: 2

Annotations: 3
  Page 1: 2 Link
  Page 3: 1 Highlight

Form: 4 terminal fields (4 widgets)
  By kind: Button: 1, Text: 3
  NeedAppearances: true

Embedded files: 1
  - data.csv (1.2 KB, text/csv)

Warnings: 1
  outline: 1
  [warn] outline: outline cycle detected; sibling chain truncated
```

Pass `--password <pw>` for encrypted documents.

---

## Caveats and design notes

- **Single-threaded.** `PdfDocument<'a>` is `!Send` (the resolver uses
  `RefCell` for caching). If you need to share a document across
  threads, build a wrapper that re-parses or use one document per
  thread. Cross-thread use of the structural API is not supported.
- **PDF 2.0 coverage.** We parse PDF 1.x and most PDF 2.0 additions
  (FormData / Schema relationship hints, UTF-8 string BOM, …). XFA
  payloads are detected but not parsed.
- **No write API.** This is a read-only API. Authoring PDF
  structures (writing bookmarks, annotations, form fields) is the
  separate `pdfmark` plan, not this one.
- **Walker caps.** Outline / form-field / name-tree walkers cap at
  depths in the 32–64 range and total nodes / entries in the
  100 000 – 1 000 000 range. Real-world documents never hit these;
  caps exist to make pathological / cyclic / malicious inputs
  bounded rather than fatal. Each truncation pushes a
  [`ParseWarning`].
- **Cross-linking widgets.** A terminal `FormField`'s
  `widget_obj_nums` list cross-references the widget annotations on
  the relevant pages. Look up the annotation via your own bookkeeping
  if you need both views of the same widget.
