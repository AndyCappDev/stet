# stet-cli

[![crates.io](https://img.shields.io/crates/v/stet-cli.svg)](https://crates.io/crates/stet-cli)
[![docs.rs](https://img.shields.io/docsrs/stet-cli)](https://docs.rs/stet-cli)

Command-line interface for stet — the `stet` binary renders PostScript,
EPS, and PDF files to PNG, PDF, or an interactive desktop viewer.

## Installation

```bash
cargo install stet-cli
```

## Usage

stet auto-detects the input format (PS, EPS, PDF). The same commands work
against every source.

### PDF

```bash
# PDF → PNG (one file per page at 300 DPI)
stet --device png document.pdf

# Pages 1–3 at 150 DPI
stet --device png --pages 1-3 --dpi 150 document.pdf

# Honour the PDF's embedded /OutputIntents (PDF/X-4 etc.) — default behaviour
stet --device png document.pdf

# Override with a specific source-CMYK profile
stet --device png --cmyk-profile /path/to/FOGRA39.icc document.pdf

# Open a PDF in the interactive viewer (default when built with the viewer feature)
stet document.pdf
```

### PostScript / EPS

```bash
# PostScript → PNG
stet --device png document.ps

# PostScript → PDF
stet --device pdf document.ps

# Render specific pages
stet --device png --pages 1-3 document.ps

# Interactive viewer, REPL on first `showpage`
stet document.ps
stet                       # no files → REPL; viewer opens when PS calls showpage
```

### Mixed batches

```bash
# Render a mix of sources in one invocation
stet --device png page1.ps page2.pdf illustration.eps
```

## Options

```
stet [OPTIONS] [FILES...]

Options:
  --device <DEVICE>          Output device: png, pdf, viewer, null
  --dpi <DPI>                Resolution in dots per inch (overrides device default)
  --pages <RANGE>            Page range, e.g. 1, 1-5, 2,4,6
  --threads <N>              Worker-thread count (default: 75% of cores in viewer
                             mode, 8 otherwise)
  --no-icc                   Disable ICC color management
  --no-aa                    Disable anti-aliasing

Colour management:
  --output-profile <FILE>    ICC output profile (also used as source CMYK when
                             --cmyk-profile is not set)
  --cmyk-profile <FILE>      Pin the source CMYK ICC profile for CMYK→sRGB
  --no-output-intent         Ignore the PDF's embedded /OutputIntents profile
                             (default: honoured)
  --bpc <on|off|auto>        Black-point compensation (default: auto,
                             currently equivalent to on)
```

PDF reading (`stet-pdf-reader`) is always available. PostScript
interpretation, `stet-render`, PDF output (`stet-pdf`), and the viewer are
built in as well. The interactive viewer can be disabled at build time.

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `viewer` | yes | Build the interactive egui window (zoom, pan, minimap, drag-and-drop). Disable for a headless CLI. |

## License

Apache-2.0 OR MIT
