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

## Three layers, and none of them is the others

Fuzzing keeps getting filed as a test. It is not one: it is stochastic,
unbounded in time, and has no pass state — only "has not failed yet". Every
piece of trouble this setup has caused came from forcing it into a
test-shaped slot. The layers, each with one job:

| layer | what it is | where |
|---|---|---|
| **Gate** | deterministic, fast, blocks a release | `cargo test` |
| **Search** | stochastic, produces *findings* not verdicts | `run.sh`, `fuzz.yml` |
| **Asset** | the corpus — accumulates value, needs maintenance | `minimize.sh` |

**The gate is `cargo test`, and nothing here is.** Every crash the fuzzer has
found is promoted into `crates/stet-fonts/tests/malformed_font_guards.rs` or
`crates/stet-pdf-reader/tests/malformed_input_guards.rs` and runs in
hundredths of a second on any machine. That is the durable artifact of
fuzzing — not the corpus, and not a campaign someone ran once. **Do not add a
fuzzing step to the release process**; that was tried on 2026-08-31 and
reverted the same day (see CLAUDE.md).

### `replay.sh` — a tool, not a gate

Replays every saved corpus input once with `-runs=0` and no mutation, so it is
deterministic: same corpus, same answer. Useful after touching a parser —
thousands of hostile inputs through it in minutes.

```sh
./fuzz/replay.sh                  # every target
./fuzz/replay.sh fuzz_pdf_parse   # one target
```

It is **not** a release gate, because it is only as good as whatever corpus
happens to be on the machine. `fuzz/corpus/` is gitignored, so on a fresh
clone it would exit 0 having checked almost nothing — a gate that silently
weakens is worse than none. An absent or empty corpus is therefore a hard
failure rather than a pass; run `seed-corpus.sh` first.

### `minimize.sh` — corpus maintenance

libFuzzer replays the **entire** corpus before `-max_total_time` means
anything, so an unpruned corpus spends the budget reloading instead of
mutating. Measured 2026-08-31, before this script existed: `fuzz_pdf_parse`
did 237 executions in a 300s run, of which ~210 were the startup replay —
about 27 actual mutations in five minutes.

```sh
./fuzz/minimize.sh                # every target
./fuzz/minimize.sh fuzz_pdf_parse # one target
```

First run, 2026-08-31 — 341 MB down to 132 MB in 3m17s:

| target | before | after |
|---|---|---|
| `fuzz_font_cff` | 4,721 files / 849 KB | 1,659 / 269 KB |
| `fuzz_font_type1` | 1,221 files / 51 MB | 463 / 15 MB |
| `fuzz_ps_tokenizer` | 1,503 files / 173 MB | 504 / 38 MB |
| `fuzz_pdf_parse` | 192 files / 93 MB | 183 / 79 MB |
| `fuzz_font_truetype` | 16 files / 3.3 KB | 14 / 3.3 KB |

What it bought, measured the same day on identical 300s runs:

| target | before cmin | after cmin |
|---|---:|---:|
| `fuzz_pdf_parse` | 237 executions | 320 executions |
| `fuzz_font_cff` | 6,273,359 | 5,673,930 |

So: **+35% on the target that needed it, and nothing on the one that did
not** — `fuzz_font_cff` is within noise, which is the expected answer for a
target whose whole corpus is 269 KB.

`fuzz_pdf_parse` barely shrinks and only modestly speeds up, and that is the
honest result: those are 183 genuinely distinct real PDFs and nearly every one
earns its place on coverage. Its startup cost is not bloat, so cmin cannot
remove it. The lever there is a **longer budget**, since the replay is a fixed
cost — 180s of a 300s run is 60% overhead, but 180s of a 1500s run is 12%.
That is why `fuzz.yml` runs 1500s per target.

**Run this periodically, not once.** The corpus regrew from 132 MB to 166 MB
during the two 300s runs used to take the measurements above — `fuzz_pdf_parse`
went 183 → 262 files in ten minutes of fuzzing. Growth is the corpus doing its
job; cmin is the maintenance that keeps the growth from eating the budget.
Before a long campaign is the natural time.

### The corpus is irreplaceable, and lives on one machine

`fuzz/corpus/` is gitignored and accumulates only where fuzzing is run. Nothing
in a clone reproduces it: `fuzz_font_cff` has no seeds in the tree at all
(there is not one `.cff`/`.otf`/`.ttf` file in the repo), so every one of its
inputs was invented by the fuzzer. CI reseeds from the sample trees each run
and discards what it finds, by design — the accumulating copy is local.
Archive it if you care about it.

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
