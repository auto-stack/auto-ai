#!/usr/bin/env bash
# Plan 376U: re-transpile ALL .at → a2r → assemble into rust/src/
#
# Crate-root files (lib.at, */mod.at) are transpiled with A2R_CRATE_ROOT=1,
# which makes a2r emit `pub use` (re-exports) + `#![allow(...)]` pragma.
# After transpilation, this script INJECTS the assembly-layer scaffolding
# a2r cannot know about:
#   - extern-crate shims (pub mod auto_ai_client { pub use ::auto_ai_client_a2r::*; })
#   - `pub mod X;` declarations for hoisted modules
#
# Assembly rules (hoisting):
#   src/X.at                 → rust/src/X.rs
#   src/lib.at               → rust/src/lib.rs       (crate root + injected shims/mods)
#   src/orchestration/X.at   → rust/src/X.rs         (hoisted to root)
#       mod.at               → rust/src/orchestration.rs (crate-root aggregator)
#   src/builtin_roles/X.at   → rust/src/builtin_role_X.rs
#       mod.at               → rust/src/builtin_roles.rs (crate-root aggregator)
#   src/config/X.at          → rust/src/X.rs
#       mod.at               → rust/src/config.rs    (crate-root aggregator)
#
# Hand-written glue files (NOT overwritten, have no .at source):
#   client_impl.rs, echo_tool.rs, main.rs
#
# Usage: ./retranspile.sh [check]
#   (no arg)  transpile + assemble, leave rust/src/ modified
#   check     after assembling, run cargo check and report error count
set -euo pipefail

AUTO="${AUTO:-auto}"
AGENT_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$AGENT_DIR/src"
RUST="$AGENT_DIR/rust/src"

# ── extern-crate shims (a2r emits `use crate::<these>::...`, needs a shim) ──
# wire is special: it re-exports ai_config::wire + adds the JsonValue alias.
read_shims() {
    cat <<'SHIMS'
// ── extern-crate shims (a2r emits `use crate::<these>::...`) ────────────────
pub mod auto_ai_client {
    // Plan 024: agent now consumes the TRANSPILED client (auto-ai-client-a2r),
    // not rust-ref. The shim re-exports it under the name agent .at source uses
    // (auto_ai_client), so the rest of the crate is unaffected.
    pub use ::auto_ai_client_a2r::*;
}
pub mod ai_config {
    pub use ::ai_config::*;
}
pub mod wire {
    pub use ::ai_config::wire::*;
    // a2r references `crate::wire::JsonValue` (generic JSON blob = serde_json::Value).
    pub type JsonValue = serde_json::Value;
}

// ── hand-written glue (no .at source) ──────────────────────────────────────
pub mod client_impl;
pub mod echo_tool;

SHIMS
}

# ── pub mod declarations for every module file in rust/src/ (hoisted flat) ──
read_pub_mods() {
    # Every .rs file except lib.rs/main.rs/client_impl.rs/echo_tool.rs gets a
    # `pub mod <stem>;` declaration. lib.rs is this file; main/client_impl/echo
    # are hand-written glue declared above or are the binary entry.
    for f in "$RUST"/*.rs; do
        local stem
        stem=$(basename "$f" .rs)
        case "$stem" in
            lib|main|client_impl|echo_tool) continue ;;
        esac
        echo "pub mod ${stem};"
    done
}

echo "[retranspile] transpiling all .at files..."
# Transpile every .at file. Crate-root files (lib.at, */mod.at) get
# A2R_CRATE_ROOT=1 so a2r emits pub use + #![allow].
transpile_one() {
    local f="$1"
    local base
    base=$(basename "$f" .at)
    local crate_root=0
    if [ "$base" = "lib" ] || [ "$base" = "mod" ]; then
        crate_root=1
    fi
    A2R_CRATE_ROOT="$crate_root" "$AUTO" trans --path "$f" rust >/dev/null 2>&1 || true
}
while IFS= read -r f; do
    transpile_one "$f"
done < <(find "$SRC" -name "*.at")

echo "[retranspile] assembling into rust/src/ ..."

# Helper: copy a2r.rs → dest.rs only if the a2r.rs exists (transpile succeeded).
copy_if_exists() {
    local src="$1" dst="$2"
    if [ -f "$src" ]; then
        cp "$src" "$dst"
    else
        echo "  [skip] $(basename "$src" .a2r.rs).at failed to transpile — keeping existing $(basename "$dst")"
    fi
}

# Flat src/*.at → rust/src/<name>.rs  (lib.at handled separately below)
for f in "$SRC"/*.at; do
    bn=$(basename "$f" .at)
    [ "$bn" = "lib" ] && continue
    copy_if_exists "$SRC/${bn}.a2r.rs" "$RUST/${bn}.rs"
done

# orchestration/*.at → rust/src/<name>.rs (hoisted), mod.at → orchestration.rs
for f in "$SRC"/orchestration/*.at; do
    bn=$(basename "$f" .at)
    if [ "$bn" = "mod" ]; then
        copy_if_exists "$SRC/orchestration/mod.a2r.rs" "$RUST/orchestration.rs"
    else
        copy_if_exists "$SRC/orchestration/${bn}.a2r.rs" "$RUST/${bn}.rs"
    fi
done

# config/*.at → rust/src/<name>.rs, mod.at → config.rs
for f in "$SRC"/config/*.at; do
    bn=$(basename "$f" .at)
    if [ "$bn" = "mod" ]; then
        copy_if_exists "$SRC/config/mod.a2r.rs" "$RUST/config.rs"
    else
        copy_if_exists "$SRC/config/${bn}.a2r.rs" "$RUST/${bn}.rs"
    fi
done

# builtin_roles/*.at → rust/src/builtin_role_<name>.rs, mod.at → builtin_roles.rs
for f in "$SRC"/builtin_roles/*.at; do
    bn=$(basename "$f" .at)
    if [ "$bn" = "mod" ]; then
        copy_if_exists "$SRC/builtin_roles/mod.a2r.rs" "$RUST/builtin_roles.rs"
    else
        copy_if_exists "$SRC/builtin_roles/${bn}.a2r.rs" "$RUST/builtin_role_${bn}.rs"
    fi
done

# ── Assemble lib.rs: transpiled crate-root + injected shims + pub mod decls ─
# The transpiled lib.a2r.rs has the #![allow] pragma + pub use re-exports.
# We inject (after #![allow], before pub use): extern shims + pub mod decls.
if [ -f "$SRC/lib.a2r.rs" ]; then
    awk -v shims="$(read_shims)" -v pubmods="$(read_pub_mods)" '
        /^#!\[allow/ { print; print ""; print shims; print pubmods; next }
        { print }
    ' "$SRC/lib.a2r.rs" > "$RUST/lib.rs"
    echo "  [lib] assembled lib.rs (crate-root transpile + shims + pub mod decls)"
else
    echo "  [skip] lib.at failed to transpile — keeping existing lib.rs"
fi

# Plan 016 3.1: fix const SOUL type — comptime read_text produces a string
# literal but a2r infers the const type as /* unknown */. Replace with &str.
for f in "$RUST"/builtin_role_*.rs; do
    [ -f "$f" ] || continue
    sed -i 's#const SOUL: /\* unknown \*/#const SOUL: \&str#' "$f"
done

# Plan 019 Phase 2: a2r borrow-reasoning workarounds. These are mechanical
# post-fixes for 4 classes of a2r transpiler defects (clone emission, ReadDir
# borrow, path move, redundant as_str). Each targets a specific pattern; when
# the a2r root cause is fixed upstream these sed rules become no-ops.
#   B: borrowed loop var field passed to owned param → needs .clone()
#   C: for-in unconditionally borrows; ReadDir only impl IntoIterator by-value
#   D: read_to_string(path) moves path; later code reuses it → borrow instead
#   E: redundant .as_str() inserted on a value already typed &str (E0658)
for f in "$RUST"/driver.rs "$RUST"/agent.rs "$RUST"/skill.rs "$RUST"/roles.rs; do
    [ -f "$f" ] || continue
    # B: driver.rs — extract_path(tc.args) + WorkProduct { .. tc.tool .. }
    sed -i 's#extract_path(tc\.args)#extract_path(tc.args.clone())#g;
            s#description: tc\.tool,#description: tc.tool.clone(),#g' "$f"
    # B: agent.rs — tool_to_definition(t) where t is &Arc
    sed -i 's#tool_to_definition(t)#tool_to_definition(t.clone())#g' "$f"
    # C: skill.rs/roles.rs — for entry in &entries → for entry in entries
    #     (entries is ReadDir from fs::read_dir; &ReadDir is not an iterator)
    sed -i 's#for entry in &entries {#for entry in entries {#g' "$f"
    # D: read_to_string(path) → read_to_string(&path) (avoid moving path)
    sed -i 's#read_to_string(path)#read_to_string(\&path)#g' "$f"
    # E: skill.rs — after_open.as_str() is redundant (after_open is already &str)
    sed -i 's#after_open\.as_str()#after_open#g' "$f"
done

# Plan 021: a2r (auto-lang Plan 387 §16 aftermath) now renders Auto's
# `mut eng PipelineEngine` parameter as `&mut PipelineEngine` (borrow inference
# change), and the call site as `&mut self.clone()` / `&mut h.clone()`. Align
# both back to by-value to match the .at source intent.
if [ -f "$RUST/pipeline.rs" ]; then
    sed -i 's#fn correct_handoff_target(eng: &mut PipelineEngine, mut h: &mut HandoffDocument,#fn correct_handoff_target(mut eng: PipelineEngine, mut h: HandoffDocument,#g;
            s#correct_handoff_target(\&mut self.clone(), \&mut h.clone(),#correct_handoff_target(self.clone(), h.clone(),#g' "$RUST/pipeline.rs"
fi

# Plan 021 缺口 3 (post-Plan 395): turbofish is now native Auto syntax
# (`node.deserialize<RoleDecl>()`), so no sed injection is needed.

# Plan 390 §15.11-followup (cross-module spec, 2026-08-06): a2r now single-wraps
# Arc<Spec> / Box<Spec> even for specs IMPORTED via `use mod: Spec` (was only
# same-module before). The previous sed (forcing register_shared param single-
# wrap) is removed — a2r renders `Arc<dyn Tool>` natively now.

# Clean up .a2r.rs intermediates
find "$SRC" -name "*.a2r.rs" -delete

echo "[retranspile] assembly complete."

if [ "${1:-}" = "check" ]; then
    echo "[retranspile] running cargo check..."
    cd "$RUST/.."
    n=$(cargo check --color never 2>&1 | grep -cE "^error" || true)
    echo "[retranspile] error count: $n"
fi
