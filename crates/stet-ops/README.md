# stet-ops

[![crates.io](https://img.shields.io/crates/v/stet-ops.svg)](https://crates.io/crates/stet-ops)
[![docs.rs](https://img.shields.io/docsrs/stet-ops)](https://docs.rs/stet-ops)

PostScript operator implementations for the stet interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

331 PostScript Level 3 operator implementations — 328 always registered,
plus three `pdfmark` operators on the PDF output path:

| Category | Count | Examples |
|----------|-------|---------|
| Stack | 11 | `pop`, `dup`, `exch`, `roll`, `mark` |
| Math | 26 | `add`, `mul`, `sqrt`, `sin`, `atan`, `rand` |
| Relational / Boolean / Bitwise | 11 | `eq`, `gt`, `and`, `not`, `bitshift` |
| Type / Conversion | 14 | `type`, `cvx`, `cvn`, `cvi`, `cvr` |
| Dictionary | 15 | `dict`, `begin`, `def`, `load`, `known` |
| Control flow | 14 | `exec`, `if`, `for`, `loop`, `stopped` |
| String / Array / Composite | 15 | `get`, `put`, `length`, `getinterval`, `forall` |
| File / Output / Filter | 28 | `file`, `read`, `write`, `token`, `print`, `filter` |
| Path construction / query | 21 | `moveto`, `curveto`, `arc`, `pathbbox`, `infill` |
| Graphics state | 18 | `gsave`, `grestore`, `setlinewidth`, `setdash` |
| Color / Transfer | 13 | `setgray`, `setrgbcolor`, `setcolorspace`, `settransfer` |
| Painting | 7 | `fill`, `stroke`, `showpage`, `erasepage` |
| Clipping | 7 | `clip`, `eoclip`, `rectclip`, `clippath` |
| Font / Show | 25 | `findfont`, `scalefont`, `show`, `awidthshow`, `glyphshow` |
| Matrix | 16 | `translate`, `scale`, `rotate`, `concat` |
| Userpath | 11 | `ufill`, `ustroke`, `uappend`, `upath` |
| Halftone / Screen | 5 | `setscreen`, `sethalftone`, `setcolorscreen` |
| Image | 3 | `image`, `imagemask`, `colorimage` |
| Pattern / Shading | 4 | `makepattern`, `setpattern`, `shfill` |
| Resource | 5 | `findresource`, `defineresource`, `resourceforall` |
| VM | 7 | `save`, `restore`, `vmstatus`, `setglobal` |
| Page device | 7 | `setpagedevice`, `currentpagedevice`, `nulldevice` |
| PDF-imaging extensions | 5 | `.setalpha`, `.setblendmode`, `.begintransparencygroup` |
| Misc / internal | 40 | `usertime`, `realtime`, `version`, interpreter internals |

Counts are the `register()` calls in `build_system_dict`, grouped by that
function's own section comments; they sum to 328. `systemdict` exposes 388
operators at runtime — the difference is defined by the Init PostScript
resources. Three more (`pdfmark` and friends) are registered only on the PDF
output path, via `register_pdf_authoring_ops`.

## Usage

```rust
use stet_ops::build_system_dict;

let mut ctx = stet_core::context::Context::new();
build_system_dict(&mut ctx);  // registers all operators into systemdict
```

## License

Apache-2.0 OR MIT
