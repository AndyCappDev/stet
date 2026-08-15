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

# `stet inspect` sample output: `Producer: stet X.Y.Z`. The device really
# does write the version now (pdf_device.rs `default_producer`), so these
# samples are checkable fact, not decoration. The check is scoped to lines
# with `Producer:` so unrelated `stet 0.…` references (e.g. install snippets
# that pin `stet = "0.2"`) don't trigger.
#
# Every doc carrying the sample is checked, not just README.md —
# docs/PDF-READER-API.md sat at 0.2.0 through two releases because it wasn't.
for doc in README.md docs/PDF-READER-API.md; do
    [ -f "$doc" ] || continue
    if grep -q "Producer: stet " "$doc" && ! grep -q "Producer: stet ${ws_version}" "$doc"; then
        echo -e "${RED}check-release-versions: ${doc} 'Producer:' sample doesn't match 'stet ${ws_version}'${RESET}" >&2
        grep -n "Producer: stet " "$doc" | head -3 >&2 || true
        errors=$((errors + 1))
    fi
done

# MSRV: `[workspace.package] rust-version` vs the README badge. The badge
# claimed 1.85 for months while the real floor was 1.88 — first-party code
# uses let-chains in 282 places — because nothing tied the two together and
# nothing ever compiled on the claimed toolchain. The `msrv` CI job proves
# the number is buildable; this proves the README still says the same number.
ws_msrv=$(awk '
    /^\[workspace\.package\]/ { in_ws = 1; next }
    /^\[/ { in_ws = 0 }
    in_ws && /^rust-version = / { gsub(/rust-version = "|"/, ""); print; exit }
' Cargo.toml)

if [ -z "$ws_msrv" ]; then
    echo -e "${RED}check-release-versions: failed to read rust-version from [workspace.package]${RESET}" >&2
    errors=$((errors + 1))
else
    if ! grep -q "Rust-${ws_msrv}+-" README.md; then
        echo -e "${RED}check-release-versions: README.md badge URL doesn't include 'Rust-${ws_msrv}+-'${RESET}" >&2
        grep -n "Rust-" README.md | head -3 >&2 || true
        errors=$((errors + 1))
    fi
    if ! grep -q "alt=\"Rust ${ws_msrv}+\"" README.md; then
        echo -e "${RED}check-release-versions: README.md badge alt text doesn't match 'Rust ${ws_msrv}+'${RESET}" >&2
        grep -n "alt=\"Rust" README.md | head -3 >&2 || true
        errors=$((errors + 1))
    fi
    # The prose in the MSRV section has to agree with the badge too.
    if ! grep -q "requires \*\*Rust ${ws_msrv}\*\*" README.md; then
        echo -e "${RED}check-release-versions: README.md MSRV prose doesn't say 'requires **Rust ${ws_msrv}**'${RESET}" >&2
        grep -n "requires \*\*Rust" README.md | head -3 >&2 || true
        errors=$((errors + 1))
    fi
    # The CI job pins the toolchain literally; it must pin what we claim.
    if ! grep -q "toolchain: \"${ws_msrv}\"" .github/workflows/ci.yml; then
        echo -e "${RED}check-release-versions: ci.yml msrv job doesn't pin toolchain '${ws_msrv}'${RESET}" >&2
        grep -n "toolchain:" .github/workflows/ci.yml | head -5 >&2 || true
        errors=$((errors + 1))
    fi

    # Every publishable crate must carry rust-version, or crates.io and
    # docs.rs show no MSRV for it and cargo can't refuse an old toolchain
    # on the consumer's behalf. `cargo package` bakes the inherited value
    # into the published manifest, so inheriting is enough — but a crate
    # added later that forgets the line would publish silently without it,
    # which is exactly how 0.4.0 shipped with rust_version: null.
    #
    # The vendored tiny-skia forks are skipped: edition 2018, published on
    # their own cadence, and they carry upstream's much lower floor.
    for manifest in crates/*/Cargo.toml; do
        crate=$(basename "$(dirname "$manifest")")
        case "$crate" in
            stet-tiny-skia|stet-tiny-skia-path) continue ;;
        esac
        if ! grep -qE "^rust-version(\.workspace = true| = \"${ws_msrv}\")" "$manifest"; then
            echo -e "${RED}check-release-versions: ${crate} has no rust-version — it would publish without an MSRV${RESET}" >&2
            echo -e "${YELLOW}  Add 'rust-version.workspace = true' to ${manifest}.${RESET}" >&2
            errors=$((errors + 1))
        fi
    done
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

echo -e "${GREEN}check-release-versions: OK (workspace ${ws_version} matches README + CHANGELOG; MSRV ${ws_msrv} matches badge + ci.yml)${RESET}"
