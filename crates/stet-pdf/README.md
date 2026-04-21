# stet-pdf

[![crates.io](https://img.shields.io/crates/v/stet-pdf.svg)](https://crates.io/crates/stet-pdf)
[![docs.rs](https://img.shields.io/docsrs/stet-pdf)](https://docs.rs/stet-pdf)

PDF output device for the stet PostScript interpreter.

Converts display lists into print-production-quality PDF files. The display
list carries the full fidelity of the PostScript program — color spaces,
transfer functions, halftone screens, overprint settings, spot colors — and
stet-pdf preserves these into the PDF rather than flattening to RGB.

## What Gets Preserved

The display list is the source of truth. stet-pdf translates each element
and its associated graphics state into the corresponding PDF constructs:

### Color

| Display list | PDF output |
|-------------|------------|
| DeviceCMYK fills/strokes | DeviceCMYK color operators (native CMYK preserved) |
| DeviceRGB fills/strokes | DeviceRGB color operators |
| DeviceGray fills/strokes | DeviceGray color operators |
| Separation (spot) colors | PDF Separation color space with tint transform |
| DeviceN (multi-ink) colors | PDF DeviceN color space with tint transform |
| ICCBased image color spaces | ICCBased color space with embedded profile |
| CIEBasedABC/A colors | CalRGB/CalGray color spaces |
| Indexed color spaces | PDF Indexed color space (lookup table preserved) |
| Rendering intent | PDF rendering intent operator (`ri`) |

### Print Production

| Display list | PDF output |
|-------------|------------|
| Overprint flag | ExtGState with `/OP`, `/op`, `/OPM` |
| Transfer functions | ExtGState with `/TR2` (Type 0 sampled functions) |
| Halftone screens | ExtGState with `/HT` (Type 1/5 halftone dictionaries) |
| Black generation / UCR | ExtGState with `/BG2`, `/UCR2` (sampled functions) |
| Trim box | Per-page `/TrimBox` |

### Images

| Display list | PDF output |
|-------------|------------|
| DeviceGray images | Flate-compressed, DeviceGray color space |
| DeviceRGB images | Flate-compressed, DeviceRGB color space |
| DeviceCMYK images | Flate-compressed, DeviceCMYK color space |
| ICCBased images | Flate-compressed with embedded ICC profile |
| Indexed images | Indexed color space with lookup table |
| Separation/DeviceN images | Separation/DeviceN color space with tint transform |
| Image masks | PDF image mask (stencil) with paint color |
| CIEBasedABC/A images | CalRGB/CalGray color space |

Image data is Flate-compressed. The native color space from the display
list is preserved — CMYK images stay CMYK, spot color images keep their
Separation color space.

### Fonts

| Source | PDF output |
|--------|------------|
| Type 1 fonts | Subsetted Type 1 with FontFile, Widths, ToUnicode CMap |
| CFF / Type 2 fonts | Type1C FontFile3 with ToUnicode CMap |
| TrueType / Type 42 fonts | CIDFontType2 with FontFile2 (reconstructed TrueType binary) |
| CID fonts (Type 0) | Type 0 → CIDFontType2 hierarchy with CIDToGIDMap |

Fonts are subsetted to include only glyphs used in the document. ToUnicode
CMaps are generated for text extraction and search in PDF viewers.

### Shadings

All seven PostScript shading types are converted to PDF shading objects:
Type 2 (axial), Type 3 (radial), Types 4/5 (triangle mesh),
Types 6/7 (Coons/tensor patch mesh). Shading color spaces are preserved.

### Transparency

PostScript has no transparency model, so the PS interpreter never produces
transparency group or soft mask display elements. These element types exist
in the display list for the PDF reader's benefit (PDF 1.4+) and are not
encountered in the PS→PDF output path.

## API

Most users should use `stet::Interpreter::render_to_pdf()` from the
[`stet`](https://crates.io/crates/stet) facade crate.

For direct use:

```rust
use stet_pdf::PdfDevice;

ctx.device_factory = Some(Box::new(move |w, h| {
    Box::new(PdfDevice::new(w, h, 300.0))
}));
```

For in-memory PDF generation (no file I/O):

```rust
let bytes = pdf_device.take_pdf_bytes();
let bytes = pdf_device.take_pdf_bytes_with_context(&ctx);
```

## License

Apache-2.0 OR MIT
