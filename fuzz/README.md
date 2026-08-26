# Fuzzing stet

Coverage-guided fuzz targets for the parsers that consume untrusted input.

This crate is **not** a workspace member. `cargo-fuzz` builds on libFuzzer,
which needs nightly, while stet is stable-only with a pinned MSRV — so `fuzz`
is listed in the root `Cargo.toml`'s `exclude`, exactly like `stet-wasm`.
`cargo build`, `cargo test`, and the MSRV job never see it.

## Setup

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
./fuzz/seed-corpus.sh
```

## Running

```sh
./fuzz/run.sh                     # every target, 120s each
./fuzz/run.sh 3600                # every target, an hour each
./fuzz/run.sh 3600 fuzz_pdf_parse # one target
```

`run.sh` sets a 90s per-input timeout rather than something tuned to
production speed. That matters: cargo-fuzz builds with AddressSanitizer and
this crate leaves overflow checks and debug assertions on, so the harness runs
roughly 15x slower than the shipping build. An input the release CLI renders
in 0.5s takes about 8s here. The first run of this harness reported two "slow
units" that were nothing of the kind.

**Before filing anything the fuzzer reports, time it through the real binary:**

```sh
cp fuzz/artifacts/fuzz_pdf_parse/slow-unit-xxxx /tmp/repro.pdf
time ./target/release/stet --device png --pages 1 /tmp/repro.pdf
```

## Targets

| Target | Surface |
|---|---|
| `fuzz_pdf_parse` | The widest one: lexer, xref + rebuild, resolver, filters, content interpreter, fonts, shadings, images, plus the structural API (outline / names / fields / layers), which rendering never touches. |
| `fuzz_font_truetype` | Table directory, `cmap` subtable formats, `hmtx`, `loca`/`glyf`, composite-glyph recursion. |
| `fuzz_font_cff` | CFF INDEX/DICT structure, then the Type 2 charstring interpreter the parsed subroutines feed. |
| `fuzz_font_type1` | eexec decryption, header parse, Type 1 charstring interpreter including `seac` (the composite lookup is wired up, so it is actually reachable). |
| `fuzz_ps_tokenizer` | PostScript tokenizer: nested comments, string escapes, ASCII85, hex strings, radix numbers, binary tokens. Stops at tokenization — see below. |

`fuzz_ps_tokenizer` deliberately does not evaluate. Running the interpreter
over arbitrary bytes mostly measures how long a `{...} loop` takes to time
out, which is a resource-governor question (SECURITY.md, Priority 1) rather
than a parser-crash question. Revisit once an execution budget exists.

## Corpora

Not committed — bulky, machine-generated, and derived from files already in the
repo. `seed-corpus.sh` rebuilds them:

| Target | Seeds |
|---|---|
| `fuzz_pdf_parse` | `pdf_samples/`, `more_pdf_samples/` (703 files) |
| `fuzz_ps_tokenizer` | `ps_samples/`, `unit_tests/`, `ps_corpus/files/` (6410 files) |
| `fuzz_font_type1` | The 35 shipped URW faces (35 files) |
| `fuzz_font_truetype`, `fuzz_font_cff` | None in tree — these start cold. Extracting embedded font programs out of the PDF corpus would help them considerably. |

## Known non-findings

`[profile.release.package.*]` in `Cargo.toml` disables overflow checks for
three crates. Without those the fuzzer stops at the first one it reaches and
never explores past it — the PDF target hit the `hayro-jbig2` one within
seconds, since the sample that triggers it is a seed.

- **`hayro-jbig2` 0.1.0** — a real upstream overflow in `region/text.rs`
  (JBIG2 6.4.5 symbol placement). Tracked in SECURITY.md for an upstream
  report; not fixable here.
- **`stet-tiny-skia`, `stet-tiny-skia-path`** — SIMD-lane emulation, modular
  by definition, matching the hardware instructions the aarch64 paths use.

## When a target finds something

1. Reproduce: `cargo +nightly fuzz run <target> <artifact>`.
2. Check it is not sanitizer-only slowness (above).
3. Minimise: `cargo +nightly fuzz tmin <target> <artifact>`.
4. Promote it into a real regression test — `crates/stet-pdf-reader/tests/malformed_input_guards.rs`
   or `crates/stet-fonts/tests/malformed_font_guards.rs` — rather than leaving
   it in `fuzz/artifacts/`, which is gitignored.
