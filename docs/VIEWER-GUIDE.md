# Viewer Guide

The stet viewer is an interactive desktop window for viewing PostScript, EPS,
and PDF files. It renders pages on demand at any zoom level using the display
list pipeline.

## Launching the Viewer

```bash
# Open a file in the viewer (default device when viewer feature is enabled)
stet document.ps
stet illustration.eps
stet report.pdf

# Multiple files (space/right arrow to advance between them)
stet page1.ps page2.ps page3.ps

# Explicit device selection
stet --device viewer document.ps
```

When no files are given, the viewer waits in the background while the
PostScript REPL runs in the terminal. The viewer window appears as soon
as the first `showpage` is executed. Each subsequent `showpage` adds
another page. Note: drag-and-drop is not available in REPL mode.

## Keyboard Controls

### Navigation

| Key | Action |
|-----|--------|
| Space, Right Arrow, Enter | Next page |
| Left Arrow, Backspace | Previous page |

### Zoom

| Key | Action |
|-----|--------|
| `+` / `=` | Zoom in (1.25x per press) |
| `-` | Zoom out (1.25x per press) |
| `0` | Reset to fit-to-window |
| `1` | Zoom to 150 DPI |
| `2` | Zoom to 300 DPI |
| `3` | Zoom to 600 DPI |
| `4` | Zoom to 1200 DPI |
| `5` | Zoom to 2400 DPI |
| `6` | Zoom to 4800 DPI |
| `7` | Zoom to 9600 DPI |

Zoom is centered on the mouse cursor position. There is no maximum zoom
limit — display lists render at any resolution.

### Other

| Key | Action |
|-----|--------|
| `Q`, Escape | Quit |
| Up Arrow | Pan up (when zoomed) |
| Down Arrow | Pan down (when zoomed) |

## Mouse Controls

| Action | Effect |
|--------|--------|
| Scroll wheel | Zoom in/out (centered on cursor) |
| Click and drag | Pan (when zoomed in) |

## Minimap

When zoomed in past the window size, a minimap appears in the corner
showing the full page with a rectangle indicating the visible area.

- Click anywhere on the minimap to jump to that position
- Drag the viewport rectangle to pan

## Drag and Drop

Drop a file onto the viewer window to open it (available when the viewer
was launched with files, not from the REPL). Supported formats:
- PostScript (`.ps`)
- Encapsulated PostScript (`.eps`, `.epsf`)
- PDF (`.pdf`)

The window title updates to show the dropped file name.

Dropping a new file while another is still being parsed or loaded cancels
the in-flight job immediately. The interpreter aborts at the next eval
iteration (PS/EPS) or the next page boundary (PDF), any already-prepared
but not-yet-displayed pages are discarded, and the dropped file's pages
start arriving in their place. Rapid drops coalesce — only the most
recent file is processed.

### Password-protected PDFs

When a dropped (or CLI-launched) PDF is encrypted, a modal dialog opens
asking for the password. Type it and press **Enter** (or click **Open**)
to continue, or press **Esc** / click **Cancel** to skip the file. If
the password is wrong, the dialog stays up with an "Incorrect password —
try again" notice.

For non-interactive workflows (headless rendering, scripting) pass the
password on the command line with `--password <pw>`. The flag applies
to the initial CLI files only — drag-dropped files still show the modal
prompt so one run can open several differently-keyed PDFs.

## Status Bar

The bottom status bar shows:
- **Page N of M** — current page and total (shows `M+` while pages are still being interpreted)
- **DPI** — effective rendering resolution at the current zoom level
- **Zoom** — zoom percentage relative to fit-to-window
- **Keyboard shortcut reference**

## How It Works

The viewer uses on-demand viewport rendering:

1. The PostScript interpreter (or PDF reader) runs on a background thread
   and sends display lists to the viewer as pages are produced.
2. The viewer stores display lists in memory and renders only the visible
   portion at the current zoom level.
3. When you zoom or pan, only the affected region is re-rendered — the
   PostScript is not re-interpreted.
4. At high zoom levels, rendering is split into horizontal bands processed
   in parallel for responsiveness.
5. A full-page render runs in the background for smooth scrolling; partial
   results appear as bands complete.

## DPI and Display Quality

The `--dpi` setting controls the resolution of the display list, not just
the output pixels. PostScript paths are transformed to device coordinates
at the specified DPI during interpretation, so the display list is built
in that coordinate space. For the best quality, set `--dpi` to match your
intended final output resolution:

```bash
stet document.ps              # Uses device default (300 DPI for png/viewer/pdf)
stet --dpi 150 document.ps    # Lower resolution for faster screen viewing
stet --dpi 600 document.ps    # High-resolution print proofing
```

Each output device defines its own default DPI in its pagedevice dictionary
(all built-in devices default to 300). The `--dpi` flag overrides this.

When you zoom in past the reference DPI, you are magnifying device-space
coordinates — the geometry is exact, but rasterization happens at the
higher effective DPI.

## CLI Options Affecting the Viewer

| Option | Effect |
|--------|--------|
| `--dpi <DPI>` | Set the reference DPI (affects initial zoom level) |
| `--threads <N>` | Override the rayon worker count (default: 75% of cores in viewer mode) |
| `--pages <RANGE>` | Only render specified pages (e.g. `1-5`, `3`, `1-3,7,10-12`) |
| `--password <PW>` | Open a password-protected PDF with the given user password |
| `--no-aa` | Disable anti-aliasing |

### Color management

| Option | Effect |
|--------|--------|
| `--no-icc` | Disable ICC colour management entirely; CMYK falls back to the PLRM formulas |
| `--cmyk-profile <PATH>` | Pin the source CMYK ICC profile used for CMYK→sRGB conversion |
| `--output-profile <PATH>` | Generic output profile; also used as the source CMYK profile when `--cmyk-profile` is not given |
| `--no-output-intent` | Ignore the PDF's `/OutputIntents[].DestOutputProfile` (default: honoured) |
| `--use-output-intent` | Kept for backward compatibility; OutputIntent is honoured by default |
| `--bpc <on\|off\|auto>` | Black-point compensation for CMYK→sRGB (default: `auto`, currently equivalent to `on`; `off` skips BPC) |

Precedence for the source CMYK profile is `--cmyk-profile` > `--output-profile` >
PDF OutputIntent (unless `--no-output-intent`) > system default. `--no-icc`
conflicts with `--cmyk-profile` and `--bpc`.
