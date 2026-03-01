#!/bin/bash
# stet Visual Regression Test Launcher
# Runs visual_test.py with passed arguments
#
# Usage:
#   ./visual_test.sh --baseline                  # Generate baseline
#   ./visual_test.sh                             # Compare against baseline
#   ./visual_test.sh --samples tiger.ps          # Test specific sample
#   ./visual_test.sh -- --dpi 600                # Pass --dpi 600 to stet

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Split arguments at "--" separator: args before go to visual_test.py,
# args after get forwarded to stet via --flags.
VISUAL_ARGS=()
XFORGE_ARGS=()
FOUND_SEP=false
for arg in "$@"; do
    if [ "$arg" = "--" ] && ! $FOUND_SEP; then
        FOUND_SEP=true
        continue
    fi
    if $FOUND_SEP; then
        XFORGE_ARGS+=("$arg")
    else
        VISUAL_ARGS+=("$arg")
    fi
done

if [ ${#XFORGE_ARGS[@]} -gt 0 ]; then
    python3 "$SCRIPT_DIR/visual_test.py" "${VISUAL_ARGS[@]}" --flags "${XFORGE_ARGS[@]}"
else
    python3 "$SCRIPT_DIR/visual_test.py" "${VISUAL_ARGS[@]}"
fi
