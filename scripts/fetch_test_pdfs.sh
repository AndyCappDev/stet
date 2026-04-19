#!/usr/bin/env bash
# Fetch public PDF test corpora into pdf_samples/ for contributors who want
# to reproduce visual-regression bugs or run ./pdf_visual_test.sh.
#
# The script creates a subdirectory per corpus (e.g. pdf_samples/pdfjs/) and
# never touches files already present at the top level of pdf_samples/.
# Re-running the script updates each corpus in place (git pull) and is a
# no-op for ones that are already up to date.
#
# Usage:
#   ./scripts/fetch_test_pdfs.sh              # fetch everything
#   ./scripts/fetch_test_pdfs.sh pdfjs        # fetch only the pdfjs corpus
#   ./scripts/fetch_test_pdfs.sh --list       # list available corpora

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SAMPLES_DIR="$PROJECT_ROOT/pdf_samples"

# Corpus registry: <name>|<git-url>|<sparse-path-in-repo>
# Each corpus is shallow-cloned with blob:none + sparse-checkout so we only
# pay for the bytes we need. The final PDFs live at pdf_samples/<name>/.
CORPORA=(
    "pdfjs|https://github.com/mozilla/pdf.js.git|test/pdfs"
)

list_corpora() {
    printf 'Available corpora:\n'
    for entry in "${CORPORA[@]}"; do
        IFS='|' read -r name url path <<< "$entry"
        printf '  %-12s %s (%s)\n' "$name" "$url" "$path"
    done
}

require_git() {
    if ! command -v git >/dev/null 2>&1; then
        echo "Error: 'git' is required but not on PATH" >&2
        exit 1
    fi
}

fetch_corpus() {
    local name="$1" url="$2" sparse_path="$3"
    local corpus_dir="$SAMPLES_DIR/$name"
    local src_dir="$SAMPLES_DIR/.${name}-src"

    mkdir -p "$SAMPLES_DIR"

    if [ -d "$src_dir/.git" ]; then
        echo "[$name] updating existing checkout in $src_dir"
        git -C "$src_dir" fetch --depth=1 origin HEAD
        git -C "$src_dir" reset --hard FETCH_HEAD
    else
        echo "[$name] cloning $url (sparse: $sparse_path)"
        git clone --depth=1 --filter=blob:none --sparse "$url" "$src_dir"
        git -C "$src_dir" sparse-checkout set "$sparse_path"
    fi

    # Expose PDFs at pdf_samples/<name>/. Use a symlink so the checkout
    # stays in .<name>-src/ (hidden from `glob *` and easy to clean up).
    local link_target
    link_target="$(cd "$src_dir/$sparse_path" && pwd)"
    if [ -L "$corpus_dir" ]; then
        # Refresh symlink if the target moved.
        local current
        current="$(readlink -f "$corpus_dir" 2>/dev/null || true)"
        if [ "$current" != "$link_target" ]; then
            rm -f "$corpus_dir"
            ln -s "$link_target" "$corpus_dir"
        fi
    elif [ -e "$corpus_dir" ]; then
        echo "Error: $corpus_dir exists but is not a symlink — refusing to overwrite" >&2
        exit 1
    else
        ln -s "$link_target" "$corpus_dir"
    fi

    local count
    count="$(find -L "$corpus_dir" -maxdepth 3 -name '*.pdf' -print 2>/dev/null | wc -l)"
    echo "[$name] ready at $corpus_dir ($count PDFs)"
}

main() {
    require_git

    local wanted=("$@")
    if [ ${#wanted[@]} -eq 0 ]; then
        wanted=()
        for entry in "${CORPORA[@]}"; do
            IFS='|' read -r name _ _ <<< "$entry"
            wanted+=("$name")
        done
    fi

    for name in "${wanted[@]}"; do
        local found=false
        for entry in "${CORPORA[@]}"; do
            IFS='|' read -r cname url path <<< "$entry"
            if [ "$cname" = "$name" ]; then
                fetch_corpus "$cname" "$url" "$path"
                found=true
                break
            fi
        done
        if ! $found; then
            echo "Error: unknown corpus '$name'" >&2
            list_corpora >&2
            exit 1
        fi
    done

    cat <<EOF

Next steps:
  1. Generate baselines on a known-good commit:
       ./pdf_visual_test.sh --baseline
  2. Switch to your feature branch and compare:
       ./pdf_visual_test.sh
EOF
}

case "${1:-}" in
    -h|--help)
        sed -n '2,12p' "$0"
        exit 0
        ;;
    --list)
        list_corpora
        exit 0
        ;;
    *)
        main "$@"
        ;;
esac
