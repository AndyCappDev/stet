#!/usr/bin/env bash
# Keep the `v*` tag namespace reserved for releases.
#
# This exists because it did not, twice. `v0.6.1-perf` and `v0.6.2-perf` both
# pointed at one commit from 2026-02-28, when the workspace was at 0.1.0 — the
# names never referred to a version at all. They sorted into `git tag`
# alongside real releases and made it look as though 0.6.0 had shipped and
# been superseded twice, which is exactly the confusion hit while preparing
# the real 0.6.0. Nothing in the repo could have said "that is not a release".
#
# The rule: **a tag starting with `v` is a release and matches `vX.Y.Z`
# exactly.** Anything else — benchmarks, experiments, throwaway markers —
# belongs under a `perf/` or `exp/` prefix, which git allows, which sorts
# separately, and which can never be mistaken for a version.
#
# Two checks, both on local tags:
#   1. every `v*` tag is strictly `vX.Y.Z` — no suffixes, no pre-release tails
#   2. every `vX.Y.Z` tag has a matching `## [X.Y.Z]` entry in CHANGELOG.md,
#      so a tag cannot exist for a version that was never written up
#
# `.githooks/pre-push` applies the same two rules to the refs actually being
# pushed, which is the check that stops a bad tag escaping the machine.
#
# Usage: scripts/check-tags.sh
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

release_re='^v[0-9]+\.[0-9]+\.[0-9]+$'
errors=0

mapfile -t vtags < <(git tag --list 'v*' | sort -V)

if [ "${#vtags[@]}" -eq 0 ]; then
  echo "check-tags: no v* tags"
  exit 0
fi

for tag in "${vtags[@]}"; do
  if ! [[ "$tag" =~ $release_re ]]; then
    echo "  $tag — not a release tag, but occupies the v* namespace" >&2
    echo "      rename it: git tag perf/${tag#v} $tag && git tag -d $tag" >&2
    errors=$((errors + 1))
    continue
  fi
  version="${tag#v}"
  if ! grep -q "^## \[${version}\]" CHANGELOG.md; then
    echo "  $tag — no '## [${version}]' entry in CHANGELOG.md" >&2
    errors=$((errors + 1))
  fi
done

if [ "$errors" -ne 0 ]; then
  cat >&2 <<'MSG'

check-tags: the v* namespace is reserved for releases.

Use `perf/<name>` or `exp/<name>` for benchmark and experiment markers. Git
allows the slash, they sort away from `v*`, and they cannot be mistaken for a
version that shipped.
MSG
  exit 1
fi

echo "check-tags: ${#vtags[@]} release tag(s), all well-formed and in CHANGELOG.md"
