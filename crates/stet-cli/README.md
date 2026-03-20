# stet-cli

Command-line interface for the stet PostScript interpreter.

## Installation

```bash
cargo install stet-cli
```

## Usage

```bash
# Render PostScript to PNG
stet --device png document.ps

# Render to PDF
stet --device pdf document.ps

# Interactive viewer (default when viewer feature is enabled)
stet document.ps

# Render specific pages
stet --device png --pages 1-3 document.ps

# Interactive REPL
stet

# Render PDF files to PNG
stet --device png document.pdf
```

## Options

```
stet [OPTIONS] [FILES...]

Options:
  --device <DEVICE>    Output device: png, pdf, viewer, null
  --dpi <DPI>          Resolution in dots per inch (overrides device default)
  --pages <RANGE>      Page range, e.g. 1, 1-5, 2,4,6
  --no-icc             Disable ICC color management
  --no-aa              Disable anti-aliasing
  --overprint          Enable overprint simulation
  --profile <FILE>     Use a specific ICC output profile
```

## Features

| Feature | Default | Description |
|---------|---------|------------|
| `viewer` | yes | Interactive egui window with zoom/pan/navigation |
| `pdf-reader` | yes | Open and render PDF files |

## License

Apache-2.0 OR MIT
