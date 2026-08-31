#!/usr/bin/env bash
#
# Replay every saved corpus input through its target, once, with no mutation.
#
# This is the half of fuzzing that can be a release gate. `run.sh` is a random
# search: it has no completion criterion, and a campaign that passes tells you
# nothing about whether the next run with a different seed will. Replay is
# deterministic — same corpus, same answer, every time — so it answers a
# question a release can actually block on: does anything we already have an
# input for now crash?
#
# It is fast for the same reason. `-runs=0` replays the corpus and exits
# rather than mutating, so the cost is the corpus, not a time budget.
#
# Usage:
#     ./fuzz/replay.sh                     # every target
#     ./fuzz/replay.sh fuzz_pdf_parse ...  # named targets only
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  mapfile -t targets < <(cargo +nightly fuzz list)
fi

status=0
missing=0

for t in "${targets[@]}"; do
  corpus="fuzz/corpus/$t"
  # An absent or empty corpus must fail, not pass quietly. `fuzz/corpus/` is
  # gitignored, so a fresh clone has nothing to replay — and a gate that
  # silently succeeds on an empty input set is worse than no gate at all.
  if [ ! -d "$corpus" ] || [ -z "$(ls -A "$corpus" 2>/dev/null)" ]; then
    echo "!!! $t: no corpus at $corpus"
    missing=1
    status=1
    continue
  fi

  echo "=== $t ($(ls -A "$corpus" | wc -l | tr -d ' ') inputs) ==="
  if ! cargo +nightly fuzz run "$t" "$corpus" -- \
      -runs=0 \
      -timeout=90 \
      -rss_limit_mb=4096; then
    echo "!!! $t crashed replaying its corpus — see fuzz/artifacts/$t/"
    status=1
  fi
done

if [ "$missing" -ne 0 ]; then
  echo
  echo "Populate the corpora first: ./fuzz/seed-corpus.sh"
fi

if [ "$status" -ne 0 ]; then
  echo
  echo "Reproduce with: cargo +nightly fuzz run <target> <artifact>"
fi

exit $status
