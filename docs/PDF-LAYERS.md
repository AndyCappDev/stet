# PDF Layers (Optional Content)

`stet-pdf-reader` exposes a PDF's Optional Content Groups (OCGs —
informally "layers") through a typed, read-only API: enumeration of
every layer plus its display name, intent, lock state, and `/Usage`
hints; the document's hierarchy and alternate configurations; and a
runtime visibility model that lets a consumer toggle layers on or off
without re-parsing the PDF or rebuilding its display list.

Defined in ISO 32000-2 §8.11. Every accessor parses lazily on first
call and caches its result.

## Quick start

```rust,no_run
use stet_pdf_reader::{PdfDocument, RenderIntent};

let data = std::fs::read("layered.pdf")?;
let doc = PdfDocument::from_bytes(&data)?;

// Enumerate layers.
for layer in doc.layers() {
    println!(
        "{:>3}  {:<32} default_visible={} locked={}",
        layer.ocg_id, layer.name, layer.default_visible, layer.locked
    );
}

// Render the first page for print, honouring /AS auto-state rules.
let print_set = doc.layer_set_for(RenderIntent::Print);
let (rgba, w, h) = doc.render_page_to_rgba_with_layers(0, 150.0, &print_set)?;
# let _ = (rgba, w, h);
# Ok::<(), Box<dyn std::error::Error>>(())
```

---

## Layer enumeration

`layers() -> &[Layer]` returns every OCG declared by the document, in
the order they appear in `/OCProperties /OCGs`. `layer(ocg_id)` looks
up a single layer by its PDF object number — useful when the caller
already has the `ocg_id` from a display-list `OcgGroup` element.

```rust,no_run
# use stet_pdf_reader::PdfDocument;
# let data = vec![];
# let doc = PdfDocument::from_bytes(&data).unwrap();
for layer in doc.layers() {
    println!("{}: {:?}", layer.ocg_id, layer.name);
}

if let Some(watermark) = doc.layer(42) {
    println!("watermark default_visible: {}", watermark.default_visible);
}
```

Each [`Layer`] carries:

- `ocg_id` — PDF object number, the canonical key.
- `name` — display label (PDFDocEncoding / UTF-16BE / UTF-8 BOM all decoded).
- `intent` — `LayerIntent::View` / `Design` / `Export` / `Multiple` / `Other`.
- `locked` — true when listed in the default config's `/Locked` array
  (UI consumers should disable user toggling for locked layers; the
  reader doesn't enforce this).
- `default_visible` — initial visibility under the default
  configuration. Drives the per-`OcgGroup` fallback in the display
  list.
- `usage` — a [`LayerUsage`] with optional View / Print / Export /
  Zoom / Language / User / PageElement / CreatorInfo sub-dicts.
- `creator_info` — `/CreatorInfo` on the OCG dict itself (some PDFs
  also stash one under `/Usage`).

[`Layer`]: https://docs.rs/stet-pdf-reader/latest/stet_pdf_reader/struct.Layer.html
[`LayerUsage`]: https://docs.rs/stet-pdf-reader/latest/stet_pdf_reader/struct.LayerUsage.html

---

## Hierarchy and configurations

Layer panels need a tree, not a flat list. `default_configuration()`
returns the document's `/D` config; `configurations()` lists every
config (default at index 0, alternates from `/Configs` at 1..N);
`configuration(idx)` looks up by index; `layer_tree()` is a shortcut
for the default config's `/Order`.

```rust,no_run
use stet_pdf_reader::{PdfDocument, LayerTreeNode};

# let data = vec![];
# let doc = PdfDocument::from_bytes(&data).unwrap();
fn dump(nodes: &[LayerTreeNode], depth: usize) {
    let indent = "  ".repeat(depth);
    for node in nodes {
        match node {
            LayerTreeNode::Layer(id) => println!("{indent}- ocg {id}"),
            LayerTreeNode::Section { label, header_layer, children } => {
                if let Some(l) = label {
                    println!("{indent}# {l}");
                } else if let Some(h) = header_layer {
                    println!("{indent}# (header layer {h})");
                } else {
                    println!("{indent}# (anonymous section)");
                }
                dump(children, depth + 1);
            }
        }
    }
}

dump(&doc.layer_tree().nodes, 0);
```

Each [`Configuration`] carries `name`, `creator`, `base_state`
(`On` / `Off` / `Unchanged`), `on` / `off` / `locked` arrays, an
`intent`, an `auto_state` rule list (the `/AS` array), the parsed
`/Order` tree, a `list_mode` (`AllPages` / `VisiblePages`), and
`rb_groups` (radio-button groups).

`/Order` parsing follows ISO 32000-2 §8.11.4.3 with one element of
look-back: bare nested arrays become anonymous sections, a
string-literal followed by an array becomes a labelled section, an OCG
ref immediately followed by an array becomes a header-layer section,
and any other shape produces a `LayerTreeNode::Layer` leaf or a
parse warning.

[`Configuration`]: https://docs.rs/stet-pdf-reader/latest/stet_pdf_reader/struct.Configuration.html

---

## Runtime visibility — `LayerSet`

The display list carries each `OcgGroup`'s [`OcgVisibility`] predicate
together with a per-variant `default_visible` fallback baked from the
document's default configuration. To override visibility at render
time, a consumer constructs a [`LayerSet`] and passes it to
`render_page_to_rgba_with_layers`.

```rust,no_run
use stet_pdf_reader::{PdfDocument, layers};

# let data = vec![];
# let doc = PdfDocument::from_bytes(&data).unwrap();
// Start from the document's default configuration so every layer has
// an explicit entry matching its document-level on/off state.
let mut set = layers::layer_set_from_document(&doc);

// Toggle the watermark off.
set.set(/* ocg_id */ 42, false);

// Render with overrides applied.
let (rgba, w, h) = doc.render_page_to_rgba_with_layers(0, 150.0, &set)?;
# let _ = (rgba, w, h);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`LayerSet::new()` is empty — every OCG falls back to its
`default_visible`. Mutate with `set` / `clear`. For radio-button
groups, `enforce_rb_group(group, newly_on)` flips one layer ON and
the rest OFF.

`layer_set_from_document(doc)` populates an entry for each layer at
its `default_visible`. `layer_set_from_configuration(doc, idx)` does
the same for an alternate configuration: it applies `BaseState`,
then the config's `/ON` and `/OFF` overrides.

[`OcgVisibility`]: https://docs.rs/stet-graphics/latest/stet_graphics/display_list/enum.OcgVisibility.html
[`LayerSet`]: https://docs.rs/stet-graphics/latest/stet_graphics/layer_set/struct.LayerSet.html

---

## OCMD policies and `/VE` expressions

When a `/OC BDC` block references an Optional Content Membership
Dictionary instead of a single OCG, the display list carries an
[`OcgVisibility::Membership`] (with the parsed `/P` policy —
`AllOn` / `AnyOn` / `AllOff` / `AnyOff`) or
[`OcgVisibility::Expression`] (with a parsed `/VE` boolean
expression — `/And` / `/Or` / `/Not` over OCG refs).

`LayerSet::evaluate` short-circuits: when **none** of the relevant
leaves are overridden, it returns the variant's `default_visible`
directly (the OCMD's static evaluation under the default
configuration). This is the byte-identity guarantee for consumers
that don't override leaves.

When at least one leaf is overridden, the policy or expression
evaluates with overridden leaves taking precedence and missing
leaves falling back to the variant's `default_visible`.

Malformed `/VE` (unknown leading operator, wrong arity on `/Not`,
nested non-array non-ref leaves) emits a `ParsePhase::Layers`
warning and falls back to the OCMD's `/OCGs` + `/P` membership
form.

---

## Intent-driven rendering

PDFs can declare `/AS` automatic-state rules that swap layers on or
off depending on whether the document is being viewed, printed, or
exported. `layer_set_for(intent)` builds a `LayerSet` with those rules
applied:

```rust,no_run
use stet_pdf_reader::{PdfDocument, RenderIntent};

# let data = vec![];
# let doc = PdfDocument::from_bytes(&data).unwrap();
let view_set   = doc.layer_set_for(RenderIntent::View);
let print_set  = doc.layer_set_for(RenderIntent::Print);
let export_set = doc.layer_set_for(RenderIntent::Export);
```

Algorithm:

1. Start from the document's default configuration (every layer at its
   `default_visible`).
2. Walk the default config's `/AS` rules. For each rule whose `/Event`
   matches the intent, look up each OCG in the rule's `/OCGs` array,
   inspect every `/Category` (View / Print / Export) on that OCG's
   `/Usage` sub-dict, and apply the resulting ON/OFF state to the
   `LayerSet`.

Per spec, layers carrying `/Usage` hints **without** a matching
`/AS` rule stay at their default — the hints are informational only.
Some viewers heuristically apply them anyway; stet does not.

PDF doesn't define precedence when multiple `/AS` rules touch the
same layer; this implementation is **last-rule-wins**.

---

## Type reference

| Type | Purpose |
|------|---------|
| `Layer` | One OCG: name, intent, lock, default-visibility, usage |
| `LayerIntent` | View / Design / Export / Multiple / Other |
| `LayerUsage` | View / Print / Export / Zoom / Language / User / PageElement / CreatorInfo |
| `UsageState` | ON / OFF (View / Print / Export sub-dicts) |
| `Configuration` | A document configuration: `/D` or one of `/Configs` |
| `BaseState` | ON / OFF / Unchanged starting point for ON/OFF overrides |
| `ListMode` | AllPages / VisiblePages — layer panel scope |
| `LayerTree` / `LayerTreeNode` | Parsed `/Order` hierarchy |
| `AutoStateRule` / `AutoStateEvent` | One `/AS` rule and its event tag |
| `OcgVisibility` | Single / Membership / Expression display-list predicate |
| `MembershipPolicy` | AllOn / AnyOn / AllOff / AnyOff |
| `VisibilityExpr` | And / Or / Not / Layer leaf — `/VE` AST |
| `LayerSet` | Per-render OCG visibility overrides |
| `RenderIntent` | View / Print / Export — `/AS` rule selector |

## Method reference

| Method | Returns | Description |
|--------|---------|-------------|
| `layers()` | `&[Layer]` | Every OCG in the document |
| `layer(ocg_id)` | `Option<&Layer>` | Look up by object number |
| `configurations()` | `&[Configuration]` | Default + alternate configs |
| `default_configuration()` | `Option<&Configuration>` | The `/D` config |
| `configuration(idx)` | `Option<&Configuration>` | Lookup by index |
| `layer_tree()` | `LayerTree` | Default config's `/Order` |
| `layer_set_for(intent)` | `LayerSet` | Intent-driven LayerSet from `/AS` rules |
| `render_page_to_rgba_with_layers(page, dpi, &set)` | `(Vec<u8>, u32, u32)` | Render with overrides |

Free functions in `stet_pdf_reader::layers`:

- `layer_set_from_document(doc) -> LayerSet`
- `layer_set_from_configuration(doc, idx) -> Option<LayerSet>`
- `layer_set_for(doc, intent) -> LayerSet` (the helper behind
  `PdfDocument::layer_set_for`)
