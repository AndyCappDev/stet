# Contributing to stet

Working on stet itself. For using it — install, CLI options, library API —
see the [README](README.md) and [docs/LIBRARY-USAGE.md](docs/LIBRARY-USAGE.md).

## Building from Source

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests (1130 passing)
cargo run -- file.ps           # Run a PostScript file
cargo run                      # Interactive REPL
cargo clippy                   # Lint
```

There is a second suite written in PostScript itself, exercising operator
behaviour through the interpreter rather than through Rust. CI runs it as its
own gate and it exits non-zero on failure:

```bash
cargo build --release
./target/release/stet unit_tests/ps_tests.ps   # 68 files, 2813 assertions
```

### Git hooks

One-time setup after cloning, if you intend to push:

```bash
git config core.hooksPath .githooks
```

This enables `.githooks/pre-push`, which runs the same gates as CI's
lint job — `cargo fmt --check`, clippy errors, the `#[non_exhaustive]`
and VM level-zero-allocation audits, the version/MSRV cross-check, and
the CLI documentation cross-check, the tag-namespace audit — plus a
`wasm32` cross-compile, which
catches the one class of error that passes everywhere else (`usize` is
32-bit there). The wasm step is skipped with a warning if the target is
not installed. A failing tree is caught before it reaches a remote rather
than a few minutes later in CI. Bypass a single
push with `git push --no-verify`.

Git does not enable this automatically, and it fails silently: without
the command above, pushes simply succeed with nothing checked.

### Tag naming

**`vX.Y.Z` is reserved for releases** — exactly that shape, with a matching
`## [X.Y.Z]` entry in `CHANGELOG.md`. Benchmark and experiment markers use a
`perf/` or `exp/` prefix instead:

```bash
git tag perf/aa-256x4          # not v0.6.1-perf
git tag exp/glyph-cache
```

Git allows the slash, those sort away from `v*`, and they can never be
mistaken for a version that shipped. `scripts/check-tags.sh` audits existing
tags and the pre-push hook rejects a malformed one before it reaches a
remote.

### WASM Viewer

```bash
cd web && ./build.sh           # Build WASM module
python3 serve.py               # Serve at localhost:8000
```

## Visual-regression testing

`cargo test` runs with no extra setup. The PDF visual-regression
harness (`./pdf_visual_test.sh`) needs a local corpus of test PDFs,
which the project doesn't ship (most are third-party). To reproduce
PDF-rendering bugs or check for regressions across a large corpus:

```bash
# 1. Fetch public test corpora into pdf_samples/ (clones with
#    sparse-checkout so only the PDFs are pulled).
./scripts/fetch_test_pdfs.sh            # all corpora
./scripts/fetch_test_pdfs.sh --list     # see what's available

# 2. Generate your local baseline on a known-good commit
#    (typically `main` before your changes).
./pdf_visual_test.sh --baseline

# 3. Switch to your feature branch and compare.
./pdf_visual_test.sh
```

Any PDFs you already have at the top level of `pdf_samples/` keep
working; the fetcher drops new corpora into their own subdirs
(e.g. `pdf_samples/pdfjs/`) and the visual-test harness walks the
tree so both flat and subdir layouts are picked up. Corpus
subdirectories are gitignored — nothing third-party lands in a
commit.
