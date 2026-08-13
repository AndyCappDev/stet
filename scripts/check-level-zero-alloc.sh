#!/usr/bin/env bash
# Audit VM allocation sites for the level-zero mis-stamp.
#
# `DualDictStore::allocate_at_level_zero` and its array/string siblings
# stamp an entity `save_level = 0`, `global = false`,
# `created_after_save = 0` — the entity claims to predate every
# outstanding `save`. That is only true during bootstrap, before any
# `save` can have happened.
#
# Anywhere else it is a bug with two halves:
#
#   * `restore` releases the entity anyway (reclamation is by the store's
#     high-water mark, which the entity is above), while
#     `check_invalidrestore` cannot see it — so a surviving reference to it
#     is neither rejected with `invalidrestore` nor kept alive.
#   * `global = false` ignores `currentglobal`, so an object destined for a
#     global container lands in local VM, violating PLRM 3.7.2.
#
# That is exactly the shape of the `defineresource_direct` bug fixed in
# 04ddc25. Operator code must use the VM-aware helpers in
# `stet_ops::vm_ops` instead: `alloc_dict` / `alloc_array` /
# `alloc_array_from` / `alloc_string` / `alloc_string_empty`, or the
# explicit-VM `*_in` variants when the destination container's VM is
# already fixed.
#
# Test code (`#[cfg(test)]` modules and `crates/*/tests/`) is exempt: a
# fresh `Context` in a unit test genuinely has no save outstanding.
#
# Exit status: 0 = no mis-stamped production sites, 1 = at least one.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# The one file allowed to allocate at level zero: interpreter bootstrap,
# which builds systemdict/userdict/errordict/$error/FontDirectory and
# friends before any PostScript — and therefore any `save` — has run.
ALLOWED_FILE="crates/stet-core/src/context.rs"

fail=0

python3 - "$ALLOWED_FILE" <<'PY' || fail=1
import pathlib, re, sys

allowed = sys.argv[1]
pat = re.compile(r'\b(dicts|arrays|strings)\s*\.\s*allocate(_from)?_at_level_zero\s*\(')

bad = []
for p in sorted(pathlib.Path('crates').rglob('*.rs')):
    s = str(p)
    if 'tiny-skia' in s or f'{p.parts[1]}/tests/' in s.replace('\\', '/'):
        continue
    text = p.read_text()
    # Everything from the first `#[cfg(test)]` on is test code.
    cutoff = text.find('#[cfg(test)]')
    if cutoff == -1:
        cutoff = len(text)
    for m in pat.finditer(text):
        if m.start() >= cutoff:
            continue
        if s == allowed:
            continue
        bad.append(f"{s}:{text.count(chr(10), 0, m.start()) + 1}")

if bad:
    print("Level-zero allocation outside interpreter bootstrap:", file=sys.stderr)
    for b in bad:
        print(f"  {b}", file=sys.stderr)
    print("", file=sys.stderr)
    print("Use stet_ops::vm_ops::alloc_dict / alloc_array / alloc_array_from /", file=sys.stderr)
    print("alloc_string / alloc_string_empty, or their *_in variants.", file=sys.stderr)
    sys.exit(1)

print("check-level-zero-alloc: no mis-stamped production allocation sites")
PY

exit "$fail"
