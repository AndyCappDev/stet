#!/usr/bin/env bash
# PGO (Profile-Guided Optimization) build script for stet
#
# Usage:
#   ./scripts/pgo-build.sh          # full PGO build with default workloads
#   ./scripts/pgo-build.sh --quick  # use only the fast workloads
#
# The resulting binary is at: target/release/stet
#
# Prerequisites: rustup component add llvm-tools-preview

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SAMPLES_DIR="$PROJECT_DIR/samples"
PROFILE_DIR="$PROJECT_DIR/target/pgo-profiles"

# --- Workload definitions ---

# Core compute-heavy workloads (always included)
CORE_WORKLOADS=(
    "mand.ps"
    "mandelbrot.ps"
    "tiny raytracer.ps"
)

# Additional workloads that exercise fonts, images, filters, clipping, etc.
EXTRA_WORKLOADS=(
    "tiger.ps"
    "eazybbs.ps"
    "hospital.eps"
    "javaplatform.ps"
    "cf-route.ps"
    "whitepaper.ps"
    "escher.ps"
    "doretree.ps"
)

# Parse args
QUICK=false
for arg in "$@"; do
    case "$arg" in
        --quick) QUICK=true ;;
        --help|-h)
            echo "Usage: $0 [--quick]"
            echo "  --quick  Use only core compute workloads (faster build)"
            exit 0
            ;;
        *) echo "Unknown option: $arg"; exit 1 ;;
    esac
done

if [ "$QUICK" = true ]; then
    WORKLOADS=("${CORE_WORKLOADS[@]}")
else
    WORKLOADS=("${CORE_WORKLOADS[@]}" "${EXTRA_WORKLOADS[@]}")
fi

# Verify workloads exist
MISSING=()
for f in "${WORKLOADS[@]}"; do
    if [ ! -f "$SAMPLES_DIR/$f" ]; then
        MISSING+=("$f")
    fi
done
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "ERROR: Missing workload files in $SAMPLES_DIR:"
    printf "  %s\n" "${MISSING[@]}"
    exit 1
fi

# Check for llvm-profdata
SYSROOT=$(rustc --print sysroot)
LLVM_PROFDATA=$(find "$SYSROOT" -name "llvm-profdata" -type f 2>/dev/null | head -1 || true)
if [ -z "$LLVM_PROFDATA" ]; then
    echo "ERROR: llvm-profdata not found. Install with:"
    echo "  rustup component add llvm-tools-preview"
    exit 1
fi
echo "Using llvm-profdata: $LLVM_PROFDATA"

cd "$PROJECT_DIR"

# --- Step 1: Build instrumented binary ---
echo ""
echo "=== Step 1/4: Building instrumented binary ==="
rm -rf "$PROFILE_DIR"
mkdir -p "$PROFILE_DIR"

RUSTFLAGS="-Cprofile-generate=$PROFILE_DIR" \
    cargo build --release --bin stet 2>&1

INSTRUMENTED_BIN="$PROJECT_DIR/target/release/stet"
if [ ! -x "$INSTRUMENTED_BIN" ]; then
    echo "ERROR: Instrumented binary not found at $INSTRUMENTED_BIN"
    exit 1
fi

# --- Step 2: Run workloads to generate profile data ---
echo ""
echo "=== Step 2/4: Running workloads (${#WORKLOADS[@]} files) ==="

for f in "${WORKLOADS[@]}"; do
    echo -n "  $f ... "
    START=$(date +%s%N)
    "$INSTRUMENTED_BIN" --dpi 72 "$SAMPLES_DIR/$f" > /dev/null 2>&1 || true
    END=$(date +%s%N)
    ELAPSED=$(( (END - START) / 1000000 ))
    echo "${ELAPSED}ms"
done

# Count raw profile files
RAW_COUNT=$(find "$PROFILE_DIR" -name "*.profraw" | wc -l)
echo "  Generated $RAW_COUNT raw profile files"

if [ "$RAW_COUNT" -eq 0 ]; then
    echo "ERROR: No profile data generated. Something went wrong."
    exit 1
fi

# --- Step 3: Merge profile data ---
echo ""
echo "=== Step 3/4: Merging profile data ==="

find "$PROFILE_DIR" -name "*.profraw" -print0 | xargs -0 "$LLVM_PROFDATA" merge -o "$PROFILE_DIR/merged.profdata"
echo "  Merged into $PROFILE_DIR/merged.profdata"

# --- Step 4: Build optimized binary ---
echo ""
echo "=== Step 4/4: Building PGO-optimized binary ==="

RUSTFLAGS="-Cprofile-use=$PROFILE_DIR/merged.profdata -Cllvm-args=-pgo-warn-missing-function" \
    cargo build --release --bin stet 2>&1

echo ""
echo "=== PGO build complete ==="
echo "Binary: $PROJECT_DIR/target/release/stet"
echo ""

# --- Quick benchmark ---
echo "=== Quick benchmark (mandelbrot.ps at 72 DPI) ==="
for i in 1 2 3; do
    echo -n "  Run $i: "
    START=$(date +%s%N)
    "$PROJECT_DIR/target/release/stet" --dpi 72 "$SAMPLES_DIR/mandelbrot.ps" > /dev/null 2>&1
    END=$(date +%s%N)
    ELAPSED_MS=$(( (END - START) / 1000000 ))
    echo "${ELAPSED_MS}ms"
done
