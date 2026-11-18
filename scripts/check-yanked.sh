#!/usr/bin/env bash
# Fail if a committed Cargo.lock pins a crate version that crates.io has
# yanked.
#
# Why this exists: CI and release.yml build with `--locked`, and cargo uses a
# yanked version from a lockfile without a word. The only place cargo itself
# checks is `cargo publish` of a crate that ships its lockfile -- stet-viewer
# and stet-cli, the last two in the publish order. 0.11.0 found out that way
# (2026-11-18): async-executor 1.14.0, yanked for a use-after-free in
# `Executor::drop`, and clipboard-win 5.4.1 had been in the lock since 0.8.0,
# and the warning arrived with nine crates already published and the
# prebuilt binaries already built from that lock.
#
# Where it runs, and deliberately where it does not:
#   * release.yml -- the draft release needs this job, so a yank stops the
#     release while only the tag exists, before anything irreversible.
#   * deps.yml    -- weekly, as information between releases.
#   * by hand before tagging (CLAUDE.md release checklist).
#   * NOT ci.yml. A yank is a change upstream, not in this repository, and a
#     gate on it would turn main red for something no commit did -- the
#     breakage the committed lock exists to prevent.
#
# It reads the crates.io sparse index (https://index.crates.io), the same
# data cargo uses, so it needs the network. A lookup that fails is an error,
# not a pass: a check that cannot see the index has checked nothing.
#
# Usage: scripts/check-yanked.sh [Cargo.lock ...]
#   With no arguments: the workspace lock and crates/stet-wasm/Cargo.lock.
#   fuzz/Cargo.lock is not committed and deliberately floats, so it is not
#   checked.

set -euo pipefail

cd "$(dirname "$0")/.."

if [ "$#" -eq 0 ]; then
    set -- Cargo.lock crates/stet-wasm/Cargo.lock
fi

python3 -I - "$@" <<'PY'
import concurrent.futures as cf
import json
import sys
import tomllib
import urllib.error
import urllib.request

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
INDEX = "https://index.crates.io"


def index_path(name):
    """The sparse-index path of a crate, as cargo computes it."""
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def yanked_versions(name):
    """The set of yanked versions of `name` on crates.io."""
    req = urllib.request.Request(
        f"{INDEX}/{index_path(name)}",
        headers={"User-Agent": "stet-check-yanked"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = resp.read().decode()
    return {
        entry["vers"]
        for entry in map(json.loads, filter(None, body.splitlines()))
        if entry.get("yanked")
    }


# Every crates.io package each lock pins, by lock.
pins = {}
for lock in sys.argv[1:]:
    with open(lock, "rb") as f:
        packages = tomllib.load(f).get("package", [])
    pins[lock] = [
        (p["name"], p["version"]) for p in packages if p.get("source") == REGISTRY
    ]

names = sorted({name for pinned in pins.values() for name, _ in pinned})
yanked, failed = {}, []
with cf.ThreadPoolExecutor(16) as pool:
    futures = {pool.submit(yanked_versions, n): n for n in names}
    for fut in cf.as_completed(futures):
        name = futures[fut]
        try:
            yanked[name] = fut.result()
        except (urllib.error.URLError, OSError, ValueError) as e:
            failed.append(f"{name}: {e}")

if failed:
    print("check-yanked: could not read the crates.io index for", file=sys.stderr)
    for line in sorted(failed):
        print(f"  {line}", file=sys.stderr)
    sys.exit(2)

found = 0
for lock, pinned in pins.items():
    bad = sorted((n, v) for n, v in pinned if v in yanked[n])
    if not bad:
        continue
    found += len(bad)
    folder = lock.rsplit("/", 1)[0] if "/" in lock else "."
    print(f"check-yanked: {lock} pins {len(bad)} yanked version(s):", file=sys.stderr)
    for n, v in bad:
        print(f"  {n} {v}", file=sys.stderr)
    update = " ".join(f"-p {n}@{v}" for n, v in bad)
    where = "" if folder == "." else f"cd {folder} && "
    print(f"  fix: {where}cargo update {update}", file=sys.stderr)

if found:
    sys.exit(1)

total = sum(len(p) for p in pins.values())
print(f"check-yanked: no yanked versions among {total} pins in {len(pins)} lockfile(s)")
PY
