# stet-viewer

Interactive egui/winit viewer for the stet PostScript interpreter.

Displays rendered PostScript pages in a desktop window with zoom, pan,
page navigation, and a minimap. The interpreter runs on a background thread
and sends display lists to the viewer via channels.

## Contents

- **`run_viewer()`** — Launch the viewer window (blocks until closed)
- **`create_channels()`** — Create matched channel pairs for interpreter ↔ viewer communication
- **`ViewerMsg`** — Message types: `Page`, `NewJob`, `JobDone`
- **`PageReady`** — Page ready for display (display list + dimensions)
- **Channel endpoints** — `InterpreterEnd` and `ViewerEnd` for bidirectional communication

## Features

- On-demand viewport rendering at arbitrary zoom levels
- Multi-threaded banded rendering with cancellation
- Drag-and-drop file opening (PS, EPS, PDF)
- Minimap navigation
- Full-page background pre-rendering

## Usage

This crate is typically used through `stet-cli` with `--device viewer` or as the
default output device. It is not intended for direct library use.

## License

Apache-2.0 OR MIT
