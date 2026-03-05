#!/usr/bin/env bash
# scripts/fetch-corpus.sh — Download public PostScript test corpus
#
# Usage:
#   ./scripts/fetch-corpus.sh [OPTIONS] [SOURCE ...]
#
# Downloads PS/EPS files from public sources into tests/corpus/.
# If no sources specified, downloads all sources.
#
# Sources:
#   arxiv              arXiv PostScript papers (sample batch)
#   eps-clipart        SourceForge EPS clipart library
#   fsu                FSU academic EPS/PS examples
#   lancaster          Don Lancaster's PostScript files
#   ghostscript        GhostScript example files
#   github             GitHub PostScript collections
#   ctan               CTAN TeX/dvips PostScript samples
#   pdvectors          Public Domain Vectors EPS files
#
# Options:
#   --limit N          Max files per source (default: 0 = unlimited)
#   --clean SOURCE     Remove a source directory and re-fetch
#   --manifest         Just regenerate the manifest (no downloads)
#   --list             List available sources and exit
#   -v, --verbose      Show detailed progress
#   -h, --help         Show this help

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CORPUS_DIR="$PROJECT_DIR/tests/corpus"
MANIFEST="$CORPUS_DIR/manifest.txt"
LIMIT=0
VERBOSE=false

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

ALL_SOURCES=(arxiv eps-clipart fsu lancaster ghostscript github ctan pdvectors)

log()    { echo -e "${CYAN}[fetch]${NC} $*"; }
warn()   { echo -e "${YELLOW}[warn]${NC} $*"; }
err()    { echo -e "${RED}[error]${NC} $*" >&2; }
ok()     { echo -e "${GREEN}[done]${NC} $*"; }
vlog()   { $VERBOSE && echo -e "  $*" || true; }

# --- Helpers ---

# Count PS/EPS files in a directory (recursively)
count_ps_files() {
    find "$1" -type f \( -iname '*.ps' -o -iname '*.eps' -o -iname '*.epsf' \) 2>/dev/null | wc -l
}

# Remove non-PS/EPS files from a directory
filter_ps_only() {
    local dir="$1"
    find "$dir" -type f \
        ! -iname '*.ps' ! -iname '*.eps' ! -iname '*.epsf' \
        ! -name 'README.md' ! -name 'thresholds.conf' ! -name '.source' \
        -delete 2>/dev/null || true
    # Remove empty directories
    find "$dir" -mindepth 1 -type d -empty -delete 2>/dev/null || true
}

# Decompress any .gz PS files in place
decompress_gz() {
    local dir="$1"
    find "$dir" -type f -name '*.ps.gz' -o -name '*.eps.gz' | while read -r f; do
        gunzip -f "$f" 2>/dev/null || rm -f "$f"
    done
}

# Remove duplicate files by content hash
dedup_files() {
    local dir="$1"
    local seen_hashes
    seen_hashes=$(mktemp)
    local removed=0

    find "$dir" -type f \( -iname '*.ps' -o -iname '*.eps' -o -iname '*.epsf' \) -print0 | \
    while IFS= read -r -d '' f; do
        hash=$(sha256sum "$f" | cut -d' ' -f1)
        if grep -qF "$hash" "$seen_hashes" 2>/dev/null; then
            rm -f "$f"
            removed=$((removed + 1))
        else
            echo "$hash" >> "$seen_hashes"
        fi
    done

    rm -f "$seen_hashes"
    vlog "Removed $removed duplicate files"
}

# Apply file limit to a directory
apply_limit() {
    local dir="$1"
    if [[ $LIMIT -gt 0 ]]; then
        local count
        count=$(count_ps_files "$dir")
        if [[ $count -gt $LIMIT ]]; then
            vlog "Limiting from $count to $LIMIT files"
            find "$dir" -type f \( -iname '*.ps' -o -iname '*.eps' -o -iname '*.epsf' \) | \
                sort | tail -n +$((LIMIT + 1)) | xargs rm -f
        fi
    fi
}

# Mark a source as downloaded
mark_done() {
    local dir="$1"
    local source="$2"
    echo "$source" > "$dir/.source"
}

# Check if source already downloaded
is_done() {
    local dir="$1"
    [[ -f "$dir/.source" ]] && [[ $(count_ps_files "$dir") -gt 0 ]]
}

# --- Source: arXiv ---
# Uses the arXiv Atom API (export.arxiv.org/api/) to discover paper IDs, then
# downloads source bundles and filters for .ps/.eps files. Most submissions are
# TeX, so the hit rate is low (~5-15%) — the script tries many papers.
#
# Supports incremental fetching: tracks attempted IDs in .attempted file,
# so re-running with a higher --limit picks up where it left off.
fetch_arxiv() {
    local dir="$CORPUS_DIR/arxiv"
    local target=${LIMIT:-500}
    [[ $target -eq 0 ]] && target=500

    mkdir -p "$dir"

    local existing
    existing=$(count_ps_files "$dir")
    if [[ $existing -ge $target ]]; then
        log "arxiv: already have $existing files (target: $target)"
        return 0
    fi

    local needed=$((target - existing))
    log "arxiv: have $existing files, fetching up to $needed more..."
    log "arxiv: note — most arXiv submissions are TeX, so ~10-20 tries per PS file"

    # Track attempted IDs across runs to avoid re-downloading failures
    local attempted_file="$dir/.attempted"
    touch "$attempted_file"
    local prev_attempted
    prev_attempted=$(wc -l < "$attempted_file")

    local tmpdir
    tmpdir=$(mktemp -d)
    local downloaded=0
    local tried=0

    # Use the Atom API to discover papers, sorted by date ascending to get
    # older papers (1990s-2000s) which are more likely to be raw PS submissions.
    # We query multiple categories at various offsets for diversity.
    local categories=("hep-th" "astro-ph" "hep-ph" "cond-mat" "math-ph" "gr-qc" "nucl-th")
    local offsets=(0 500 1000 2000 3000 5000 8000 12000 20000 30000 50000)
    local batch_size=50

    for offset in "${offsets[@]}"; do
        [[ $downloaded -ge $needed ]] && break

        for cat in "${categories[@]}"; do
            [[ $downloaded -ge $needed ]] && break

            local api_url="https://export.arxiv.org/api/query?search_query=cat:${cat}&start=${offset}&max_results=${batch_size}&sortBy=submittedDate&sortOrder=ascending"
            vlog "API query: cat=$cat offset=$offset"

            local ids=()
            local xml
            xml=$(curl -s --max-time 30 "$api_url" 2>/dev/null) || continue

            # Extract paper IDs from Atom feed
            while IFS= read -r id; do
                # Strip version suffix for download (e-print endpoint handles it)
                ids+=("${id%v[0-9]*}")
            done < <(echo "$xml" | grep -oP '<id>http://arxiv.org/abs/\K[^<]+')

            [[ ${#ids[@]} -eq 0 ]] && continue

            # arXiv API rate limit: 1 request per 3 seconds
            sleep 3

            for id in "${ids[@]}"; do
                [[ $downloaded -ge $needed ]] && break

                # Skip if already attempted in a previous run
                if grep -qxF "$id" "$attempted_file" 2>/dev/null; then
                    continue
                fi

                local safe_id="${id//\//_}"
                local outfile="$tmpdir/${safe_id}.gz"

                # Record this ID as attempted before downloading
                echo "$id" >> "$attempted_file"
                tried=$((tried + 1))

                # Rate limit: 1 request per 3 seconds per arXiv policy
                sleep 3

                vlog "  [$downloaded/$needed found, $tried tried] e-print: $id"
                if ! curl -sL --max-time 60 -o "$outfile" \
                    "https://export.arxiv.org/e-print/$id" 2>/dev/null; then
                    rm -f "$outfile"
                    continue
                fi

                # Check what we got
                local ftype
                ftype=$(file -b "$outfile" 2>/dev/null || echo "unknown")

                if [[ "$ftype" == *"PostScript"* ]]; then
                    # Raw uncompressed PS (rare but possible)
                    mv "$outfile" "$dir/${safe_id}.ps"
                    downloaded=$((downloaded + 1))
                    log "  [$downloaded/$needed] $id → PS file"
                elif [[ "$ftype" == *"gzip"* ]]; then
                    # Most e-prints are gzipped — could be tar.gz or single file.gz
                    local extractdir="$tmpdir/extract_${safe_id}"
                    mkdir -p "$extractdir"

                    if tar xzf "$outfile" -C "$extractdir" 2>/dev/null; then
                        # Tar bundle — look for PS/EPS files inside
                        local tar_found=0
                        while IFS= read -r -d '' psfile; do
                            local dest="$dir/${safe_id}_$(basename "$psfile")"
                            mv "$psfile" "$dest"
                            downloaded=$((downloaded + 1))
                            tar_found=$((tar_found + 1))
                        done < <(find "$extractdir" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0)
                        [[ $tar_found -gt 0 ]] && log "  [$downloaded/$needed] $id → $tar_found PS files"
                    else
                        # Not a tar — try as gzipped single file
                        if gunzip -c "$outfile" > "$tmpdir/${safe_id}.ps" 2>/dev/null; then
                            local inner_type
                            inner_type=$(file -b "$tmpdir/${safe_id}.ps" 2>/dev/null || echo "unknown")
                            if [[ "$inner_type" == *"PostScript"* ]]; then
                                mv "$tmpdir/${safe_id}.ps" "$dir/"
                                downloaded=$((downloaded + 1))
                                log "  [$downloaded/$needed] $id → gzipped PS"
                            else
                                rm -f "$tmpdir/${safe_id}.ps"
                            fi
                        fi
                    fi
                    rm -rf "$extractdir"
                fi

                rm -f "$outfile"
            done
        done
    done

    rm -rf "$tmpdir"
    dedup_files "$dir"
    local total_attempted
    total_attempted=$(wc -l < "$attempted_file")
    mark_done "$dir" "arxiv"
    ok "arxiv: $(count_ps_files "$dir") PS files ($downloaded new from $tried tried, $total_attempted total attempted)"
}

# --- Source: EPS Clipart Library ---
fetch_eps_clipart() {
    local dir="$CORPUS_DIR/eps-clipart"
    if is_done "$dir"; then
        log "eps-clipart: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "eps-clipart: fetching from SourceForge..."

    mkdir -p "$dir"
    local tmpdir
    tmpdir=$(mktemp -d)

    # The EPS clipart collection is available as a tarball on SourceForge
    local url="https://sourceforge.net/projects/epscliparts/files/latest/download"

    vlog "Downloading archive from SourceForge..."
    if curl -sL --max-time 600 -o "$tmpdir/eps-clipart.tar.gz" "$url" 2>/dev/null; then
        vlog "Extracting archive..."
        tar xzf "$tmpdir/eps-clipart.tar.gz" -C "$tmpdir" 2>/dev/null || \
        unzip -q -o "$tmpdir/eps-clipart.tar.gz" -d "$tmpdir" 2>/dev/null || {
            # Try as zip
            mv "$tmpdir/eps-clipart.tar.gz" "$tmpdir/eps-clipart.zip"
            unzip -q -o "$tmpdir/eps-clipart.zip" -d "$tmpdir" 2>/dev/null || {
                warn "eps-clipart: failed to extract archive"
                rm -rf "$tmpdir"
                return 1
            }
        }

        # Move all EPS files to our directory
        find "$tmpdir" -type f \( -iname '*.eps' -o -iname '*.ps' \) -print0 | \
        while IFS= read -r -d '' f; do
            mv "$f" "$dir/" 2>/dev/null || true
        done
    else
        warn "eps-clipart: download failed"
        rm -rf "$tmpdir"
        return 1
    fi

    rm -rf "$tmpdir"
    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "eps-clipart"
    ok "eps-clipart: $(count_ps_files "$dir") files"
}

# --- Source: FSU Academic Examples ---
fetch_fsu() {
    local dir="$CORPUS_DIR/fsu"
    if is_done "$dir"; then
        log "fsu: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "fsu: fetching from people.sc.fsu.edu..."

    mkdir -p "$dir/eps" "$dir/ps"

    # Fetch the EPS index page and extract file links
    for section in eps ps; do
        local base_url="https://people.sc.fsu.edu/~jburkardt/data/${section}"
        local index_url="${base_url}/${section}.html"

        vlog "Fetching index: $index_url"
        local html
        html=$(curl -sL --max-time 30 "$index_url" 2>/dev/null) || continue

        # Extract .eps or .ps file links (FSU uses spaces around = in href)
        local files=()
        while IFS= read -r fname; do
            files+=("$fname")
        done < <(echo "$html" | grep -oiP "href\s*=\s*\"[^\"]+\.${section}\"" | grep -oP '"[^"]+"' | tr -d '"')

        for fname in "${files[@]}"; do
            local outpath="$dir/${section}/${fname}"
            if [[ ! -f "$outpath" ]]; then
                vlog "  Downloading $fname"
                curl -sL --max-time 30 -o "$outpath" "${base_url}/${fname}" 2>/dev/null || true
                sleep 0.5
            fi
        done
    done

    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "fsu"
    ok "fsu: $(count_ps_files "$dir") files"
}

# --- Source: Don Lancaster's PostScript ---
fetch_lancaster() {
    local dir="$CORPUS_DIR/lancaster"
    if is_done "$dir"; then
        log "lancaster: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "lancaster: fetching from tinaja.com..."

    mkdir -p "$dir"

    # Fetch the PS library index pages and extract file links
    # Don Lancaster uses .psl extension for PS library files
    local base_urls=(
        "https://www.tinaja.com/post01.shtml"
        "https://www.tinaja.com/ebooks01.shtml"
    )

    local all_files=()

    for index_url in "${base_urls[@]}"; do
        vlog "Fetching index: $index_url"
        local html
        html=$(curl -sL --max-time 30 "$index_url" 2>/dev/null) || continue

        # Extract .ps and .psl file links (relative or absolute, may have spaces around =)
        while IFS= read -r href; do
            # Resolve relative URLs
            if [[ "$href" == http* ]]; then
                all_files+=("$href")
            else
                all_files+=("https://www.tinaja.com/$href")
            fi
        done < <(echo "$html" | grep -oiP 'href\s*=\s*"[^"]*\.psl?"' | grep -oP '"[^"]+"' | tr -d '"')
    done

    for url in "${all_files[@]}"; do
        local fname
        fname=$(basename "$url")
        local outpath="$dir/$fname"

        # Normalize .psl to .ps
        if [[ "$outpath" == *.psl ]]; then
            outpath="${outpath%.psl}.ps"
        fi

        if [[ ! -f "$outpath" ]]; then
            vlog "  Downloading $fname"
            curl -sL --max-time 30 -o "$outpath" "$url" 2>/dev/null || true
            sleep 0.5
        fi
    done

    # Verify files are actually PostScript (not HTML error pages)
    find "$dir" -type f -name '*.ps' | while read -r f; do
        local ftype
        ftype=$(file -b "$f" 2>/dev/null || echo "unknown")
        if [[ "$ftype" == *"HTML"* ]] || [[ "$ftype" == *"ASCII text"* && ! "$ftype" == *"PostScript"* ]]; then
            # Check if first line starts with %! (PS magic)
            if ! head -1 "$f" | grep -q '^%!'; then
                vlog "  Removing non-PS file: $(basename "$f")"
                rm -f "$f"
            fi
        fi
    done

    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "lancaster"
    ok "lancaster: $(count_ps_files "$dir") files"
}

# --- Source: GhostScript Examples ---
fetch_ghostscript() {
    local dir="$CORPUS_DIR/ghostscript"
    if is_done "$dir"; then
        log "ghostscript: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "ghostscript: fetching from GitHub (ArtifexSoftware/ghostpdl)..."

    mkdir -p "$dir"
    local tmpdir
    tmpdir=$(mktemp -d)

    # Sparse checkout of just the examples directory
    vlog "Cloning ghostpdl examples (sparse checkout)..."
    if git clone --depth 1 --filter=blob:none --sparse \
        "https://github.com/ArtifexSoftware/ghostpdl.git" \
        "$tmpdir/ghostpdl" 2>/dev/null; then

        cd "$tmpdir/ghostpdl"
        git sparse-checkout set examples toolbin 2>/dev/null || true
        cd "$PROJECT_DIR"

        # Copy PS/EPS files
        find "$tmpdir/ghostpdl" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0 | \
        while IFS= read -r -d '' f; do
            local relpath="${f#$tmpdir/ghostpdl/}"
            local dest="$dir/$(echo "$relpath" | tr '/' '_')"
            cp "$f" "$dest"
        done
    else
        warn "ghostscript: git clone failed, trying direct download..."
        # Fallback: download specific known example files
        local files=(
            "examples/tiger.eps"
            "examples/golfer.eps"
            "examples/escher.ps"
            "examples/snowflak.ps"
            "examples/colorcir.ps"
            "examples/doretree.ps"
            "examples/alphabet.ps"
            "examples/waterfal.ps"
        )
        for f in "${files[@]}"; do
            local fname
            fname=$(basename "$f")
            curl -sL --max-time 30 -o "$dir/$fname" \
                "https://raw.githubusercontent.com/ArtifexSoftware/ghostpdl/master/$f" 2>/dev/null || true
        done
    fi

    rm -rf "$tmpdir"
    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "ghostscript"
    ok "ghostscript: $(count_ps_files "$dir") files"
}

# --- Source: GitHub Collections ---
fetch_github() {
    local dir="$CORPUS_DIR/github"
    if is_done "$dir"; then
        log "github: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "github: fetching PostScript collections..."

    mkdir -p "$dir"
    local tmpdir
    tmpdir=$(mktemp -d)

    # Repository list
    local repos=(
        "ivansostarko/postscript-examples"
        "tylus/pstest"
    )

    for repo in "${repos[@]}"; do
        local repo_name="${repo##*/}"
        vlog "Cloning $repo..."

        if git clone --depth 1 "https://github.com/$repo.git" \
            "$tmpdir/$repo_name" 2>/dev/null; then

            find "$tmpdir/$repo_name" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0 | \
            while IFS= read -r -d '' f; do
                local fname="${repo_name}_$(basename "$f")"
                cp "$f" "$dir/$fname"
            done
        else
            warn "github: failed to clone $repo"
        fi
    done

    rm -rf "$tmpdir"
    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "github"
    ok "github: $(count_ps_files "$dir") files"
}

# --- Source: CTAN TeX Samples ---
fetch_ctan() {
    local dir="$CORPUS_DIR/ctan"
    if is_done "$dir"; then
        log "ctan: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "ctan: fetching TeX/dvips PostScript samples..."

    mkdir -p "$dir"
    local tmpdir
    tmpdir=$(mktemp -d)

    # Download specific packages known to contain PS output files
    local packages=(
        "pstricks-examples"
        "testflow"
        "pst-plot"
        "pst-3dplot"
        "pst-func"
    )

    for pkg in "${packages[@]}"; do
        vlog "Fetching CTAN package: $pkg"
        local url="https://mirrors.ctan.org/graphics/pstricks/contrib/${pkg}.zip"

        if curl -sL --max-time 120 -o "$tmpdir/${pkg}.zip" "$url" 2>/dev/null; then
            mkdir -p "$tmpdir/$pkg"
            unzip -q -o "$tmpdir/${pkg}.zip" -d "$tmpdir/$pkg" 2>/dev/null || true

            find "$tmpdir/$pkg" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0 | \
            while IFS= read -r -d '' f; do
                local fname="${pkg}_$(basename "$f")"
                cp "$f" "$dir/$fname" 2>/dev/null || true
            done
        else
            # Try alternate CTAN path
            url="https://mirrors.ctan.org/macros/latex/contrib/${pkg}.zip"
            if curl -sL --max-time 120 -o "$tmpdir/${pkg}.zip" "$url" 2>/dev/null; then
                mkdir -p "$tmpdir/$pkg"
                unzip -q -o "$tmpdir/${pkg}.zip" -d "$tmpdir/$pkg" 2>/dev/null || true

                find "$tmpdir/$pkg" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0 | \
                while IFS= read -r -d '' f; do
                    local fname="${pkg}_$(basename "$f")"
                    cp "$f" "$dir/$fname" 2>/dev/null || true
                done
            else
                vlog "  Package $pkg not found"
            fi
        fi
    done

    # Also grab testflow specifically
    vlog "Fetching testflow..."
    local tf_url="https://mirrors.ctan.org/macros/latex/contrib/testflow.zip"
    if curl -sL --max-time 60 -o "$tmpdir/testflow.zip" "$tf_url" 2>/dev/null; then
        mkdir -p "$tmpdir/testflow_extract"
        unzip -q -o "$tmpdir/testflow.zip" -d "$tmpdir/testflow_extract" 2>/dev/null || true

        find "$tmpdir/testflow_extract" -type f \( -iname '*.ps' -o -iname '*.eps' \) -print0 | \
        while IFS= read -r -d '' f; do
            local fname="testflow_$(basename "$f")"
            cp "$f" "$dir/$fname" 2>/dev/null || true
        done
    fi

    rm -rf "$tmpdir"
    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "ctan"
    ok "ctan: $(count_ps_files "$dir") files"
}

# --- Source: Public Domain Vectors ---
fetch_pdvectors() {
    local dir="$CORPUS_DIR/public-domain-vectors"
    if is_done "$dir"; then
        log "pdvectors: already downloaded ($(count_ps_files "$dir") files)"
        return 0
    fi
    log "pdvectors: fetching from publicdomainvectors.org..."

    mkdir -p "$dir"
    local downloaded=0
    local target=${LIMIT:-200}
    [[ $target -eq 0 ]] && target=200

    # Scrape EPS download links from category pages
    local categories=("animals" "buildings" "flags" "food" "nature" "people" "signs" "symbols" "transport")

    for cat in "${categories[@]}"; do
        [[ $downloaded -ge $target ]] && break

        local page=1
        while [[ $downloaded -lt $target && $page -le 5 ]]; do
            local url="https://publicdomainvectors.org/en/free-clipart/${cat}/page/${page}"
            vlog "Fetching category page: $cat p$page"

            local html
            html=$(curl -sL --max-time 30 "$url" 2>/dev/null) || break

            # Extract EPS download links
            local eps_links=()
            while IFS= read -r link; do
                eps_links+=("$link")
            done < <(echo "$html" | grep -oP 'href="[^"]*\.eps"' | grep -oP '"[^"]+"' | tr -d '"' | head -20)

            [[ ${#eps_links[@]} -eq 0 ]] && break

            for link in "${eps_links[@]}"; do
                [[ $downloaded -ge $target ]] && break

                # Resolve relative URLs
                local full_url="$link"
                if [[ "$link" != http* ]]; then
                    full_url="https://publicdomainvectors.org$link"
                fi

                local fname
                fname=$(basename "$full_url")
                local outpath="$dir/$fname"

                if [[ ! -f "$outpath" ]]; then
                    if curl -sL --max-time 30 -o "$outpath" "$full_url" 2>/dev/null; then
                        downloaded=$((downloaded + 1))
                        vlog "  Downloaded: $fname"
                    fi
                    sleep 1
                fi
            done

            page=$((page + 1))
        done
    done

    # Verify downloaded files are actually EPS
    find "$dir" -type f -name '*.eps' | while read -r f; do
        if ! head -1 "$f" 2>/dev/null | grep -q '^%!'; then
            rm -f "$f"
        fi
    done

    dedup_files "$dir"
    apply_limit "$dir"
    mark_done "$dir" "pdvectors"
    ok "pdvectors: $(count_ps_files "$dir") files"
}

# --- Generate manifest ---
generate_manifest() {
    log "Generating manifest..."

    {
        echo "# PostScript Test Corpus Manifest"
        echo "# Generated: $(date -Iseconds)"
        echo ""
        echo "Source                   Files"
        echo "───────────────────────  ─────"

        local total=0
        for source in "${ALL_SOURCES[@]}"; do
            local source_dir="$CORPUS_DIR"
            case "$source" in
                pdvectors) source_dir="$CORPUS_DIR/public-domain-vectors" ;;
                *)         source_dir="$CORPUS_DIR/$source" ;;
            esac

            local count=0
            if [[ -d "$source_dir" ]]; then
                count=$(count_ps_files "$source_dir")
            fi
            total=$((total + count))
            printf "%-24s %5d\n" "$source" "$count"
        done

        echo "───────────────────────  ─────"
        printf "%-24s %5d\n" "TOTAL" "$total"
    } > "$MANIFEST"

    cat "$MANIFEST"
}

# --- Parse arguments ---
SOURCES=()
CLEAN_SOURCE=""
MANIFEST_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --limit) LIMIT="$2"; shift 2 ;;
        --clean) CLEAN_SOURCE="$2"; shift 2 ;;
        --manifest) MANIFEST_ONLY=true; shift ;;
        --list)
            echo "Available sources:"
            for s in "${ALL_SOURCES[@]}"; do
                echo "  $s"
            done
            exit 0
            ;;
        -v|--verbose) VERBOSE=true; shift ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        -*) echo "Unknown option: $1"; exit 1 ;;
        *)
            # Validate source name
            valid=false
            for s in "${ALL_SOURCES[@]}"; do
                [[ "$1" == "$s" ]] && valid=true && break
            done
            if $valid; then
                SOURCES+=("$1")
            else
                err "Unknown source: $1"
                echo "Available: ${ALL_SOURCES[*]}"
                exit 1
            fi
            shift
            ;;
    esac
done

# If no sources specified, use all
if [[ ${#SOURCES[@]} -eq 0 ]]; then
    SOURCES=("${ALL_SOURCES[@]}")
fi

# --- Main ---
mkdir -p "$CORPUS_DIR"

# Handle --clean
if [[ -n "$CLEAN_SOURCE" ]]; then
    local_dir="$CORPUS_DIR/$CLEAN_SOURCE"
    [[ "$CLEAN_SOURCE" == "pdvectors" ]] && local_dir="$CORPUS_DIR/public-domain-vectors"
    if [[ -d "$local_dir" ]]; then
        log "Cleaning $CLEAN_SOURCE..."
        rm -rf "$local_dir"
        ok "Removed $local_dir"
    fi
fi

# Handle --manifest
if $MANIFEST_ONLY; then
    generate_manifest
    exit 0
fi

echo -e "${BOLD}PostScript Test Corpus Downloader${NC}"
echo -e "Sources: ${SOURCES[*]}"
[[ $LIMIT -gt 0 ]] && echo -e "Limit: $LIMIT files per source"
echo ""

START_TIME=$(date +%s)

# Fetch each source
failed=0
for source in "${SOURCES[@]}"; do
    local_start=$(date +%s)
    case "$source" in
        arxiv)        fetch_arxiv        || { warn "arxiv fetch failed"; failed=$((failed+1)); } ;;
        eps-clipart)  fetch_eps_clipart   || { warn "eps-clipart fetch failed"; failed=$((failed+1)); } ;;
        fsu)          fetch_fsu           || { warn "fsu fetch failed"; failed=$((failed+1)); } ;;
        lancaster)    fetch_lancaster     || { warn "lancaster fetch failed"; failed=$((failed+1)); } ;;
        ghostscript)  fetch_ghostscript   || { warn "ghostscript fetch failed"; failed=$((failed+1)); } ;;
        github)       fetch_github        || { warn "github fetch failed"; failed=$((failed+1)); } ;;
        ctan)         fetch_ctan          || { warn "ctan fetch failed"; failed=$((failed+1)); } ;;
        pdvectors)    fetch_pdvectors     || { warn "pdvectors fetch failed"; failed=$((failed+1)); } ;;
    esac
    local_elapsed=$(( $(date +%s) - local_start ))
    if [[ $local_elapsed -ge 60 ]]; then
        log "$source: ${local_elapsed}s ($(( local_elapsed / 60 ))m $(( local_elapsed % 60 ))s)"
    elif [[ $local_elapsed -ge 2 ]]; then
        log "$source: ${local_elapsed}s"
    fi
done

ELAPSED=$(( $(date +%s) - START_TIME ))

echo ""
generate_manifest

echo ""
if [[ $ELAPSED -ge 60 ]]; then
    echo -e "${BOLD}Total time: $(( ELAPSED / 60 ))m $(( ELAPSED % 60 ))s${NC}"
else
    echo -e "${BOLD}Total time: ${ELAPSED}s${NC}"
fi

if [[ $failed -gt 0 ]]; then
    warn "$failed source(s) had errors"
    exit 1
fi
