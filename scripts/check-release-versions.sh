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

# Release-asset download URLs must name the exact current version.
#
# Unlike a dependency snippet, which stays correct across a patch release
# because cargo resolves "0.8" to the newest 0.8.x, a download URL names a
# file that either exists or 404s. `curl -L .../v0.8.0/stet-0.8.0-...tar.gz`
# is wrong the instant 0.8.1 ships, and it is on the front page, aimed at
# someone deciding whether stet is worth their time. Full version here, not
# just the minor.
url_bad=$(grep -rn 'releases/download/v[0-9]' README.md crates/*/README.md docs/*.md 2>/dev/null \
    | grep -v "releases/download/v${ws_version}/" || true)
# Archive *filenames* travel with the URL and carry the version too -- in a
# shell variable, an unpack command, and the directory it produces. One rule
# for any `stet-X.Y.Z-<target>` string catches all of them, so a new snippet
# shape cannot slip past by not being a `cd` line.
dir_bad=$(grep -rnoE 'stet-[0-9]+\.[0-9]+\.[0-9]+-[a-z0-9_]+' README.md crates/*/README.md docs/*.md 2>/dev/null \
    | grep -v ":stet-${ws_version}-" || true)
if [ -n "$url_bad" ] || [ -n "$dir_bad" ]; then
    echo -e "${RED}check-release-versions: release-asset path(s) not on ${ws_version}${RESET}" >&2
    [ -n "$url_bad" ] && echo "$url_bad" >&2
    [ -n "$dir_bad" ] && echo "$dir_bad" >&2
    echo -e "${YELLOW}  These name a file on the releases page; a stale one 404s.${RESET}" >&2
    errors=$((errors + 1))
fi

# Dependency snippets must name the current minor.
#
# These are what a reader copies off crates.io, and nothing checked them:
# `stet = "0.2"` sat in six places across three releases, telling new users
# to pin a version two minors behind. Only major.minor is checked — a
# snippet saying "0.4" stays correct through 0.4.1, which is how cargo
# resolves it anyway.
#
# `docs/*.md` is in scope because the library docs moved there: the bulk of
# the README's library section became docs/LIBRARY-USAGE.md, snippets and
# all. Scanning only the READMEs would have quietly stopped covering them —
# which is the exact failure this check exists to prevent.
ws_minor="${ws_version%.*}"
snippet_bad=$(grep -rnE '^[[:space:]]*stet[a-z-]* = "[0-9]+\.[0-9]+"|version = "[0-9]+\.[0-9]+", default-features' \
    README.md crates/*/README.md docs/*.md 2>/dev/null | grep -v "\"${ws_minor}\"" || true)
if [ -n "$snippet_bad" ]; then
    echo -e "${RED}check-release-versions: dependency snippet(s) not on ${ws_minor}${RESET}" >&2
    echo "$snippet_bad" >&2
    errors=$((errors + 1))
fi

# crates/stet-wasm/Cargo.lock pins the stet crates by version.
#
# It goes stale at every bump even though stet-wasm's own version is not
# moving, and nothing else notices: stet-wasm is `exclude`d from the
# workspace, so the root `cargo build` never reads that lock, and the rest of
# this script looks at docs rather than lockfiles. During the 0.8.1 bump it
# still pinned 0.8.0 across eight crates *after* this script had reported OK;
# it was found by grep. Refresh with:
#
#     cd crates/stet-wasm && cargo update --workspace --offline
#
# Only stet's own entries are checked. Third-party pins are cargo's business,
# and stet-wasm's own `version` is deliberately on an independent cadence, so
# both are skipped. The root Cargo.lock needs no rule: any `cargo build`
# refreshes it, so it cannot be stale in a tree that compiles.
wasm_lock="crates/stet-wasm/Cargo.lock"
if [ -f "$wasm_lock" ]; then
    wasm_bad=$(awk -v want="$ws_version" '
        /^name = "/ { gsub(/name = "|"/, ""); name = $0; next }
        /^version = / && name ~ /^stet/ && name != "stet-wasm" &&
        name !~ /^stet-tiny-skia/ {
            gsub(/version = "|"/, "")
            if ($0 != want) print "  " name " = " $0
        }
    ' "$wasm_lock")
    if [ -n "$wasm_bad" ]; then
        echo -e "${RED}check-release-versions: ${wasm_lock} pins stet crates off ${ws_version}${RESET}" >&2
        echo "$wasm_bad" >&2
        echo -e "${YELLOW}  Refresh: (cd crates/stet-wasm && cargo update --workspace --offline)${RESET}" >&2
        errors=$((errors + 1))
    fi
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

echo -e "${GREEN}check-release-versions: OK (workspace ${ws_version} matches README + CHANGELOG + wasm lock; MSRV ${ws_msrv} matches badge + ci.yml)${RESET}"
