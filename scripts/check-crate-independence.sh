#!/usr/bin/env bash
# Assert that the PostScript side and the PDF-reading side stay independently
# usable.
#
# stet's narrow-waist architecture only pays off if a consumer can take one
# half without the other. Two promises make that real, and both are load-
# bearing for people who are already relying on them:
#
#   * A PDF-only consumer must not link the PostScript VM. `stet-pdf-reader`
#     therefore depends on `stet-fonts` + `stet-graphics` and nothing else of
#     ours — it produces the same `DisplayList` without `stet-core`.
#
#   * A PostScript-only consumer must not link the PDF parser. The `stet`
#     facade therefore carries `stet-pdf-reader` as a **dev-dependency only**.
#     `crates/stet/README.md` documents the split and the reason ("PDF-only
#     users don't pay for the VM"), and the facade deliberately exposes no
#     `PdfDocument`.
#
# That second promise is not hypothetical: the one observed external adopter
# takes the facade with `default-features = false, features = ["render"]` and
# links nine crates with no PDF reader among them. Making `stet-pdf-reader` a
# real dependency would silently add the whole PDF parser to their build.
#
# Dev-dependencies are exempt in both directions — they are not part of a
# consumer's dependency graph, only of ours when running our own tests.
#
# Both promises are checked twice: against the manifests (a direct dependency
# in the wrong table) and against the resolved dependency graph (a transitive
# one). The second check is what catches the interesting case. Until
# 2026-08-30 `stet-pdf-reader`'s default features pulled `stet-render`, which
# depended unconditionally on `stet-core` for a single trait — so the reader
# linked the whole PostScript VM by default while every manifest looked
# correct. `stet-render` now gates that trait impl behind its `ps-device`
# feature, and the reader takes the crate with `default-features = false`.
#
# Exit status: 0 = both promises hold, 1 = at least one is broken.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

python3 <<'PY'
import sys, tomllib

# Direct runtime dependencies each guarded crate is permitted to have.
# Dev-dependencies are exempt in both directions: they never reach a
# consumer's dependency graph, only ours when running our own tests.
RULES = {
    "crates/stet-pdf-reader/Cargo.toml": (
        {"stet-fonts", "stet-graphics", "stet-render"},
        "a PDF-only consumer must not link the PostScript VM directly",
    ),
    "crates/stet/Cargo.toml": (
        {"stet-fonts", "stet-graphics", "stet-core", "stet-ops",
         "stet-engine", "stet-render", "stet-pdf"},
        "a PostScript-only consumer must not link the PDF parser",
    ),
}

RUNTIME_TABLES = ("dependencies", "build-dependencies")

def runtime_deps(manifest):
    """Every stet-* dependency a consumer of this crate would link."""
    found = {}
    def scan(table, path):
        for name, spec in table.items():
            if name.startswith("stet"):
                optional = isinstance(spec, dict) and spec.get("optional", False)
                found[name] = f"{path}{' (optional)' if optional else ''}"
    for t in RUNTIME_TABLES:
        scan(manifest.get(t, {}), f"[{t}]")
    for target, cfg in manifest.get("target", {}).items():
        for t in RUNTIME_TABLES:
            scan(cfg.get(t, {}), f"[target.{target}.{t}]")
    return found

failed = False
for path, (allowed, reason) in RULES.items():
    try:
        with open(path, "rb") as f:
            manifest = tomllib.load(f)
    except FileNotFoundError:
        print(f"check-crate-independence: {path} not found", file=sys.stderr)
        failed = True
        continue
    except tomllib.TOMLDecodeError as e:
        print(f"check-crate-independence: {path} is not valid TOML: {e}", file=sys.stderr)
        failed = True
        continue

    crate = manifest.get("package", {}).get("name", path)
    for dep, where in sorted(runtime_deps(manifest).items()):
        if dep not in allowed:
            failed = True
            print(
                f"check-crate-independence: {crate} has a runtime dependency on "
                f"{dep} in {where} of {path}", file=sys.stderr)
            print(f"  This breaks the promise that {reason}.", file=sys.stderr)
            print("  Move it to [dev-dependencies] if only the tests need it.", file=sys.stderr)
            print("  See crates/stet/README.md and CLAUDE.md (crate architecture).", file=sys.stderr)

if failed:
    sys.exit(1)

# Manifest checks passed; now verify the resolved graph.
PY

# The transitive check. `cargo tree` resolves without building; it needs a
# lockfile, which the repo has. Skipped with a warning if cargo is absent so
# the script stays usable outside a toolchain.
if ! command -v cargo >/dev/null 2>&1; then
    echo "check-crate-independence: cargo not found — skipping the transitive check" >&2
    echo "check-crate-independence: direct dependencies OK (transitive check skipped)"
    exit 0
fi

# crate|forbidden-in-its-default-feature-closure|why
CLOSURES=(
    "stet-pdf-reader|stet-core stet-ops stet-engine|a PDF-only consumer must not link the PostScript VM"
    "stet|stet-pdf-reader|a PostScript-only consumer must not link the PDF parser"
)

closure_failed=0
for entry in "${CLOSURES[@]}"; do
    IFS='|' read -r crate forbidden reason <<< "$entry"
    tree="$(cargo tree -p "$crate" --edges normal 2>/dev/null || true)"
    if [ -z "$tree" ]; then
        echo "check-crate-independence: could not resolve dependencies for $crate" >&2
        closure_failed=1
        continue
    fi
    for bad in $forbidden; do
        # Match the crate name as a whole word, so stet-core does not match
        # a hypothetical stet-core-foo.
        if grep -qE "(^|[^a-z-])${bad}( |\$)" <<< "$tree"; then
            echo "check-crate-independence: ${bad} is in ${crate}'s default-feature" >&2
            echo "  dependency closure. This breaks the promise that ${reason}." >&2
            echo "  Run: cargo tree -p ${crate} --edges normal -i ${bad}" >&2
            echo "  to see which edge pulls it in." >&2
            closure_failed=1
        fi
    done
done

if [ "$closure_failed" -ne 0 ]; then
    exit 1
fi

echo "check-crate-independence: PS and PDF-reading halves are independently usable"
echo "check-crate-independence: verified against manifests and the resolved graph"
