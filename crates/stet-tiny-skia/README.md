# stet-tiny-skia

[![crates.io](https://img.shields.io/crates/v/stet-tiny-skia.svg)](https://crates.io/crates/stet-tiny-skia)
[![docs.rs](https://img.shields.io/docsrs/stet-tiny-skia)](https://docs.rs/stet-tiny-skia)

A modified fork of [tiny-skia](https://github.com/RazrFalcon/tiny-skia)
for use in the stet PostScript and PDF rendering engine.

## Changes from upstream tiny-skia

**Analytical antialiasing**: Replaced tiny-skia's 4x4 supersampling
(16 samples per pixel) with a sub-row cell accumulation approach using
256 horizontal subpixels x 4 sub-rows (1024 effective samples per pixel).
This eliminates the "fat stroke" appearance on thin lines and diagonal
edges that is visible at PostScript's typical hairline widths.

## Why a fork?

PostScript rendering demands high-quality antialiasing at all stroke
widths, including sub-pixel hairlines. The upstream 16-sample
supersampling produces visible stepping artifacts on thin diagonal
lines that are common in technical illustrations, font outlines, and
EPS figures. The analytical coverage approach provides smooth edges
without the 64x cost of brute-force supersampling to match.

The fork is published as a separate crate (`stet-tiny-skia`) to avoid
conflicts with the upstream `tiny-skia` on crates.io.

## Upstream

Based on [tiny-skia](https://github.com/RazrFalcon/tiny-skia) by
[Yevhenii Reizner](https://github.com/RazrFalcon). tiny-skia is a
Skia subset ported to Rust — a minimal, CPU-only 2D rendering library
focused on quality, speed, and small binary size.

## License

[New BSD License](./LICENSE) (same as upstream tiny-skia and Skia)
