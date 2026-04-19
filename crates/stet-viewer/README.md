# stet-viewer

Interactive desktop viewer (egui / winit) for **PostScript, EPS, and PDF**
files, backed by the stet rendering pipeline.

The viewer accepts display lists from either the stet PostScript
interpreter or [`stet-pdf-reader`](https://crates.io/crates/stet-pdf-reader)
— both produce the same
[`DisplayList`](https://docs.rs/stet-graphics/latest/stet_graphics/display_list/struct.DisplayList.html)
type — so a single window handles PS, EPS, and PDF input with no mode
switch and no separate PDF viewer. Zoom, pan, and page navigation run
off the stored display list; pages are never re-interpreted when the
user zooms or moves.

## What the viewer does

- **Supports PS, EPS, and PDF.** Drag-and-drop any of the three formats
  onto the window; the viewer auto-detects and routes the input to the
  appropriate interpreter / parser.
- **On-demand viewport rendering** via
  [`stet-render`](https://crates.io/crates/stet-render) — any rectangular
  region at any zoom level, with no re-interpretation.
- **Multi-threaded banded rendering** with cancellation, so zoom/pan
  feels responsive even at extreme zoom levels or on large pages.
- **Minimap** overlay when zoomed past the window size; click or drag to
  jump.
- **Full-page background pre-rendering** so scrolling a zoomed page
  stays smooth.
- **Per-page CMYK buffer tracking** when the source declares a
  DeviceCMYK page group — blend-mode math then runs in the
  spec-correct colour space.

See the [Viewer Guide](https://github.com/AndyCappDev/stet/blob/main/docs/VIEWER-GUIDE.md)
for keyboard / mouse controls, DPI presets, and minimap behaviour.

## Typical use: the `stet` CLI

Most users don't touch this crate directly — they use the viewer via
the [`stet-cli`](https://crates.io/crates/stet-cli) binary:

```bash
cargo install stet-cli

stet document.ps          # PostScript / EPS
stet document.pdf         # PDF
stet                      # no args → REPL, viewer opens on first showpage
stet a.ps b.pdf c.eps     # batch — Space/Enter advances between files
```

## Library API

The viewer runs on the main thread; the interpreter (or PDF reader)
runs on a background thread and streams display lists into the viewer
over channels. The public API reflects that split:

| Item | Role |
|------|------|
| [`run_viewer`] | Enter the viewer event loop. Blocks until the window closes. |
| [`create_channels`] | Build the matched `(InterpreterEnd, ViewerEnd)` pair that wires the two threads together. |
| [`ViewerMsg`] | Interpreter → viewer messages (`Page`, `NewJob`, `JobDone`). |
| [`PageReady`] | A single page's display list + pixel dimensions + reference DPI, ready for display. |

[`run_viewer`]: https://docs.rs/stet-viewer/latest/stet_viewer/fn.run_viewer.html
[`create_channels`]: https://docs.rs/stet-viewer/latest/stet_viewer/fn.create_channels.html
[`ViewerMsg`]: https://docs.rs/stet-viewer/latest/stet_viewer/enum.ViewerMsg.html
[`PageReady`]: https://docs.rs/stet-viewer/latest/stet_viewer/struct.PageReady.html

For a worked example of driving the viewer directly, see
[`stet-cli`'s `run_viewer_mode`](https://github.com/AndyCappDev/stet/blob/main/crates/stet-cli/src/main.rs)
— it wires up both the PS interpreter thread and the PDF-rendering
thread to a single viewer.

## License

Apache-2.0 OR MIT
