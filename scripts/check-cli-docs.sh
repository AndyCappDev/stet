#!/usr/bin/env bash
# Verify every command-line option the CLI accepts is documented.
#
# The flag set is derivable from the source, so nobody should have to remember
# to update prose after adding one. This exists because they didn't: `--page`
# and `--password` shipped in 0.5.0 documented only in `--help`, and
# `--max-vm` and `--timeout` were added later the same way. The crates.io page
# for `stet-cli` renders `crates/stet-cli/README.md`, so a flag missing there
# is a flag users cannot discover without installing the binary first.
#
# Three surfaces have to agree, and they drift in both directions — the
# `--threads` default was correct in both READMEs and stale in `--help`:
#
#   1. the argument parser in `crates/stet-cli/src/main.rs`  (the truth)
#   2. `print_help()` in the same file                        (what `--help` prints)
#   3. `README.md` and `crates/stet-cli/README.md`            (what readers see)
#
# Everything is read from source text, so this needs no build and runs in the
# lint job. It checks that each flag is *present*, not that its description is
# accurate — prose still needs a human.
#
# Usage: scripts/check-cli-docs.sh
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

main="crates/stet-cli/src/main.rs"
cli_readme="crates/stet-cli/README.md"
root_readme="README.md"

for f in "$main" "$cli_readme" "$root_readme"; do
  if [ ! -f "$f" ]; then
    echo "check-cli-docs: missing $f" >&2
    exit 1
  fi
done

# The line range of print_help(), so help text and parser arms can be told
# apart — both live in the same file and both contain flag spellings.
help_range="$(awk '/^fn print_help\(\)/{s=NR} s&&/^}/{print s","NR; exit}' "$main")"
if [ -z "$help_range" ]; then
  echo "check-cli-docs: could not locate print_help() in $main" >&2
  echo "  If it was renamed, update this script — do not delete the check." >&2
  exit 1
fi
help_start="${help_range%,*}"
help_end="${help_range#*,}"

# Flags the parser actually accepts: a match arm at the start of a line, which
# excludes the spellings inside the help string and in comments. Long forms
# only — the short aliases (-h, -V, -o) are not separately documented.
#
# An arm may lead with a short alias (`"-x" | "--example" => {`), so match the
# whole arm and then pull the long forms out of it. Anchoring on `"--` instead
# skips such an arm entirely and reports its flag as documented when it is in
# neither README — silently inverting the one thing this script is for.
mapfile -t flags < <(
  sed -n "1,${help_start}p;${help_end},\$p" "$main" |
    grep -E '^[[:space:]]+"-[^"]*"([[:space:]]*\|[[:space:]]*"[^"]*")*[[:space:]]*=>' |
    grep -oE '"--[a-z0-9-]+"' |
    tr -d '"' |
    sort -u
)

if [ "${#flags[@]}" -eq 0 ]; then
  echo "check-cli-docs: found no flags in $main — the parser shape changed" >&2
  echo "  Update this script rather than removing it." >&2
  exit 1
fi

help_text="$(sed -n "${help_start},${help_end}p" "$main")"
fail=0

report() {
  printf '  %-22s missing from %s\n' "$1" "$2"
  fail=1
}

# Match a flag as a whole word. A plain substring search reports `--page` as
# documented whenever `--pages` appears, which is exactly the pair this
# codebase has.
has_flag() {
  grep -qE -- "$1([^a-zA-Z0-9-]|\$)" "$2"
}
has_flag_in_text() {
  grep -qE -- "$1([^a-zA-Z0-9-]|\$)" <<<"$2"
}

for flag in "${flags[@]}"; do
  # `--help` and `--version` are conventional and self-describing; they are
  # listed in the usage block but need no entry in a README options table.
  case "$flag" in
    --help | --version)
      has_flag_in_text "$flag" "$help_text" || report "$flag" "--help output"
      continue
      ;;
  esac
  has_flag_in_text "$flag" "$help_text" || report "$flag" "--help output"
  has_flag "$flag" "$cli_readme" || report "$flag" "$cli_readme"
  has_flag "$flag" "$root_readme" || report "$flag" "$root_readme"
done

# Subcommands are discoverable the same way and drift the same way.
for sub in inspect; do
  grep -qE "\"$sub\"" "$main" || continue
  grep -qF -- "$sub" "$cli_readme" || report "subcommand '$sub'" "$cli_readme"
  grep -qF -- "$sub" "$root_readme" || report "subcommand '$sub'" "$root_readme"
done

if [ "$fail" -ne 0 ]; then
  cat >&2 <<'MSG'

check-cli-docs: CLI documentation is out of date.

Add the flags above to whichever surface is missing them:
  - crates/stet-cli/README.md   <- this is what crates.io renders
  - README.md                   <- the "### Options" table
  - print_help() in crates/stet-cli/src/main.rs

A flag documented only in `--help` cannot be found by anyone deciding
whether to install stet in the first place.
MSG
  exit 1
fi

echo "check-cli-docs: ${#flags[@]} CLI options documented in --help and both READMEs"
