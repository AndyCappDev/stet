#!/usr/bin/env bash
# Verify the workspace `Cargo.toml` version matches the README badge,
# the README sample-output Producer string, and that CHANGELOG.md has
# an entry for the current version. Catches the "bumped Cargo.toml,
# forgot to bump README + CHANGELOG" mistake before it reaches the
# remote / crates.io.
#
# Wired into the pre-push hook and into `.github/workflows/ci.yml`'s
# fmt+clippy job. Bypass at the hook level with `git push --no-verify`
# if you really know what you're doing.

set -euo pipefail

if [ -t 1 ]; then
    RED="\033[31m"; GREEN="\033[32m"; YELLOW="\033[33m"; RESET="\033[0m"
else
    RED=""; GREEN=""; YELLOW=""; RESET=""
fi

# Workspace version lives at `[workspace.package] version = "X.Y.Z"`.
ws_version=$(awk '
    /^\[workspace\.package\]/ { in_ws = 1; next }
    /^\[/ { in_ws = 0 }
    in_ws && /^version = / { gsub(/version = "|"/, ""); print; exit }
' Cargo.toml)

if [ -z "$ws_version" ]; then
    echo -e "${RED}check-release-versions: failed to read workspace version from Cargo.toml${RESET}" >&2
    exit 1
fi

errors=0

# README.md badge: shields.io URL contains `Version-X.Y.Z-`. Also check
# the alt text for an exact `Version X.Y.Z` match.
if ! grep -q "Version-${ws_version}-" README.md; then
    echo -e "${RED}check-release-versions: README.md badge URL doesn't include 'Version-${ws_version}-'${RESET}" >&2
    grep -n "Version-" README.md | head -3 >&2 || true
    errors=$((errors + 1))
fi
if ! grep -q "alt=\"Version ${ws_version}\"" README.md; then
    echo -e "${RED}check-release-versions: README.md badge alt text doesn't match 'Version ${ws_version}'${RESET}" >&2
    grep -n "alt=\"Version" README.md | head -3 >&2 || true
    errors=$((errors + 1))
fi

# README.md `stet inspect` sample: `Producer: stet X.Y.Z`. The check is
# scoped to lines with `Producer:` so unrelated `stet 0.…` references
# (e.g. install snippets that pin `stet = "0.2"`) don't trigger.
if grep -q "Producer: stet " README.md && ! grep -q "Producer: stet ${ws_version}" README.md; then
    echo -e "${RED}check-release-versions: README.md 'Producer:' sample doesn't match 'stet ${ws_version}'${RESET}" >&2
    grep -n "Producer: stet " README.md | head -3 >&2 || true
    errors=$((errors + 1))
fi

# CHANGELOG.md: must have an `## [X.Y.Z]` heading.
if ! grep -q "^## \[${ws_version}\]" CHANGELOG.md; then
    echo -e "${RED}check-release-versions: CHANGELOG.md is missing a '## [${ws_version}]' entry${RESET}" >&2
    grep -nE "^## \[[0-9]+" CHANGELOG.md | head -5 >&2 || true
    errors=$((errors + 1))
fi

if [ "$errors" -gt 0 ]; then
    echo -e "${RED}check-release-versions: ${errors} mismatch(es) found vs Cargo.toml workspace version ${ws_version}${RESET}" >&2
    echo -e "${YELLOW}  Fix the docs and re-push, or bypass with 'git push --no-verify' if you really mean it.${RESET}" >&2
    exit 1
fi

echo -e "${GREEN}check-release-versions: OK (workspace ${ws_version} matches README + CHANGELOG)${RESET}"
