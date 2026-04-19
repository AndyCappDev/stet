# stet-ops

PostScript operator implementations for the stet interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

~320 PostScript Level 3 operators organized by category:

| Category | Count | Examples |
|----------|-------|---------|
| Stack | 11 | `pop`, `dup`, `exch`, `roll`, `mark` |
| Math | 24 | `add`, `mul`, `sqrt`, `sin`, `atan` |
| Relational / Boolean | 11 | `eq`, `gt`, `and`, `not`, `bitshift` |
| Type / Conversion | 14 | `type`, `cvx`, `cvn`, `cvi`, `cvr` |
| Dictionary | 14 | `dict`, `begin`, `def`, `load`, `known` |
| Control | 13 | `exec`, `if`, `for`, `loop`, `stopped` |
| String / Array | 8 | `get`, `put`, `length`, `getinterval` |
| File / Output | 24 | `file`, `read`, `write`, `token`, `print` |
| Path | 13 | `moveto`, `lineto`, `curveto`, `arc`, `closepath` |
| Graphics State | 18 | `gsave`, `grestore`, `setlinewidth`, `setdash` |
| Color | 12 | `setgray`, `setrgbcolor`, `setcmykcolor`, `setcolorspace` |
| Painting | 7 | `fill`, `stroke`, `showpage`, `image` |
| Clipping | 7 | `clip`, `eoclip`, `rectclip`, `clippath` |
| Font / Show | 18 | `findfont`, `scalefont`, `show`, `awidthshow`, `glyphshow` |
| Matrix | 16 | `translate`, `scale`, `rotate`, `concat` |
| Filter | 1+ | `filter` (ASCIIHex, ASCII85, Flate, LZW, DCT, etc.) |
| Shading | 1 | `shfill` (all 7 shading types) |
| Resource | 8 | `findresource`, `defineresource` |
| VM | 7 | `save`, `restore`, `vmstatus` |

## Usage

```rust
use stet_ops::build_system_dict;

let mut ctx = stet_core::context::Context::new();
build_system_dict(&mut ctx);  // registers all operators into systemdict
```

## License

Apache-2.0 OR MIT
