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
# KNOWN GAP, deliberately not gated here (2026-08-30). The first promise holds
# for *direct* dependencies only. `stet-pdf-reader`'s default features include
# `render`, which pulls `stet-render`, which depends on `stet-core` — so
# `stet-pdf-reader = "0.6"` with default features does link the VM
# transitively, and there is currently no configuration giving PDF -> RGBA
# without it. `stet-render` needs `stet-core` for a single item,
# `stet_core::device::OutputDevice`. Moving that trait to `stet-graphics`
# (where the param structs it uses already live) and re-exporting it from
# `stet-core` would close the gap without breaking downstream implementors,
# at which point this script should be tightened from direct dependencies to
# the full runtime closure. Until then `crates/stet/README.md`'s "PDF-only
# users don't pay for the VM" is true only under `--no-default-features`.
#
# This is checked mechanically rather than left to review because the failure
# is invisible in isolation: adding one line to a `[dependencies]` table
# compiles fine, passes every test, and quietly breaks the promise. It is also
# easy to *misread* by hand — `[features]` precedes `[dependencies]` in
# `crates/stet/Cargo.toml`, so a naive `sed '/^\[dependencies\]/,/^\[features\]/'`
# range never terminates and reports the dev-dependency as a real one. The
# manifests are parsed as TOML here for exactly that reason.
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

print("check-crate-independence: PS and PDF-reading halves are independently usable")
PY
