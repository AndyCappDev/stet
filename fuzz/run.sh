#!/usr/bin/env bash
# Run one or all fuzz targets with defaults that suit this harness.
#
# Usage:
#   fuzz/run.sh                    # every target, 120s each
#   fuzz/run.sh 600                # every target, 600s each
#   fuzz/run.sh 600 fuzz_pdf_parse # one target
#
# On the timeout: cargo-fuzz builds with AddressSanitizer, and this crate's
# release profile also leaves overflow checks and debug assertions on. That
# stack runs roughly 15x slower than the shipping build — an input the release
# CLI renders in 0.5s takes about 8s here. A per-input timeout tuned to
# production speed therefore reports slow units that are not slow at all, which
# is how the first run of this harness produced two false positives. 90s is
# comfortably above the sanitizer-inflated cost of the largest corpus seed
# while still catching a genuine hang.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

duration="${1:-120}"
shift || true
targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  mapfile -t targets < <(cargo +nightly fuzz list)
fi

status=0
for t in "${targets[@]}"; do
  echo "=== $t (${duration}s) ==="
  # -report_slow_units is raised alongside -timeout for the same reason: it
  # defaults to 10s, and under the sanitizer a perfectly ordinary 0.5s corpus
  # seed lands well past that, littering fuzz/artifacts with slow-unit files
  # that are not findings. Four of them on the first run were all real
  # documents rendering in under a second.
  if ! cargo +nightly fuzz run "$t" -- \
      -max_total_time="$duration" \
      -timeout=90 \
      -report_slow_units=60 \
      -rss_limit_mb=4096; then
    echo "!!! $t reported a finding — see fuzz/artifacts/$t/"
    status=1
  fi
done

if [ $status -ne 0 ]; then
  echo
  echo "Reproduce with: cargo +nightly fuzz run <target> <artifact>"
  echo "Before filing, check whether the artifact is slow only under the"
  echo "sanitizer: time it through ./target/release/stet as well."
fi
exit $status
