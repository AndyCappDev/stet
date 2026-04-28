#!/usr/bin/env bash
# Audit the documented match-surface enums for `#[non_exhaustive]`.
#
# Per `docs/PLAN-NON-EXHAUSTIVE.md` and the policy section in `CLAUDE.md`,
# enums in the files listed below are stable extension points and must
# carry `#[non_exhaustive]` so new variants can land additively for
# downstream renderers, custom output devices, and PDF-reading tools.
#
# This script greps each listed file for `pub enum` and verifies that
# the immediately preceding non-blank line contains `#[non_exhaustive]`.
# Run it manually or wire it into `.git/hooks/pre-push` alongside the
# fmt/clippy checks.
#
# Exit status: 0 = all enums marked, 1 = at least one unmarked.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Curated list of files whose public enums must be `#[non_exhaustive]`.
# Keep in sync with the "Stable extension points" section in CLAUDE.md.
FILES=(
    crates/stet-graphics/src/display_list.rs
    crates/stet-graphics/src/device.rs
    crates/stet-graphics/src/color.rs
    crates/stet-core/src/error.rs
    crates/stet-core/src/file_store.rs
    crates/stet-core/src/pdfmark.rs
    crates/stet-pdf-reader/src/error.rs
    crates/stet-pdf-reader/src/destination.rs
    crates/stet-pdf-reader/src/annotations.rs
    crates/stet-pdf-reader/src/form_fields.rs
    crates/stet-pdf-reader/src/metadata.rs
    crates/stet-pdf-reader/src/viewer_prefs.rs
    crates/stet-pdf-reader/src/embedded_files.rs
    crates/stet-pdf-reader/src/diagnostics.rs
    crates/stet-pdf-reader/src/layers/mod.rs
    crates/stet-pdf-reader/src/layers/metadata.rs
    crates/stet-pdf-reader/src/layers/configuration.rs
)

# Enums that are intentionally NOT marked `#[non_exhaustive]`. These
# are user-constructed (LayerSet/OcgVisibility/VisibilityExpr — callers
# build them to drive layer visibility) or interpreter-internal helpers
# whose closed set of variants is part of their meaning. New entries
# here need a one-line justification.
ALLOWLIST=(
    # display_list.rs — interpreter-internal helpers; OcgVisibility et al.
    # are user-constructed payloads, see docs/PDF-LAYERS.md.
    "SoftMaskSubtype"
    "GroupColorSpace"
    "OcgVisibility"
    "MembershipPolicy"
    "VisibilityExpr"
    # device.rs — alt-space discriminator, only ever used inside
    # SpotColorSpace payloads.
    "SimpleColorSpace"
    # file_store.rs — runtime VM file handle. External callers see
    # opaque file IDs through PostScript operators, not this enum.
    "FileHandle"
)

violations=0

for f in "${FILES[@]}"; do
    if [[ ! -f "$f" ]]; then
        echo "warning: $f listed but not present (was it renamed?)" >&2
        continue
    fi

    # awk pass: walk the file remembering whether we recently saw
    # `#[non_exhaustive]`, and on each `pub enum` line check that the
    # marker appeared on a contiguous run of attribute / blank lines
    # immediately above. Non-attribute non-blank lines reset the flag.
    # Allowlisted enums are exempt.
    awk -v file="$f" -v allow="${ALLOWLIST[*]}" '
        BEGIN {
            n = split(allow, parts, " ")
            for (i = 1; i <= n; i++) allowed[parts[i]] = 1
        }
        /^[[:space:]]*#\[non_exhaustive\]/ { saw_marker = 1; next }
        /^[[:space:]]*#\[/                 { next }
        /^[[:space:]]*$/                   { next }
        /^[[:space:]]*\/\/\//              { next }
        /^[[:space:]]*\/\//                { next }
        /^pub enum [A-Za-z_]/ {
            name = $3
            sub(/[<{].*$/, "", name)
            if (!saw_marker && !(name in allowed)) {
                printf "%s:%d: pub enum %s missing #[non_exhaustive]\n",
                    file, NR, name > "/dev/stderr"
                bad++
            }
            saw_marker = 0
            next
        }
        { saw_marker = 0 }
        END { exit (bad ? 1 : 0) }
    ' "$f" || violations=$((violations + 1))
done

if (( violations > 0 )); then
    echo >&2
    echo "ERROR: $violations file(s) contain unmarked public enums." >&2
    echo "       See docs/PLAN-NON-EXHAUSTIVE.md and CLAUDE.md for the policy." >&2
    exit 1
fi

echo "non-exhaustive marker audit: OK (${#FILES[@]} files checked)"
