# stet-graphics

Graphics types, display list, and ICC color support for the stet PostScript interpreter.

This is a low-level crate. Most users should use the [`stet`](https://crates.io/crates/stet)
facade crate instead.

## Contents

- **`color`** — `DeviceColor`, CIE color space parameters, color conversion
- **`device`** — Rendering parameter structs (`FillParams`, `StrokeParams`, `ImageParams`, etc.), `PageSink`/`PageSinkFactory` traits
- **`display_list`** — `DisplayList` and `DisplayElement` for deferred rendering
- **`icc`** — `IccCache` for ICC color profile management (uses `moxcms`)
- **`mesh_shading`** — Binary mesh and patch shading parsers (Types 4-7)

## Usage

```rust
use stet_graphics::display_list::{DisplayList, DisplayElement};
use stet_graphics::icc::IccCache;
use stet_graphics::device::{FillParams, StrokeParams};
```

## License

Apache-2.0 OR MIT
