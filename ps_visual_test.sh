#!/bin/bash
# stet PostScript Visual Regression Test Launcher
# Runs visual_test.py with passed arguments
#
# Usage:
#   ./ps_visual_test.sh --baseline                  # Generate PNG baseline
#   ./ps_visual_test.sh                             # Compare against PNG baseline
#   ./ps_visual_test.sh --samples tiger.ps          # Test specific sample
#   ./ps_visual_test.sh -- --dpi 600                # Pass --dpi 600 to stet
#   ./ps_visual_test.sh --device pdf --baseline     # Generate PDF baseline (flat dir)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Check for --device pdf mode: render all samples to PDF in a flat directory
DEVICE=""
REMAINING_ARGS=()
for arg in "$@"; do
    if [ "$arg" = "--device" ]; then
        DEVICE="__next__"
        continue
    fi
    if [ "$DEVICE" = "__next__" ]; then
        DEVICE="$arg"
        continue
    fi
    REMAINING_ARGS+=("$arg")
done

if [ "$DEVICE" = "pdf" ]; then
    # PDF mode: render samples to flat visual_tests_pdf directory
    STET="$SCRIPT_DIR/target/release/stet"
    if [ ! -f "$STET" ]; then
        STET="$SCRIPT_DIR/target/debug/stet"
    fi
    if [ ! -f "$STET" ]; then
        echo "Error: stet binary not found. Run 'cargo build --release' first."
        exit 1
    fi

    OUTDIR="$SCRIPT_DIR/visual_tests_pdf"
    mkdir -p "$OUTDIR"

    # Parse --samples from remaining args
    SAMPLES=()
    SKIP_NEXT=false
    for arg in "${REMAINING_ARGS[@]}"; do
        if $SKIP_NEXT; then
            SKIP_NEXT=false
            continue
        fi
        case "$arg" in
            --baseline) ;;  # ignored, PDF mode is always "baseline"
            --samples) SKIP_NEXT=true ;;  # TODO: handle specific samples
            *) ;;
        esac
    done

    # Collect sample files (use globbing to handle spaces in filenames)
    SAMPLE_FILES=()
    while IFS= read -r -d '' f; do
        SAMPLE_FILES+=("$f")
    done < <(find "$SCRIPT_DIR/ps_samples" -maxdepth 1 \( -name '*.ps' -o -name '*.eps' \) -print0 | sort -z)
    echo "Rendering ${#SAMPLE_FILES[@]} samples to PDF in $OUTDIR..."

    OKAY=0
    FAIL=0
    START=$(date +%s)
    for f in "${SAMPLE_FILES[@]}"; do
        name=$(basename "$f" .ps)
        name=$(basename "$name" .eps)
        printf "  Rendering %s..." "$(basename "$f")"
        cp "$f" "$OUTDIR/"
        if (cd "$OUTDIR" && "$STET" --device pdf "$(basename "$f")" 2>/dev/null); then
            echo " OK"
            OKAY=$((OKAY + 1))
        else
            echo " FAIL"
            FAIL=$((FAIL + 1))
        fi
        rm -f "$OUTDIR/$(basename "$f")"
    done
    END=$(date +%s)
    echo ""
    echo "Done: $OKAY succeeded, $FAIL failed ($(( END - START ))s)"
    exit 0
fi

# Standard PNG mode: split arguments at "--" separator
VISUAL_ARGS=()
STET_ARGS=()
FOUND_SEP=false
for arg in "$@"; do
    if [ "$arg" = "--" ] && ! $FOUND_SEP; then
        FOUND_SEP=true
        continue
    fi
    if $FOUND_SEP; then
        STET_ARGS+=("$arg")
    else
        VISUAL_ARGS+=("$arg")
    fi
done

if [ ${#STET_ARGS[@]} -gt 0 ]; then
    python3 "$SCRIPT_DIR/visual_test.py" "${VISUAL_ARGS[@]}" --flags "${STET_ARGS[@]}"
else
    python3 "$SCRIPT_DIR/visual_test.py" "${VISUAL_ARGS[@]}"
fi
