#!/bin/bash
# stet PDF Visual Regression Test Launcher
# Runs pdf_visual_test.py with passed arguments
#
# Usage:
#   ./pdf_visual_test.sh --baseline                  # Generate PNG baseline
#   ./pdf_visual_test.sh                             # Compare against PNG baseline
#   ./pdf_visual_test.sh --samples 509.pdf           # Test specific sample
#   ./pdf_visual_test.sh -- --dpi 300                # Pass --dpi 300 to stet

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Split arguments at "--" separator
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
    python3 "$SCRIPT_DIR/pdf_visual_test.py" "${VISUAL_ARGS[@]}" --flags "${STET_ARGS[@]}"
else
    python3 "$SCRIPT_DIR/pdf_visual_test.py" "${VISUAL_ARGS[@]}"
fi
