#!/usr/bin/env bash
#
# Minimise each corpus to the smallest subset that preserves its coverage.
#
# The corpus is the accumulated value of every hour ever spent fuzzing, and it
# is also the thing that quietly destroys throughput if nothing prunes it.
# libFuzzer replays the *entire* corpus before `-max_total_time` means
# anything, so a bloated corpus spends the budget reloading instead of
# mutating.
#
# Measured on 2026-08-31, before this script existed: `fuzz_pdf_parse` held
# 192 files totalling 93 MB, the largest a single 10.7 MB PDF. A 300s run did
# 237 executions — of which ~210 were the corpus replay, leaving about 27
# actual mutations in five minutes. The target was not fuzzing, it was reading
# files.
#
# `cmin` keeps the inputs that contribute coverage and drops the rest, so the
# corpus stays worth what it costs to load. Run it periodically, and before
# any long campaign.
#
# Usage:
#     ./fuzz/minimize.sh                     # every target
#     ./fuzz/minimize.sh fuzz_pdf_parse ...  # named targets only
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  mapfile -t targets < <(cargo +nightly fuzz list)
fi

# `du -sh` is for humans; this is for arithmetic.
corpus_bytes() { du -sb "$1" 2>/dev/null | cut -f1; }
corpus_files() { find "$1" -type f 2>/dev/null | wc -l | tr -d ' '; }
human() { numfmt --to=iec --suffix=B "$1" 2>/dev/null || echo "${1}B"; }

status=0
for t in "${targets[@]}"; do
  corpus="fuzz/corpus/$t"
  if [ ! -d "$corpus" ] || [ -z "$(ls -A "$corpus" 2>/dev/null)" ]; then
    echo "=== $t: no corpus, skipping (run ./fuzz/seed-corpus.sh) ==="
    continue
  fi

  before_f=$(corpus_files "$corpus")
  before_b=$(corpus_bytes "$corpus")
  echo "=== $t: $before_f files, $(human "$before_b") ==="

  if ! cargo +nightly fuzz cmin "$t" -- -rss_limit_mb=4096; then
    echo "!!! $t: cmin failed — corpus left as it was"
    status=1
    continue
  fi

  after_f=$(corpus_files "$corpus")
  after_b=$(corpus_bytes "$corpus")
  echo "    -> $after_f files, $(human "$after_b")" \
       "(dropped $((before_f - after_f)) files, $(human $((before_b - after_b))))"
done

exit $status
