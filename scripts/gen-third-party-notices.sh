#!/usr/bin/env bash
# stet - A PostScript Interpreter
# Copyright (c) 2026 Scott Bowman
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Write the third-party notices for one prebuilt `stet` binary to stdout.
#
# Usage:
#   scripts/gen-third-party-notices.sh TARGET [CARGO FEATURE FLAGS...]
#
# e.g. scripts/gen-third-party-notices.sh x86_64-unknown-linux-musl --no-default-features
#
# Pass the same target and feature flags the binary is built with: the musl
# build has no viewer, and the crate graph -- so the licence list -- differs.
#
# Two sources, because cargo-about only sees crate licences:
#   1. Third-party data stet embeds with include_bytes!, which a crate's
#      licence field does not describe: the URW++ fonts (AGPL-3.0 with a
#      font exception) and Adobe's CMap and glyph-list data (BSD-3-Clause).
#      Their licence files live next to the data, in the crate that embeds it.
#   2. Every Rust crate linked into the binary, from cargo-about, using
#      about.toml (which licences are accepted -- a new one fails the run)
#      and about.hbs (the plain-text layout).
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 TARGET [CARGO FEATURE FLAGS...]" >&2
    exit 2
fi
target=$1
shift

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

rule() {
    printf '%s\n' "================================================================================"
}

section() {
    rule
    printf '%s\n' "$1"
    rule
    printf '\n'
    cat "$2"
    printf '\n'
}

# Run cargo-about first: a failure (an unaccepted licence) must stop the
# script before anything is printed, and the header depends on its output.
crates="$(cargo about generate --locked --fail \
    -m crates/stet-cli/Cargo.toml \
    --target "$target" \
    "$@" \
    about.hbs)"

cat <<HEADER
Third-party notices for the stet binary ($target)

stet itself is licensed Apache-2.0 OR MIT (LICENSE-APACHE, LICENSE-MIT).
This binary also contains the third-party material listed below, under the
terms reproduced here: first the data stet embeds, then every Rust crate
compiled into it.

HEADER

# The IJG licence requires executable-only distributions to say this in
# their documentation. Printed only while an IJG-licensed crate is linked.
if printf '%s\n' "$crates" | grep -q '(IJG)$'; then
    printf '%s\n\n' "This software is based in part on the work of the Independent JPEG Group."
fi

section "URW++ base 35 fonts -- embedded by stet and stet-pdf-reader" \
    crates/stet/LICENSE-URW-FONTS
section "Adobe CMap resources (CID <-> Unicode tables) -- embedded by stet-fonts" \
    crates/stet-fonts/LICENSE-ADOBE-CMAP
section "Adobe Glyph List and ITC Zapf Dingbats Glyph List -- embedded by stet-fonts" \
    crates/stet-fonts/LICENSE-ADOBE-AGL

rule
printf '%s\n' "Rust crates"
rule
printf '%s\n' "$crates"
