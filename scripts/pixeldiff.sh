#!/usr/bin/env bash
# scripts/pixeldiff.sh — Pixel comparison between stet and GhostScript
#
# Usage:
#   ./scripts/pixeldiff.sh [OPTIONS] [FILE ...]
#
# If no files specified, compares all samples/*.ps and samples/*.eps.
#
# Options:
#   -d, --dpi DPI        Render resolution (default: 150)
#   -t, --threshold TH   RMSE threshold 0.0-1.0 (default: 0.05)
#   --timeout SECS       Per-file timeout (default: 120)
#   --keep               Keep rendered images in tests/pixeldiff/
#   --gs-only            Only render GhostScript references (skip stet + comparison)
#   --stet-only          Only render stet (skip GS, compare against existing refs)
#   --pages N            Only compare first N pages (default: all)
#   -v, --verbose        Show detailed per-page output
#   -h, --help           Show this help

set -uo pipefail

# --- Configuration ---
DPI=300
THRESHOLD=0.05
TIMEOUT=120
KEEP=false
VERBOSE=false
GS_ONLY=false
STET_ONLY=false
MAX_PAGES=0  # 0 = all
STET="./target/release/stet"
WORKDIR="tests/pixeldiff"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# --- Parse arguments ---
FILES=()
while [[ $# -gt 0 ]]; do
    case $1 in
        -d|--dpi) DPI="$2"; shift 2 ;;
        -t|--threshold) THRESHOLD="$2"; shift 2 ;;
        --timeout) TIMEOUT="$2"; shift 2 ;;
        --keep) KEEP=true; shift ;;
        --gs-only) GS_ONLY=true; shift ;;
        --stet-only) STET_ONLY=true; shift ;;
        --pages) MAX_PAGES="$2"; shift 2 ;;
        -v|--verbose) VERBOSE=true; shift ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        -*) echo "Unknown option: $1"; exit 1 ;;
        *) FILES+=("$1"); shift ;;
    esac
done

# --- Verify dependencies ---
for cmd in gs magick; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "Error: $cmd not found. Install GhostScript and ImageMagick."
        exit 1
    fi
done

if [[ ! -x "$STET" ]]; then
    echo "Building stet (release)..."
    cargo build --release --quiet 2>&1
fi

# --- Collect files ---
if [[ ${#FILES[@]} -eq 0 ]]; then
    while IFS= read -r -d '' f; do
        FILES+=("$f")
    done < <(find samples/ -maxdepth 1 \( -name '*.ps' -o -name '*.eps' \) -print0 | sort -z)
fi

if [[ ${#FILES[@]} -eq 0 ]]; then
    echo "No files to compare."
    exit 1
fi

# --- Setup work directory ---
mkdir -p "$WORKDIR/reference" "$WORKDIR/stet" "$WORKDIR/diffs"

# --- Counters ---
TOTAL=0
PASSED=0
FAILED=0
ERRORS=0
SKIPPED=0

# Per-file results for summary
declare -a RESULT_NAMES=()
declare -a RESULT_STATUS=()
declare -a RESULT_DETAIL=()

# --- Helper: render with GhostScript ---
render_gs() {
    local file="$1"
    local basename="$2"
    local outdir="$WORKDIR/reference/$basename"
    mkdir -p "$outdir"

    local gs_args=(-dBATCH -dNOPAUSE -dQUIET -sDEVICE=png16m "-r$DPI")

    # For EPS files, use -dEPSCrop to match stet's auto-sizing
    if [[ "$file" == *.eps || "$file" == *.EPS ]]; then
        gs_args+=(-dEPSCrop)
    fi

    gs_args+=("-sOutputFile=$outdir/page-%04d.png" "$file")

    if timeout "$TIMEOUT" gs "${gs_args[@]}" 2>/dev/null; then
        return 0
    else
        return 1
    fi
}

# --- Helper: render with stet ---
render_stet() {
    local file="$1"
    local basename="$2"
    local outdir="$WORKDIR/stet/$basename"
    mkdir -p "$outdir"

    # stet outputs alongside input file — run it, then move output files
    local filedir
    filedir=$(dirname "$file")
    local stem
    stem=$(basename "$file")
    stem="${stem%.ps}"
    stem="${stem%.PS}"
    stem="${stem%.eps}"
    stem="${stem%.EPS}"
    stem="${stem%.epsf}"
    stem="${stem%.EPSF}"

    # Run stet
    if timeout "$TIMEOUT" "$STET" --device png --dpi "$DPI" "$file" >/dev/null 2>&1; then
        # Move output PNGs to our work dir
        for png in "$filedir/$stem"-[0-9][0-9][0-9][0-9].png; do
            if [[ -f "$png" ]]; then
                local pagenum
                pagenum=$(basename "$png" | grep -oP '\d{4}(?=\.png$)')
                mv "$png" "$outdir/page-$pagenum.png"
            fi
        done
        return 0
    else
        # Still try to collect any pages that were rendered before timeout/error
        for png in "$filedir/$stem"-[0-9][0-9][0-9][0-9].png; do
            if [[ -f "$png" ]]; then
                local pagenum
                pagenum=$(basename "$png" | grep -oP '\d{4}(?=\.png$)')
                mv "$png" "$outdir/page-$pagenum.png"
            fi
        done
        return 1
    fi
}

# --- Helper: compare pages ---
compare_pages() {
    local basename="$1"
    local gsdir="$WORKDIR/reference/$basename"
    local stetdir="$WORKDIR/stet/$basename"
    local diffdir="$WORKDIR/diffs/$basename"
    mkdir -p "$diffdir"

    local gs_pages=()
    local page_count=0
    local page_pass=0
    local page_fail=0
    local worst_rmse=0
    local detail=""

    # Find all GS reference pages
    for gspage in "$gsdir"/page-[0-9][0-9][0-9][0-9].png; do
        [[ -f "$gspage" ]] || continue
        gs_pages+=("$gspage")
    done

    if [[ ${#gs_pages[@]} -eq 0 ]]; then
        detail="no GS reference pages"
        echo "$detail"
        return 2
    fi

    for gspage in "${gs_pages[@]}"; do
        local pagenum
        pagenum=$(basename "$gspage" .png | sed 's/page-//')
        local stetpage="$stetdir/page-$pagenum.png"

        page_count=$((page_count + 1))

        # Check page limit
        if [[ $MAX_PAGES -gt 0 && $page_count -gt $MAX_PAGES ]]; then
            break
        fi

        if [[ ! -f "$stetpage" ]]; then
            page_fail=$((page_fail + 1))
            detail+="page $pagenum: MISSING; "
            $VERBOSE && echo -e "  page $pagenum: ${RED}MISSING${NC}" >&2
            continue
        fi

        # Check dimensions match
        local gs_dims stet_dims
        gs_dims=$(magick identify -format "%wx%h" "$gspage" 2>/dev/null)
        stet_dims=$(magick identify -format "%wx%h" "$stetpage" 2>/dev/null)

        if [[ "$gs_dims" != "$stet_dims" ]]; then
            page_fail=$((page_fail + 1))
            detail+="page $pagenum: SIZE MISMATCH gs=${gs_dims} stet=${stet_dims}; "
            $VERBOSE && echo -e "  page $pagenum: ${YELLOW}SIZE MISMATCH${NC} gs=${gs_dims} stet=${stet_dims}" >&2
            continue
        fi

        # Compute RMSE
        local rmse_output rmse_val
        rmse_output=$(magick compare -metric RMSE "$stetpage" "$gspage" "$diffdir/page-$pagenum.png" 2>&1 || true)
        # Output format: "12345.6 (0.188235)" — we want the normalized value in parens
        rmse_val=$(echo "$rmse_output" | grep -oP '\([\d.]+\)' | tr -d '()')

        if [[ -z "$rmse_val" ]]; then
            page_fail=$((page_fail + 1))
            detail+="page $pagenum: COMPARE ERROR; "
            $VERBOSE && echo -e "  page $pagenum: ${RED}COMPARE ERROR${NC}: $rmse_output" >&2
            continue
        fi

        # Compare against threshold (using bc for float comparison)
        local pass
        pass=$(echo "$rmse_val <= $THRESHOLD" | bc -l 2>/dev/null || echo "0")

        if [[ "$pass" == "1" ]]; then
            page_pass=$((page_pass + 1))
            $VERBOSE && echo -e "  page $pagenum: ${GREEN}PASS${NC} (RMSE: $rmse_val)" >&2
            # Remove diff image for passing pages unless --keep
            if ! $KEEP; then
                rm -f "$diffdir/page-$pagenum.png"
            fi
        else
            page_fail=$((page_fail + 1))
            detail+="page $pagenum: RMSE=$rmse_val; "
            $VERBOSE && echo -e "  page $pagenum: ${RED}FAIL${NC} (RMSE: $rmse_val > $THRESHOLD)" >&2
        fi

        # Track worst RMSE
        local is_worse
        is_worse=$(echo "$rmse_val > $worst_rmse" | bc -l 2>/dev/null || echo "0")
        if [[ "$is_worse" == "1" ]]; then
            worst_rmse="$rmse_val"
        fi
    done

    # Check for extra stet pages not in GS
    for stetpage in "$stetdir"/page-[0-9][0-9][0-9][0-9].png; do
        [[ -f "$stetpage" ]] || continue
        local pagenum
        pagenum=$(basename "$stetpage" .png | sed 's/page-//')
        if [[ ! -f "$gsdir/page-$pagenum.png" ]]; then
            $VERBOSE && echo -e "  page $pagenum: ${YELLOW}EXTRA${NC} (in stet but not GS)" >&2
        fi
    done

    if [[ $page_fail -eq 0 ]]; then
        [[ -z "$detail" ]] && detail="${page_pass}pp, worst RMSE: $worst_rmse"
        echo "PASS|$detail"
        return 0
    else
        [[ -z "$detail" ]] && detail="${page_fail}/${page_count} pages failed"
        echo "FAIL|$detail"
        return 1
    fi
}

# --- Main loop ---
echo -e "${BOLD}Pixel Comparison: stet vs GhostScript${NC}"
echo -e "DPI: ${DPI}, RMSE threshold: ${THRESHOLD}, timeout: ${TIMEOUT}s"
echo -e "Files: ${#FILES[@]}"
echo ""

for file in "${FILES[@]}"; do
    basename=$(basename "$file")
    stem="${basename%.ps}"
    stem="${stem%.PS}"
    stem="${stem%.eps}"
    stem="${stem%.EPS}"
    stem="${stem%.epsf}"
    stem="${stem%.EPSF}"

    TOTAL=$((TOTAL + 1))

    echo -ne "  ${CYAN}${basename}${NC} ... "

    # Render GS (unless --stet-only and refs exist)
    gs_ok=true
    if ! $STET_ONLY; then
        if ! render_gs "$file" "$stem"; then
            echo -e "${RED}GS FAILED${NC}"
            RESULT_NAMES+=("$basename")
            RESULT_STATUS+=("GS_ERROR")
            RESULT_DETAIL+=("GhostScript render failed")
            ERRORS=$((ERRORS + 1))
            continue
        fi
    else
        # Check that references exist
        if [[ ! -d "$WORKDIR/reference/$stem" ]] || \
           ! ls "$WORKDIR/reference/$stem"/page-*.png &>/dev/null; then
            echo -e "${YELLOW}SKIP${NC} (no GS reference)"
            RESULT_NAMES+=("$basename")
            RESULT_STATUS+=("SKIPPED")
            RESULT_DETAIL+=("no GS reference images")
            SKIPPED=$((SKIPPED + 1))
            continue
        fi
    fi

    if $GS_ONLY; then
        gs_count=$(ls "$WORKDIR/reference/$stem"/page-*.png 2>/dev/null | wc -l)
        echo -e "${GREEN}GS OK${NC} (${gs_count} pages)"
        RESULT_NAMES+=("$basename")
        RESULT_STATUS+=("GS_OK")
        RESULT_DETAIL+=("${gs_count} pages rendered")
        PASSED=$((PASSED + 1))
        continue
    fi

    # Render stet
    stet_ok=true
    if ! render_stet "$file" "$stem"; then
        # Check if any pages were rendered despite the error
        stet_count=$(ls "$WORKDIR/stet/$stem"/page-*.png 2>/dev/null | wc -l)
        if [[ $stet_count -eq 0 ]]; then
            echo -e "${RED}STET FAILED${NC}"
            RESULT_NAMES+=("$basename")
            RESULT_STATUS+=("STET_ERROR")
            RESULT_DETAIL+=("stet render failed (no pages)")
            ERRORS=$((ERRORS + 1))
            continue
        fi
        # Some pages rendered — proceed with comparison
        stet_ok=false
    fi

    # Compare
    $VERBOSE && echo ""
    cmp_result=$(compare_pages "$stem") || true
    cmp_status="${cmp_result%%|*}"
    cmp_detail="${cmp_result#*|}"

    if [[ "$cmp_status" == "PASS" ]]; then
        if $stet_ok; then
            echo -e "${GREEN}PASS${NC} ($cmp_detail)"
        else
            echo -e "${YELLOW}PARTIAL${NC} ($cmp_detail, stet exited with error)"
        fi
        RESULT_NAMES+=("$basename")
        RESULT_STATUS+=("PASS")
        RESULT_DETAIL+=("$cmp_detail")
        PASSED=$((PASSED + 1))
    elif [[ "$cmp_status" == "FAIL" ]]; then
        echo -e "${RED}FAIL${NC} ($cmp_detail)"
        RESULT_NAMES+=("$basename")
        RESULT_STATUS+=("FAIL")
        RESULT_DETAIL+=("$cmp_detail")
        FAILED=$((FAILED + 1))
    else
        echo -e "${YELLOW}ERROR${NC} ($cmp_detail)"
        RESULT_NAMES+=("$basename")
        RESULT_STATUS+=("ERROR")
        RESULT_DETAIL+=("$cmp_detail")
        ERRORS=$((ERRORS + 1))
    fi
done

# --- Cleanup ---
if ! $KEEP; then
    # Remove empty diff directories
    find "$WORKDIR/diffs" -type d -empty -delete 2>/dev/null || true
    # Remove stet renders (they're regenerated each run)
    rm -rf "$WORKDIR/stet"
fi

# --- Summary ---
echo ""
echo -e "${BOLD}═══════════════════════════════════════════════════${NC}"
echo -e "${BOLD}Summary${NC}: $TOTAL files tested"
echo -e "  ${GREEN}PASS${NC}: $PASSED"
echo -e "  ${RED}FAIL${NC}: $FAILED"
echo -e "  ${YELLOW}ERROR${NC}: $ERRORS"
if [[ $SKIPPED -gt 0 ]]; then
    echo -e "  ${YELLOW}SKIP${NC}: $SKIPPED"
fi
echo ""

# List failures
if [[ $FAILED -gt 0 || $ERRORS -gt 0 ]]; then
    echo -e "${BOLD}Failures:${NC}"
    for i in "${!RESULT_NAMES[@]}"; do
        case "${RESULT_STATUS[$i]}" in
            FAIL)
                echo -e "  ${RED}FAIL${NC}  ${RESULT_NAMES[$i]}: ${RESULT_DETAIL[$i]}"
                ;;
            STET_ERROR|GS_ERROR|ERROR)
                echo -e "  ${YELLOW}ERR${NC}   ${RESULT_NAMES[$i]}: ${RESULT_DETAIL[$i]}"
                ;;
        esac
    done
    echo ""
fi

# Diff images location
if $KEEP && [[ -d "$WORKDIR/diffs" ]]; then
    diff_count=$(find "$WORKDIR/diffs" -name '*.png' 2>/dev/null | wc -l)
    if [[ $diff_count -gt 0 ]]; then
        echo "Diff images: $WORKDIR/diffs/"
    fi
fi

if [[ $FAILED -gt 0 ]]; then
    exit 1
else
    exit 0
fi
