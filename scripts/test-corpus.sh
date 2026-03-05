#!/usr/bin/env bash
# scripts/test-corpus.sh — Batch test PostScript corpus against GhostScript
#
# Usage:
#   ./scripts/test-corpus.sh [OPTIONS] [DIR]
#
# Tests all PS/EPS files in DIR (default: tests/corpus/) by rendering with
# both stet and GhostScript, then comparing output with ImageMagick RMSE.
#
# Options:
#   -d, --dpi DPI        Render resolution (default: 150)
#   -t, --threshold TH   RMSE threshold 0.0-1.0 (default: 0.05)
#   --timeout SECS       Per-file timeout (default: 30)
#   --quick              Test random 100-file subset
#   --quick N            Test random N-file subset
#   --source SOURCE      Only test files from a specific corpus source
#   --pages N            Only compare first N pages per file (default: 1)
#   --thresholds FILE    Per-file threshold overrides (default: tests/corpus/thresholds.conf)
#   --report DIR         Output directory for reports (default: tests/corpus-results/)
#   --keep               Keep rendered images after comparison
#   --parallel N         Run N files in parallel (default: 1)
#   -v, --verbose        Show detailed per-file output
#   -h, --help           Show this help

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- Configuration ---
DPI=150
THRESHOLD=0.05
TIMEOUT=30
QUICK=0
SOURCE=""
MAX_PAGES=1
THRESHOLDS_FILE=""
REPORT_DIR="$PROJECT_DIR/tests/corpus-results"
KEEP=false
PARALLEL=1
VERBOSE=false
CORPUS_DIR=""
STET="$PROJECT_DIR/target/release/stet"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

# --- Parse arguments ---
while [[ $# -gt 0 ]]; do
    case $1 in
        -d|--dpi) DPI="$2"; shift 2 ;;
        -t|--threshold) THRESHOLD="$2"; shift 2 ;;
        --timeout) TIMEOUT="$2"; shift 2 ;;
        --quick)
            if [[ "${2:-}" =~ ^[0-9]+$ ]]; then
                QUICK="$2"; shift 2
            else
                QUICK=100; shift
            fi
            ;;
        --source) SOURCE="$2"; shift 2 ;;
        --pages) MAX_PAGES="$2"; shift 2 ;;
        --thresholds) THRESHOLDS_FILE="$2"; shift 2 ;;
        --report) REPORT_DIR="$2"; shift 2 ;;
        --keep) KEEP=true; shift ;;
        --parallel) PARALLEL="$2"; shift 2 ;;
        -v|--verbose) VERBOSE=true; shift ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        -*) echo "Unknown option: $1"; exit 1 ;;
        *)
            if [[ -d "$1" ]]; then
                CORPUS_DIR="$1"
            else
                echo "Not a directory: $1"; exit 1
            fi
            shift
            ;;
    esac
done

# Defaults
[[ -z "$CORPUS_DIR" ]] && CORPUS_DIR="$PROJECT_DIR/tests/corpus"
[[ -z "$THRESHOLDS_FILE" ]] && THRESHOLDS_FILE="$CORPUS_DIR/thresholds.conf"

# --- Verify dependencies ---
for cmd in gs magick; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "Error: $cmd not found. Install GhostScript and ImageMagick."
        exit 1
    fi
done

if [[ ! -x "$STET" ]]; then
    echo "Building stet (release)..."
    cargo build --release --quiet --manifest-path "$PROJECT_DIR/Cargo.toml" 2>&1
fi

if [[ ! -d "$CORPUS_DIR" ]]; then
    echo "Corpus directory not found: $CORPUS_DIR"
    echo "Run scripts/fetch-corpus.sh first."
    exit 1
fi

# --- Load per-file thresholds ---
declare -A FILE_THRESHOLDS=()
if [[ -f "$THRESHOLDS_FILE" ]]; then
    while IFS= read -r line; do
        # Skip comments and empty lines
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ -z "${line// /}" ]] && continue
        # Format: filename threshold
        local_fname=$(echo "$line" | awk '{print $1}')
        local_thresh=$(echo "$line" | awk '{print $2}')
        if [[ -n "$local_fname" && -n "$local_thresh" ]]; then
            FILE_THRESHOLDS["$local_fname"]="$local_thresh"
        fi
    done < "$THRESHOLDS_FILE"
fi

# --- Collect files ---
mapfile -t ALL_FILES < <(
    find "$CORPUS_DIR" -type f \( -iname '*.ps' -o -iname '*.eps' -o -iname '*.epsf' \) | \
    sort
)

# Filter by source if specified
if [[ -n "$SOURCE" ]]; then
    filtered=()
    for f in "${ALL_FILES[@]}"; do
        if [[ "$f" == *"/$SOURCE/"* ]]; then
            filtered+=("$f")
        fi
    done
    ALL_FILES=("${filtered[@]}")
fi

# Quick mode: random subset
if [[ $QUICK -gt 0 && ${#ALL_FILES[@]} -gt $QUICK ]]; then
    mapfile -t ALL_FILES < <(printf '%s\n' "${ALL_FILES[@]}" | shuf | head -n "$QUICK")
fi

TOTAL=${#ALL_FILES[@]}
if [[ $TOTAL -eq 0 ]]; then
    echo "No files to test in $CORPUS_DIR"
    exit 1
fi

# --- Setup ---
mkdir -p "$REPORT_DIR"
WORKDIR=$(mktemp -d "$REPORT_DIR/run-XXXXXX")
mkdir -p "$WORKDIR/gs" "$WORKDIR/stet" "$WORKDIR/diffs"

# Result tracking
RESULTS_FILE="$WORKDIR/results.tsv"
: > "$RESULTS_FILE"

# --- Test a single file ---
test_file() {
    local file="$1"
    local basename
    basename=$(basename "$file")
    local stem="${basename%.ps}"
    stem="${stem%.PS}"
    stem="${stem%.eps}"
    stem="${stem%.EPS}"
    stem="${stem%.epsf}"
    stem="${stem%.EPSF}"

    # Determine threshold for this file
    local thresh="$THRESHOLD"
    if [[ "${FILE_THRESHOLDS[$basename]:-}" != "" ]]; then
        thresh="${FILE_THRESHOLDS[$basename]}"
    fi

    local gs_dir="$WORKDIR/gs/$stem"
    local stet_dir="$WORKDIR/stet/$stem"
    local diff_dir="$WORKDIR/diffs/$stem"
    mkdir -p "$gs_dir" "$stet_dir" "$diff_dir"

    # --- Render with GhostScript ---
    local gs_args=(-dBATCH -dNOPAUSE -dQUIET -sDEVICE=png16m "-r$DPI")
    if [[ "$file" == *.eps || "$file" == *.EPS || "$file" == *.epsf ]]; then
        gs_args+=(-dEPSCrop)
    fi
    gs_args+=("-sOutputFile=$gs_dir/page-%04d.png" "$file")

    local gs_status="ok"
    if ! timeout "$TIMEOUT" gs "${gs_args[@]}" 2>/dev/null; then
        local exit_code=$?
        if [[ $exit_code -eq 124 ]]; then
            echo -e "$basename\tGS_TIMEOUT\t-\tGhostScript timed out (${TIMEOUT}s)" >> "$RESULTS_FILE"
            return
        fi
        # Check if any pages were produced
        if ! ls "$gs_dir"/page-*.png &>/dev/null; then
            echo -e "$basename\tGS_CRASH\t-\tGhostScript crashed with no output" >> "$RESULTS_FILE"
            return
        fi
        gs_status="error"
    fi

    # --- Render with stet ---
    local filedir
    filedir=$(dirname "$file")
    local file_stem
    file_stem=$(basename "$file")
    file_stem="${file_stem%.ps}"
    file_stem="${file_stem%.PS}"
    file_stem="${file_stem%.eps}"
    file_stem="${file_stem%.EPS}"
    file_stem="${file_stem%.epsf}"
    file_stem="${file_stem%.EPSF}"

    local stet_status="ok"
    local stet_stderr
    stet_stderr=$(mktemp)
    if ! timeout "$TIMEOUT" "$STET" --device png --dpi "$DPI" "$file" >/dev/null 2>"$stet_stderr"; then
        local exit_code=$?
        if [[ $exit_code -eq 124 ]]; then
            # Collect any partial output before reporting
            for png in "$filedir/$file_stem"-[0-9][0-9][0-9][0-9].png; do
                [[ -f "$png" ]] && mv "$png" "$stet_dir/" 2>/dev/null
            done
            if ! ls "$stet_dir"/*.png &>/dev/null 2>/dev/null; then
                echo -e "$basename\tTIMEOUT\t-\tstet timed out (${TIMEOUT}s)" >> "$RESULTS_FILE"
                rm -f "$stet_stderr"
                return
            fi
            stet_status="timeout_partial"
        else
            stet_status="error"
        fi
    fi
    rm -f "$stet_stderr"

    # Move stet output PNGs
    for png in "$filedir/$file_stem"-[0-9][0-9][0-9][0-9].png; do
        if [[ -f "$png" ]]; then
            local pagenum
            pagenum=$(basename "$png" | grep -oP '\d{4}(?=\.png$)')
            mv "$png" "$stet_dir/page-$pagenum.png"
        fi
    done

    # Check stet produced output
    if ! ls "$stet_dir"/page-*.png &>/dev/null 2>/dev/null; then
        echo -e "$basename\tCRASH\t-\tstet produced no output" >> "$RESULTS_FILE"
        return
    fi

    # --- Compare pages ---
    local gs_pages=()
    for gspage in "$gs_dir"/page-[0-9][0-9][0-9][0-9].png; do
        [[ -f "$gspage" ]] || continue
        gs_pages+=("$gspage")
    done

    if [[ ${#gs_pages[@]} -eq 0 ]]; then
        echo -e "$basename\tGS_CRASH\t-\tNo GS reference pages" >> "$RESULTS_FILE"
        return
    fi

    local page_count=0
    local worst_rmse=0
    local any_fail=false
    local fail_detail=""

    for gspage in "${gs_pages[@]}"; do
        local pagenum
        pagenum=$(basename "$gspage" .png | sed 's/page-//')
        local stetpage="$stet_dir/page-$pagenum.png"

        page_count=$((page_count + 1))
        [[ $MAX_PAGES -gt 0 && $page_count -gt $MAX_PAGES ]] && break

        if [[ ! -f "$stetpage" ]]; then
            any_fail=true
            fail_detail+="p${pagenum}:MISSING "
            continue
        fi

        # Check dimensions
        local gs_dims stet_dims
        gs_dims=$(magick identify -format "%wx%h" "$gspage" 2>/dev/null)
        stet_dims=$(magick identify -format "%wx%h" "$stetpage" 2>/dev/null)

        if [[ "$gs_dims" != "$stet_dims" ]]; then
            any_fail=true
            fail_detail+="p${pagenum}:SIZE(${gs_dims}vs${stet_dims}) "
            continue
        fi

        # RMSE comparison
        local rmse_output rmse_val
        rmse_output=$(magick compare -metric RMSE "$stetpage" "$gspage" \
            "$diff_dir/page-$pagenum.png" 2>&1 || true)
        rmse_val=$(echo "$rmse_output" | grep -oP '\([\d.]+\)' | tr -d '()')

        if [[ -z "$rmse_val" ]]; then
            any_fail=true
            fail_detail+="p${pagenum}:CMP_ERR "
            continue
        fi

        # Update worst RMSE
        local is_worse
        is_worse=$(echo "$rmse_val > $worst_rmse" | bc -l 2>/dev/null || echo "0")
        [[ "$is_worse" == "1" ]] && worst_rmse="$rmse_val"

        # Check threshold
        local pass
        pass=$(echo "$rmse_val <= $thresh" | bc -l 2>/dev/null || echo "0")
        if [[ "$pass" != "1" ]]; then
            any_fail=true
            fail_detail+="p${pagenum}:RMSE=$rmse_val "
        else
            # Remove passing diff images
            $KEEP || rm -f "$diff_dir/page-$pagenum.png"
        fi
    done

    # Record result
    if $any_fail; then
        echo -e "$basename\tFAIL\t$worst_rmse\t$fail_detail" >> "$RESULTS_FILE"
    else
        echo -e "$basename\tPASS\t$worst_rmse\t${page_count}pp" >> "$RESULTS_FILE"
    fi

    # Cleanup if not keeping
    if ! $KEEP; then
        rm -rf "$gs_dir" "$stet_dir"
        find "$diff_dir" -maxdepth 0 -empty -delete 2>/dev/null || true
    fi
}

# --- Main ---
echo -e "${BOLD}PostScript Corpus Test${NC}"
echo -e "Directory: $CORPUS_DIR"
echo -e "Files: $TOTAL, DPI: $DPI, threshold: $THRESHOLD, timeout: ${TIMEOUT}s"
[[ $QUICK -gt 0 ]] && echo -e "Mode: quick (random $QUICK files)"
[[ -n "$SOURCE" ]] && echo -e "Source filter: $SOURCE"
echo ""

START_TIME=$(date +%s)

# Process files
count=0
for file in "${ALL_FILES[@]}"; do
    count=$((count + 1))
    basename=$(basename "$file")

    # Progress indicator
    if $VERBOSE; then
        echo -ne "  ${DIM}[$count/$TOTAL]${NC} ${CYAN}${basename}${NC} ... "
    else
        # Compact progress every 10 files
        if (( count % 10 == 0 )) || (( count == TOTAL )); then
            printf "\r  Progress: %d/%d" "$count" "$TOTAL"
        fi
    fi

    test_file "$file"

    if $VERBOSE; then
        # Show inline result
        local_result=$(tail -1 "$RESULTS_FILE" | cut -f2)
        case "$local_result" in
            PASS)       echo -e "${GREEN}PASS${NC}" ;;
            FAIL)       echo -e "${RED}FAIL${NC}" ;;
            CRASH)      echo -e "${RED}CRASH${NC}" ;;
            TIMEOUT)    echo -e "${YELLOW}TIMEOUT${NC}" ;;
            GS_CRASH)   echo -e "${YELLOW}GS_CRASH${NC}" ;;
            GS_TIMEOUT) echo -e "${YELLOW}GS_TIMEOUT${NC}" ;;
            *)          echo -e "${DIM}${local_result}${NC}" ;;
        esac
    fi
done

$VERBOSE || printf "\r                              \r"

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))

# --- Tally results ---
count_status() { awk -F'\t' -v s="$1" '$2==s{n++}END{print n+0}' "$RESULTS_FILE"; }
PASS_COUNT=$(count_status PASS)
FAIL_COUNT=$(count_status FAIL)
CRASH_COUNT=$(count_status CRASH)
TIMEOUT_COUNT=$(count_status TIMEOUT)
GS_CRASH_COUNT=$(count_status GS_CRASH)
GS_TIMEOUT_COUNT=$(count_status GS_TIMEOUT)

# --- Text Report ---
TEXT_REPORT="$REPORT_DIR/report.txt"
{
    echo "PostScript Corpus Test Report"
    echo "============================="
    echo ""
    echo "Date:       $(date -Iseconds)"
    echo "Corpus:     $CORPUS_DIR"
    echo "Files:      $TOTAL"
    echo "DPI:        $DPI"
    echo "Threshold:  $THRESHOLD"
    echo "Timeout:    ${TIMEOUT}s"
    echo "Duration:   ${ELAPSED}s"
    echo ""
    echo "Results"
    echo "-------"
    printf "  PASS:       %5d  (%3d%%)\n" "$PASS_COUNT" "$((PASS_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    printf "  FAIL:       %5d  (%3d%%)\n" "$FAIL_COUNT" "$((FAIL_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    printf "  CRASH:      %5d  (%3d%%)\n" "$CRASH_COUNT" "$((CRASH_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    printf "  TIMEOUT:    %5d  (%3d%%)\n" "$TIMEOUT_COUNT" "$((TIMEOUT_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    printf "  GS_CRASH:   %5d  (%3d%%)\n" "$GS_CRASH_COUNT" "$((GS_CRASH_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    printf "  GS_TIMEOUT: %5d  (%3d%%)\n" "$GS_TIMEOUT_COUNT" "$((GS_TIMEOUT_COUNT * 100 / (TOTAL > 0 ? TOTAL : 1)))"
    echo ""

    if [[ $FAIL_COUNT -gt 0 ]]; then
        echo "Failures (sorted by worst RMSE)"
        echo "-------------------------------"
        awk -F'\t' '$2=="FAIL"' "$RESULTS_FILE" | sort -t$'\t' -k3 -rn | \
        while IFS=$'\t' read -r name status rmse detail; do
            printf "  %-40s  RMSE=%-8s  %s\n" "$name" "$rmse" "$detail"
        done
        echo ""
    fi

    if [[ $CRASH_COUNT -gt 0 ]]; then
        echo "Crashes"
        echo "-------"
        awk -F'\t' '$2=="CRASH"' "$RESULTS_FILE" | while IFS=$'\t' read -r name status rmse detail; do
            printf "  %-40s  %s\n" "$name" "$detail"
        done
        echo ""
    fi

    if [[ $TIMEOUT_COUNT -gt 0 ]]; then
        echo "Timeouts"
        echo "--------"
        awk -F'\t' '$2=="TIMEOUT"' "$RESULTS_FILE" | while IFS=$'\t' read -r name status rmse detail; do
            printf "  %-40s  %s\n" "$name" "$detail"
        done
        echo ""
    fi
} > "$TEXT_REPORT"

# --- HTML Report ---
HTML_REPORT="$REPORT_DIR/report.html"
{
    cat <<'HTMLHEAD'
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>PostScript Corpus Test Report</title>
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 2em; background: #fafafa; }
  h1 { color: #333; }
  .summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 1em; margin: 1em 0 2em; }
  .stat { background: white; border-radius: 8px; padding: 1em; box-shadow: 0 1px 3px rgba(0,0,0,0.1); text-align: center; }
  .stat .count { font-size: 2em; font-weight: bold; }
  .stat .label { color: #666; font-size: 0.9em; }
  .pass .count { color: #22c55e; }
  .fail .count { color: #ef4444; }
  .crash .count { color: #dc2626; }
  .timeout .count { color: #f59e0b; }
  .gs-issue .count { color: #a855f7; }
  table { border-collapse: collapse; width: 100%; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
  th, td { padding: 0.5em 1em; text-align: left; border-bottom: 1px solid #eee; }
  th { background: #f8f8f8; font-weight: 600; position: sticky; top: 0; }
  tr:hover { background: #f0f7ff; }
  .status-PASS { color: #22c55e; font-weight: bold; }
  .status-FAIL { color: #ef4444; font-weight: bold; }
  .status-CRASH { color: #dc2626; font-weight: bold; }
  .status-TIMEOUT { color: #f59e0b; font-weight: bold; }
  .status-GS_CRASH, .status-GS_TIMEOUT { color: #a855f7; font-weight: bold; }
  .meta { color: #888; font-size: 0.85em; margin-bottom: 2em; }
  .filter-bar { margin: 1em 0; }
  .filter-bar button { padding: 0.4em 0.8em; margin: 0 0.2em; border: 1px solid #ddd; border-radius: 4px; cursor: pointer; background: white; }
  .filter-bar button.active { background: #333; color: white; border-color: #333; }
</style>
</head>
<body>
HTMLHEAD

    echo "<h1>PostScript Corpus Test Report</h1>"
    echo "<div class='meta'>Generated: $(date -Iseconds) &mdash; DPI: $DPI, threshold: $THRESHOLD, timeout: ${TIMEOUT}s, duration: ${ELAPSED}s</div>"

    echo "<div class='summary'>"
    echo "  <div class='stat pass'><div class='count'>$PASS_COUNT</div><div class='label'>PASS</div></div>"
    echo "  <div class='stat fail'><div class='count'>$FAIL_COUNT</div><div class='label'>FAIL</div></div>"
    echo "  <div class='stat crash'><div class='count'>$CRASH_COUNT</div><div class='label'>CRASH</div></div>"
    echo "  <div class='stat timeout'><div class='count'>$TIMEOUT_COUNT</div><div class='label'>TIMEOUT</div></div>"
    echo "  <div class='stat gs-issue'><div class='count'>$((GS_CRASH_COUNT + GS_TIMEOUT_COUNT))</div><div class='label'>GS Issues</div></div>"
    echo "</div>"

    # Filter buttons
    cat <<'FILTERJS'
<div class="filter-bar">
  <button class="active" onclick="filterRows('all')">All</button>
  <button onclick="filterRows('PASS')">Pass</button>
  <button onclick="filterRows('FAIL')">Fail</button>
  <button onclick="filterRows('CRASH')">Crash</button>
  <button onclick="filterRows('TIMEOUT')">Timeout</button>
  <button onclick="filterRows('GS')">GS Issues</button>
</div>
<script>
function filterRows(status) {
  document.querySelectorAll('.filter-bar button').forEach(b => b.classList.remove('active'));
  event.target.classList.add('active');
  document.querySelectorAll('tbody tr').forEach(row => {
    const s = row.dataset.status;
    if (status === 'all') row.style.display = '';
    else if (status === 'GS') row.style.display = s.startsWith('GS') ? '' : 'none';
    else row.style.display = s === status ? '' : 'none';
  });
}
</script>
FILTERJS

    echo "<table>"
    echo "<thead><tr><th>File</th><th>Status</th><th>RMSE</th><th>Detail</th></tr></thead>"
    echo "<tbody>"

    # Sort: failures first (by RMSE desc), then crashes, timeouts, passes
    sort -t$'\t' -k2,2 -k3,3rn "$RESULTS_FILE" | \
    while IFS=$'\t' read -r name status rmse detail; do
        local_class="status-${status}"
        echo "<tr data-status='$status'>"
        echo "  <td>$name</td>"
        echo "  <td class='$local_class'>$status</td>"
        echo "  <td>${rmse:--}</td>"
        echo "  <td>${detail:--}</td>"
        echo "</tr>"
    done

    echo "</tbody></table>"
    echo "</body></html>"

} > "$HTML_REPORT"

# Copy results file
cp "$RESULTS_FILE" "$REPORT_DIR/results.tsv"

# Cleanup temp workdir
if ! $KEEP; then
    rm -rf "$WORKDIR"
fi

# --- Console summary ---
echo ""
echo -e "${BOLD}═══════════════════════════════════════════════════${NC}"
echo -e "${BOLD}Corpus Test Results${NC} ($TOTAL files, ${ELAPSED}s)"
echo -e "  ${GREEN}PASS${NC}:       $PASS_COUNT"
echo -e "  ${RED}FAIL${NC}:       $FAIL_COUNT"
echo -e "  ${RED}CRASH${NC}:      $CRASH_COUNT"
echo -e "  ${YELLOW}TIMEOUT${NC}:    $TIMEOUT_COUNT"
echo -e "  ${YELLOW}GS_CRASH${NC}:   $GS_CRASH_COUNT"
echo -e "  ${YELLOW}GS_TIMEOUT${NC}: $GS_TIMEOUT_COUNT"
echo ""
echo -e "Text report:  $TEXT_REPORT"
echo -e "HTML report:  $HTML_REPORT"
echo -e "Raw results:  $REPORT_DIR/results.tsv"

if [[ $FAIL_COUNT -gt 0 || $CRASH_COUNT -gt 0 ]]; then
    echo ""
    echo -e "${BOLD}Top failures:${NC}"
    awk -F'\t' '$2=="FAIL" || $2=="CRASH"' "$REPORT_DIR/results.tsv" | sort -t$'\t' -k3 -rn | head -10 | \
    while IFS=$'\t' read -r name status rmse detail; do
        case "$status" in
            FAIL)  echo -e "  ${RED}FAIL${NC}   $name  RMSE=$rmse  $detail" ;;
            CRASH) echo -e "  ${RED}CRASH${NC}  $name  $detail" ;;
        esac
    done
fi

echo ""
if [[ $FAIL_COUNT -gt 0 || $CRASH_COUNT -gt 0 ]]; then
    exit 1
else
    exit 0
fi
