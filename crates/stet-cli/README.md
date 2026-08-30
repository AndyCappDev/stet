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

### Page size for PostScript input

A plain `%!PS` program is rendered onto whatever page the device provides,
US Letter by default. `%%BoundingBox` does **not** change that — DSC defines
it as a description of the artwork, not a page-size request, and Ghostscript
behaves the same way. `--page` supplies the size from outside:

```bash
stet --device png --page 620x1000 broadside.ps   # explicit, in points
stet --device png --page a4 report.ps            # named size
stet --device png --page a4-landscape report.ps  # swap the dimensions
```

EPS is the exception: an `EPSF` header or a `.eps` extension makes stet
honour `%%BoundingBox`, and `--page` overrides it when both apply. For PDF
input the size comes from the document, so use `--width` / `--height` instead.

### Untrusted input

PostScript is Turing-complete, so there is no time limit by default and a
hostile program can loop forever. Set one, and cap PostScript VM, when the
input is not yours:

```bash
stet --device png --timeout 30 --max-vm 2048 untrusted.ps
```

Both are ceilings on the interpreter, not on rendering resolution. Run
untrusted input inside an OS-level sandbox as well.

### Inspecting a PDF

`stet inspect` prints a document's structure — metadata, page boxes, outline,
annotations, form fields, embedded files, and optional-content layers —
without rendering it.

```bash
stet inspect document.pdf
stet inspect --password secret encrypted.pdf
```

## Options

```
stet [OPTIONS] <FILE>...
stet inspect <FILE.pdf> [--password <PW>]
stet --help
stet --version

Output devices:
  --device <DEVICE>          png, pdf, viewer, viewport-png, null
                             (default: viewer for files when built with the
                             viewer feature, png otherwise)

Common options:
  -o, --output <PATH>        Write output to PATH instead of alongside the
                             input. A "%d" token in PATH becomes the page
                             number ("%03d" zero-pads to three digits);
                             without one, PATH names a single file and a job
                             producing a second page is an error. Takes a
                             single input file
  --dpi <DPI>                Resolution for raster output (default 300)
  --page <SIZE>              Page size for PostScript/EPS input, in points:
                             a named size (letter, legal, tabloid, ledger,
                             executive, a0-a6, b4, b5) or WIDTHxHEIGHT.
                             Append -landscape or -portrait to a named size.
                             Rejected for PDF input
  --pages <RANGE>            Page selection: 3, 1-5, 1-3,7,10-12
  --width <PX>               Override page width (PDF input only; not
                             combinable with --dpi or --page)
  --height <PX>              Override page height (same restrictions)
  --threads <N>              Worker-thread count (default: 75% of cores in
                             viewer mode, 8 otherwise)
  --password <PW>            Password for encrypted PDF input
  --no-aa                    Disable anti-aliasing

Resource limits (for untrusted input):
  --timeout <SECONDS>        Abort a job running longer than this. No limit
                             by default — PostScript is Turing-complete and
                             legitimate jobs can run for minutes
  --max-vm <MB>              Ceiling on PostScript VM: strings, arrays, and
                             dictionaries (default 8192). Exceeding it raises
                             VMerror instead of aborting. Separate from the
                             renderer's image and band buffers, so this does
                             not cap rendering resolution

Colour management:
  --no-icc                   Disable ICC colour management; use the PLRM
                             CMYK→sRGB formulas. Cannot combine with
                             --cmyk-profile or --bpc
  --output-profile <FILE>    ICC output profile (also used as source CMYK
                             when --cmyk-profile is not set)
  --cmyk-profile <FILE>      Pin the source CMYK ICC profile for CMYK→sRGB
  --use-output-intent        Honour the PDF's embedded /OutputIntents profile
                             as the source CMYK profile (default)
  --no-output-intent         Ignore it and use the system CMYK profile
  --bpc <on|off|auto>        Black-point compensation (default: auto,
                             currently equivalent to on)
```

`stet --help` prints the same list; `scripts/check-cli-docs.sh` in the
repository keeps the two from drifting apart.

PDF reading (`stet-pdf-reader`) is always available. PostScript
interpretation, `stet-render`, PDF output (`stet-pdf`), and the viewer are
built in as well. The interactive viewer can be disabled at build time.

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `viewer` | yes | Build the interactive egui window (zoom, pan, minimap, drag-and-drop). Disable for a headless CLI. |

## License

Apache-2.0 OR MIT
