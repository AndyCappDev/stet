#!/usr/bin/env bash
# Populate fuzz seed corpora from the sample trees.
#
# Coverage-guided fuzzing from zero bytes spends most of its budget
# rediscovering file formats. Seeding with real documents starts it inside the
# interesting region: a valid xref table, a working font program, a content
# stream that actually draws.
#
# Corpora are NOT committed — they are bulky, machine-generated, and derived
# from files already in the repo. Run this after a clone.
#
# Usage: fuzz/seed-corpus.sh
# SEED_LIMIT caps how many files land in each corpus. Unset means all of them,
# which is what a local campaign wants. CI wants a small number, because
# libFuzzer runs the *entire* seed corpus once before `-max_total_time` starts
# to mean anything: 703 PDFs rendered under AddressSanitizer is far longer than
# the 60s smoke budget, so an uncapped seed turns the CI job into a 20-minute
# corpus replay that never fuzzes at all.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

limit="${SEED_LIMIT:-0}"

seed() {
  local target="$1"; shift
  local dir="fuzz/corpus/$target"
  mkdir -p "$dir"
  for src in "$@"; do
    [ -e "$src" ] || continue
    if [ "$limit" -gt 0 ] && [ "$(find "$dir" -type f | wc -l)" -ge "$limit" ]; then
      break
    fi
    cp -n "$src" "$dir/" 2>/dev/null || true
  done
  echo "  $target: $(find "$dir" -type f | wc -l | tr -d ' ') files"
}

echo "Seeding fuzz corpora..."

# PDFs: the sample corpus, plus any PDFs sitting next to the PostScript ones.
seed fuzz_pdf_parse pdf_samples/*.pdf more_pdf_samples/*.pdf ps_samples/*.pdf

# PostScript: the sample programs, the unit-test suite, and the PS corpus
# files. Between them these reach far more of the tokenizer's odd corners —
# binary tokens, radix numbers, unbalanced string escapes — than the samples
# alone.
seed fuzz_ps_tokenizer ps_samples/*.ps ps_samples/*.eps unit_tests/*.ps

# `ps_corpus/files` nests its 6268 inputs in per-generator subdirectories, so a
# glob only matches the directories. Recurse instead. These dominate the
# corpus by count and are the most valuable seeds it has — they are real
# generator output (Ghostscript, distiller variants) rather than hand-written
# samples.
if [ -d ps_corpus/files ]; then
  mkdir -p fuzz/corpus/fuzz_ps_tokenizer
  # Flatten the path into the filename. A plain `cp` would silently drop most
  # of these: the generator subdirectories reuse basenames, so copying by
  # basename collapses 6268 inputs down to about 900.
  # Process substitution, not a pipe: breaking out of `find | while` closes the
  # pipe, `find` takes SIGPIPE, and `set -o pipefail` turns that into a 141 that
  # `set -e` treats as fatal — which silently skipped the font seeding below.
  while read -r f; do
    if [ "$limit" -gt 0 ] && \
       [ "$(find fuzz/corpus/fuzz_ps_tokenizer -type f | wc -l)" -ge "$limit" ]; then
      break
    fi
    flat="$(echo "${f#ps_corpus/files/}" | tr '/' '_')"
    cp -n "$f" "fuzz/corpus/fuzz_ps_tokenizer/$flat" 2>/dev/null || true
  done < <(find ps_corpus/files -type f)
  echo "  fuzz_ps_tokenizer: $(find fuzz/corpus/fuzz_ps_tokenizer -type f | wc -l | tr -d ' ') files (after ps_corpus)"
fi

# Fonts: the 35 shipped URW Type 1 faces.
seed fuzz_font_type1 crates/stet/resources/Font/*

echo
echo "Note: fuzz_font_truetype and fuzz_font_cff have no seeds in-tree."
echo "They will still run, just from a colder start."
