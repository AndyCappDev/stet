# xforge Phase 6: Resource System & Init Scripts — Implementation Plan

## Context

Phase 5 is complete: ~236 operators, 352 tests, zero clippy warnings. The interpreter renders PostScript graphics with paths, transforms, colors, text, images, and filters to PNG.

Phase 6 adds the PostScript resource system, runs init scripts at startup, and makes the interpreter self-hosting. PostForge's init scripts (~1,500 lines of PostScript) bootstrap the resource system, define resource categories, register font substitution mappings, and set up error handling. The core resource operators (`findresource`, `defineresource`, etc.) are Rust-native but dispatch to PostScript procedures in category implementation dictionaries.

**Done when**: Adapted init scripts execute successfully at startup. `findresource` loads resources from disk. Encodings loaded via resource system. Font loading works through the resource system. All existing tests and sample files continue to work.

---

## Architecture: Key Decisions

### 1. Resource Storage — Dict-of-Dicts on Context

Resources live in two dict hierarchies (mirroring PostForge):
- **Global resources**: `ctx.global_resources` — an `EntityId` pointing to a dict where keys are category names (Font, Encoding, etc.) and values are dicts mapping resource keys to instances. Persists across save/restore.
- **Local resources**: `ctx.local_resources` — same structure but in local VM, subject to save/restore.
- **Category registry**: `ctx.category_registry` — an `EntityId` pointing to the Category dict mapping category names to their implementation dicts.

Each category implementation dict contains `/FindResource`, `/DefineResource`, `/UndefineResource`, `/ResourceStatus`, `/ResourceForAll` procedures, plus `/ResourceDir`, `/ResourceExtension`, `/InstanceType`, `/Category`.

### 2. Resource Operator Dispatch Pattern

The five core resource operators follow a common pattern:
1. Pop category name from stack
2. Look up category in Category registry
3. Get the corresponding procedure (FindResource, DefineResource, etc.)
4. If it's a PS procedure: push impl dict on d_stack, push procedure on e_stack, execute
5. If it's the operator itself (recursive reference): do simple VM-based lookup directly

### 3. Init Script Strategy — Adapt from PostForge

Copy PostForge's init scripts (`~/Projects/postforge/postforge/resources/Init/`) with targeted adaptations:
- **sysdict.ps**: Adapt product/version, keep resource bootstrap + error handling + page sizes
- **resourcecategories.ps**: Nearly verbatim — creates all standard + implicit categories
- **fontcategory.ps**: Moderate adaptation — simplify FindResource since xforge has Rust-native `.t1` parsing
- **fontmapping.ps**: Copy verbatim — pure data

PostForge's `join` operator (non-standard, used for path construction in init scripts) will be implemented in xforge to maximize init script compatibility.

### 4. Font System Integration

Phase 4's Rust-native font loader remains the backend. The init scripts define `findfont` as `{ /Font findresource }` which dispatches to Font category's FindResource. That procedure searches resource dicts, then `resources/Font/` directory, using `run` to execute `.t1` files (triggering xforge's Rust parser). The Rust `op_findfont` etc. are still registered but get shadowed by PS definitions from init scripts.

### 5. Operator Shadowing Strategy

Register ALL Rust operators including findfont/definefont/page sizes. Init scripts `def` PS versions that shadow them in systemdict. If init scripts run → PS versions take precedence. If they don't → Rust versions still function. This preserves backward compatibility.

---

## Pre-Step: Save Plan to docs/

**Create**: `docs/PHASE6-PLAN.md` — copy this plan to the docs directory alongside existing phase plans (PHASE1-PLAN.md through PHASE4-PLAN.md).

---

## Implementation Steps (10 steps, always compiling)

### Step 1: Missing Utility Operators
**Modify**: `crates/xforge-ops/src/control_ops.rs`, `crates/xforge-ops/src/misc_ops.rs`
**Modify**: `crates/xforge-ops/src/lib.rs`

Operators sysdict.ps needs that xforge doesn't have yet:

| Operator | Stack | Notes |
|----------|-------|-------|
| `countexecstack` | — → int | Return exec stack depth |
| `execstack` | array → subarray | Copy exec stack into array |
| `join` | arr/str1 str2 → str3 | Concatenate; array of strings → joined string |

Internal/stub operators:

| Operator | Stack | Notes |
|----------|-------|-------|
| `.nextfid` | — → int | Return next font ID, increment counter |
| `.loadsystemfont` | name → path true \| false | Return false (no system font cache) |
| `.loadbinarysystemfont` | name → bool | Return false |
| `.loadbinaryfontfile` | name path → bool | Return false |
| `.systemundef` | dict key → | `undef` ignoring access restrictions |
| `.setinteractivepaint` | bool → | No-op |
| `pauseexechistory` | — → | No-op |
| `resumeexechistory` | — → | No-op |
| `exechistorystack` | array → subarray | Return 0-length subarray |
| `exitserver` | password → | No-op |
| `startjob` | password bool → bool | Pop args, push true |

~16 new operators, ~8 tests.

### Step 2: Resource Storage Infrastructure
**Modify**: `crates/xforge-core/src/context.rs`

Add to `Context`:
```rust
pub global_resources: EntityId,   // dict: category_name → resource_dict
pub local_resources: EntityId,    // dict: category_name → resource_dict
pub category_registry: EntityId,  // dict: category_name → impl_dict
pub resource_base_path: Option<String>,  // path to resources/ directory
```

Add `NameCache` entries: `n_find_resource`, `n_define_resource`, `n_undef_resource`, `n_resource_status`, `n_resource_for_all`, `n_category`, `n_instance_type`, `n_resource_dir`, `n_resource_ext`.

In `Context::new()`: allocate the three resource dicts, mark global_resources as global VM.

~2 tests.

### Step 3: Parameter Operators
**New file**: `crates/xforge-ops/src/param_ops.rs`
**Modify**: `crates/xforge-ops/src/lib.rs`

| Operator | Stack | Notes |
|----------|-------|-------|
| `setuserparams` | dict → | Store user params |
| `currentuserparams` | — → dict | Return current user params |
| `setsystemparams` | dict → | Stub: pop, no-op |
| `currentsystemparams` | — → dict | Return system params |
| `setdevparams` | dict → | Stub: pop, no-op |
| `currentdevparams` | — → dict | Stub: return empty dict |

Add `user_params: EntityId` and `system_params: EntityId` to Context. Initialize with defaults: MaxDictStack=500, MaxExecStack=5000, MaxOpStack=300000, ExecutionHistory=false, ExecutionHistorySize=20.

~4 tests.

### Step 4: Core Resource Operators
**New file**: `crates/xforge-ops/src/resource_ops.rs`
**Modify**: `crates/xforge-ops/src/lib.rs`

**`findresource`**: key category → instance
- Special case: category `/Category` → look up directly in category_registry
- Otherwise: get impl dict from category_registry, get `/FindResource`, dispatch

**`defineresource`**: key instance category → instance
- Get impl dict, dispatch DefineResource procedure
- Direct mode: validate InstanceType, store in global/local resource dict based on `vm_alloc_mode`

**`undefineresource`**: key category →
- Get impl dict, dispatch UndefineResource procedure

~6 tests.

### Step 5: Resource Query + Helper Operators
**Modify**: `crates/xforge-ops/src/resource_ops.rs`

| Operator | Stack | Notes |
|----------|-------|-------|
| `resourcestatus` | key category → status size true \| false | Dispatch ResourceStatus |
| `resourceforall` | template proc scratch category → | Dispatch ResourceForAll |
| `globalresourcedict` | category → dict true \| false | Look up in global_resources |
| `localresourcedict` | category → dict true \| false | Look up in local_resources |
| `categoryimpdict` | — → dict | Return category_registry |

~6 tests.

### Step 6: Copy and Adapt Init Scripts
**New dirs**: `resources/Init/`, `resources/Encoding/`

Copy from `~/Projects/postforge/postforge/resources/`:
- `Init/sysdict.ps` → adapt (product name, remove PostForge-specifics)
- `Init/resourcecategories.ps` → minimal adaptation (resource paths)
- `Init/fontcategory.ps` → moderate adaptation
- `Init/fontmapping.ps` → copy verbatim
- `Encoding/StandardEncoding.ps` → copy
- `Encoding/ISOLatin1Encoding.ps` → copy
- `Encoding/SymbolEncoding.ps` → copy

Key sysdict.ps adaptations:
- Product: `(AGPL xforge)`, version to match xforge
- Resource paths: ensure they work relative to xforge's `resources/` dir
- Remove executive/REPL section (xforge has its own simpler REPL)
- Keep: resource bootstrap, error handling, page sizes, printing operators

### Step 7: CLI Startup — Init Script Execution
**Modify**: `crates/xforge-cli/src/main.rs`

Startup sequence:
1. `Context::new()` + `build_system_dict()` (as before)
2. Discover `resources/` directory (expand existing `find_font_resource_path` → `find_resource_path`)
3. Set `ctx.resource_base_path`
4. Execute: `{(resources/Init/sysdict.ps) run} stopped { (Init failed\n) print } if`
5. If init succeeds: resource system ready, PS ops shadow Rust ops
6. If init fails: warn, continue with Rust-only mode

### Step 8: Resource-Based Font Loading
**Verify**: existing font loading still works through the resource system chain:
- `findfont` (PS) → `/Font findresource` → Font category FindResource → searches dicts → `run` on `.t1` file → Rust parser → font dict registered

May need minor adjustments to fontcategory.ps to align with xforge's font file organization.

### Step 9: Encoding Resources
Verify encoding loading: `sysdict.ps` line 683-685 does:
```postscript
/StandardEncoding /StandardEncoding /Encoding findresource def
```
This triggers the Encoding category's FindResource which loads `resources/Encoding/StandardEncoding.ps`, producing a 256-element array.

### Step 10: Integration Tests + Verification
**Modify**: `crates/xforge-engine/tests/rendering.rs`
**Modify**: `docs/ROADMAP.md`

Tests:
1. Init scripts run without error
2. `findresource` loads Encoding from disk
3. `findfont` works through PS resource wrapper
4. `defineresource` round-trip
5. All rendering tests pass (regressions)

---

## New Files Summary

**New Rust files (2):**
- `crates/xforge-ops/src/resource_ops.rs` — 8 resource operators
- `crates/xforge-ops/src/param_ops.rs` — 6 parameter operators

**New resource files (7):**
- `resources/Init/sysdict.ps`, `resourcecategories.ps`, `fontcategory.ps`, `fontmapping.ps`
- `resources/Encoding/StandardEncoding.ps`, `ISOLatin1Encoding.ps`, `SymbolEncoding.ps`

**Modified Rust files (5):**
- `crates/xforge-core/src/context.rs` — resource storage, NameCache additions
- `crates/xforge-ops/src/lib.rs` — register ~28 new operators
- `crates/xforge-ops/src/control_ops.rs` — countexecstack, execstack
- `crates/xforge-ops/src/misc_ops.rs` — join, stubs
- `crates/xforge-cli/src/main.rs` — init script loading, resource path discovery

---

## Operator Count

Phase 5: ~236 → Phase 6: ~264 (+28 Rust operators, plus ~80 PS-defined from init scripts)

## Test Target

~30 new tests → ~382 total

---

## Verification

1. `cargo build` — compiles cleanly
2. `cargo test` — all tests pass
3. `cargo clippy` — zero warnings
4. `cargo run -- ~/Projects/postforge/samples/test1.ps` — renders text correctly
5. `cargo run -- ~/Projects/postforge/samples/turkey-imagemask.ps` — renders turkey
6. `cargo run -- /tmp/tiger.ps` — renders correctly
7. `cargo run -- ~/Projects/postforge/samples/golfer.ps` — renders correctly
8. Init scripts run at startup without errors (no `Init failed` message)
9. PS-defined error handlers produce formatted error messages

---

## Deferred to Later Phases

- Remaining operators to reach 345 (userpath, insideness, packed arrays, CIE color)
- Interactive REPL improvements (rustyline, command history)
- CID fonts / CMap / FontSet loaders
- Pattern / Form / Shading execution
- eexec operator (not needed — xforge parses .t1 natively)
