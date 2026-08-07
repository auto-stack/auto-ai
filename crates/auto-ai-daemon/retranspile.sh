#!/usr/bin/env bash
# re-transpile ALL .at → a2r → assemble into rust/src/
#
# Flat layout (same as auto-ai-client):
#   src/X.at  → rust/src/X.rs
#
# Plus one hand-written glue module:
#   rust/src/tier_router_glue.rs   (exposes `tier_routing.routes` — `routes` is a
#                                   reserved keyword in .at, so it can't be read
#                                   from the transpiled tier_router.rs)
#
# Usage: ./retranspile.sh [check]
#
# Plan 025 Phase 1: a2r codegen workarounds. Each sed block targets a specific
# a2r defect (mirrors the pattern in auto-ai-client/retranspile.sh Plan 020 and
# auto-ai-agent/retranspile.sh Plan 019/021). They become no-ops once a2r's root
# causes are fixed.
set -euo pipefail

# The Auto→Rust transpiler. Assumed on PATH (cargo install from auto-lang, or
# built locally). Override with $AUTO.
AUTO="${AUTO:-auto}"
CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$CRATE_DIR/src"
RUST="$CRATE_DIR/rust/src"

# ── extern-crate shim (a2r emits `use crate::ai_config::...`) ───────────────
# Expose the whole external ai-config crate under `crate::ai_config` so both
# root-reexported symbols (DaemonConfig, ModelTier, ...) and submodule paths
# (loader::TierRouting) resolve.
read_shims() {
    cat <<'SHIMS'
// ── extern-crate shim (a2r emits `use crate::ai_config::...`).
pub mod ai_config {
    pub use ::ai_config::*;
}

SHIMS
}

# ── pub mod declarations for every module file in rust/src/ ──
# tier_router_glue.rs is hand-written (not transpiled) but still declared.
read_pub_mods() {
    for f in "$RUST"/*.rs; do
        local stem
        stem=$(basename "$f" .rs)
        case "$stem" in
            lib|main) continue ;;
        esac
        echo "pub mod ${stem};"
    done
}

echo "[retranspile] transpiling all .at files..."
transpile_one() {
    local f="$1"
    A2R_CRATE_ROOT=0 "$AUTO" trans --path "$f" rust >/dev/null 2>&1 || true
}
while IFS= read -r f; do
    transpile_one "$f"
done < <(find "$SRC" -name "*.at")

echo "[retranspile] assembling into rust/src/ ..."

mkdir -p "$RUST"

copy_if_exists() {
    local src="$1" dst="$2"
    if [ -f "$src" ]; then
        cp "$src" "$dst"
    else
        echo "  [skip] $(basename "$src" .a2r.rs).at failed to transpile — keeping existing $(basename "$dst")"
    fi
}

for f in "$SRC"/*.at; do
    bn=$(basename "$f" .at)
    copy_if_exists "$SRC/${bn}.a2r.rs" "$RUST/${bn}.rs"
done

# Assemble lib.rs: header + injected shim + pub mod declarations.
# (No lib.at crate root yet — lib.rs is assembled from these pieces. The Phase 0
# spike main.rs stays hand-written for now; Phase 2 will transpile it.)
{
    echo "// Auto-assembled by retranspile.sh (Plan 025 Phase 1)."
    echo "// lib stub — Phase 1 modules appended below as they come online."
    echo ""
    read_shims
    read_pub_mods
} > "$RUST/lib.rs"
echo "  [lib] assembled lib.rs (shim + pub mod decls)"

find "$SRC" -name "*.a2r.rs" -delete

# ═══════════════════════════════════════════════════════════════════════════
# Plan 025 Phase 1: a2r codegen workarounds (mechanical post-fixes per file).
# Each targets a specific a2r defect; becomes a no-op once a2r is fixed.
# ═══════════════════════════════════════════════════════════════════════════

# ── sse.rs ──────────────────────────────────────────────────────────────────
# E0507: str_find(self.buf, ...) moves the owned field — borrow instead
# (Plan 019 D-class, same fix as auto-ai-client/retranspile.sh).
[ -f "$RUST/sse.rs" ] && sed -i 's#a2r_std::str_find(self\.buf,#a2r_std::str_find(\&self.buf,#g' "$RUST/sse.rs"

# ── config.rs ───────────────────────────────────────────────────────────────
if [ -f "$RUST/config.rs" ]; then
    # a2r spurious `impl` keyword on struct return types.
    sed -i 's/-> impl DaemonConfig/-> DaemonConfig/g' "$RUST/config.rs"
    # Bare `DaemonConfig` (return types) not in scope — inject a use after the
    # a2r-emitted `use crate::ai_config;`.
    sed -i '/^use crate::ai_config;$/a use crate::ai_config::DaemonConfig;' "$RUST/config.rs"
    # ProviderConfig: a2r emits a positional tuple ctor; the type is a struct.
    sed -i 's#return ai_config::ProviderConfig(kind, base_url, Some(key), None, models, Some(DEFAULT_CONCURRENCY), true);#return ai_config::ProviderConfig { kind: kind.to_string(), base_url: base_url.to_string(), api_key: Some(key.to_string()), key_env: None, models: models, max_concurrency: Some(DEFAULT_CONCURRENCY), auth_required: true };#' "$RUST/config.rs"
    # const inferred as i32; rust-ref's max_concurrency is Option<usize>.
    sed -i 's#const DEFAULT_CONCURRENCY: i32 = 4;#const DEFAULT_CONCURRENCY: usize = 4;#' "$RUST/config.rs"
    # `.as_str()` on a &str value uses the unstable `str_as_str` feature — drop
    # it (the bridged fns take &str; a2r auto-borrows).
    sed -i 's#path_str\.as_str()#path_str#g' "$RUST/config.rs"
    # a2r renders the bridged static `dirs::home_dir` as `dirs.home_dir()` —
    # convert `.` to `::` for associated-fn calls.
    sed -i 's#return dirs\.home_dir();#return dirs::home_dir();#' "$RUST/config.rs"
fi

# ── tracker.rs ──────────────────────────────────────────────────────────────
if [ -f "$RUST/tracker.rs" ]; then
    # HashMap::get returns Option<&V>; clone the borrow before returning.
    sed -i 's#Some(entry) => return entry,#Some(entry) => return entry.clone(),#' "$RUST/tracker.rs"
    # `guard.get(&name)` — name is &String; HashMap wants Borrow<str>. Use as_str.
    # (Only the `all()` loop hits this; leave other .get(name) calls — they pass
    # the loop var which is already &String there. Target the &&String form.)
    sed -i 's#match guard.get(&name) {#match guard.get(name) {#' "$RUST/tracker.rs"
    # Tuple push with borrowed elements — clone both. (Use | delimiter: the
    # replacement contains ( ) and , which clash with the usual # separator.)
    sed -i 's|Some(u) => out.push((name, u)),|Some(u) => out.push((name.clone(), u.clone())),|' "$RUST/tracker.rs"
fi

# ── tier_router.rs ──────────────────────────────────────────────────────────
if [ -f "$RUST/tier_router.rs" ]; then
    # Hand-written glue module — qualify the a2r-emitted bare `use tier_router_glue;`.
    sed -i 's#^use tier_router_glue;#use crate::tier_router_glue;#' "$RUST/tier_router.rs"
    # ModelTier::parse_name takes &str; tier_name is String (from .collect()).
    sed -i 's#ModelTier::parse_name(tier_name)#ModelTier::parse_name(tier_name.as_str())#g' "$RUST/tier_router.rs"
    # HashMap::get needs &K; a2r passes the owned key.
    sed -i 's#self\.routing\.get(tier)#self.routing.get(\&tier)#g' "$RUST/tier_router.rs"
    sed -i 's#routing\.get(tier)#routing.get(\&tier)#g' "$RUST/tier_router.rs"
    # candidates() returns Vec but map value is &Vec — clone.
    sed -i 's#Some(v) => return v,#Some(v) => return v.clone(),#' "$RUST/tier_router.rs"
    # Vec::remove takes usize; a2r emits the i32 index verbatim.
    sed -i 's#chain\.remove(idx)#chain.remove(idx as usize)#' "$RUST/tier_router.rs"
    # resolve() returns Option<&TierCandidate> from a borrow — clone.
    sed -i 's#return Some(c);#return Some(c.clone());#' "$RUST/tier_router.rs"
    # ensure_candidate: `list` from map.get is &Vec; clone before mutating.
    sed -i 's#let mut updated = list;#let mut updated = list.clone();#' "$RUST/tier_router.rs"
    sed -i 's#routing\.insert(tier, updated\.clone());#routing.insert(tier, updated);#' "$RUST/tier_router.rs"
    # ensure_candidate match arms: Some block ends with `;` (→ ()) but None arm
    # `routing.insert(...)` returns Option — wrap None in a block to match ().
    sed -i 's#None => routing\.insert(tier, vec!\[tc\]),#None => { routing.insert(tier, vec![tc]); },#' "$RUST/tier_router.rs"
    # candidates_preferred: index_of_provider(chain, ...) moves chain, but chain
    # is reused after — pass a clone.
    sed -i 's#let idx = index_of_provider(chain,#let idx = index_of_provider(chain.clone(),#' "$RUST/tier_router.rs"
    # Provider-loop .collect() has no type hint → HashMap::get Q infer fails.
    # (Use | delimiter; the turbofish contains < > that confuse the # form.)
    sed -i 's|for name in config\.providers\.keys()\.cloned()\.collect() {|for name in config.providers.keys().cloned().collect::<Vec<String>>() {|' "$RUST/tier_router.rs"
    # config.providers.get(&name) — &String isn't Borrow<str>-inferrable here.
    sed -i 's#config\.providers\.get(&name)#config.providers.get(name.as_str())#' "$RUST/tier_router.rs"
    # has_provider_for_tier: `cands` from map.get is already &Vec; `&cands` is
    # &&Vec (not iterable). Iterate `cands` directly. (Two distinct contexts:
    # this helper, where cands is borrowed; resolve() keeps `&cands` since its
    # cands is owned. Fixed in-place below — only the helper's loop.)
    # NOTE: applied via line-targeted fix below (see retranspile verification).
fi

# ── format.rs ───────────────────────────────────────────────────────────────
if [ -f "$RUST/format.rs" ]; then
    # Helpers all_tool_results/has_tool_use move `blocks`; borrow at the call
    # site and in the signature so openai_content keeps owning it.
    sed -i 's#all_tool_results(blocks)#all_tool_results(\&blocks)#' "$RUST/format.rs"
    sed -i 's#has_tool_use(blocks)#has_tool_use(\&blocks)#' "$RUST/format.rs"
    sed -i 's#fn all_tool_results(blocks: Vec<ContentBlock>)#fn all_tool_results(blocks: \&Vec<ContentBlock>)#' "$RUST/format.rs"
    sed -i 's#fn has_tool_use(blocks: Vec<ContentBlock>)#fn has_tool_use(blocks: \&Vec<ContentBlock>)#' "$RUST/format.rs"
    # Value::String(name/id) — name/id are &str from enum destructuring.
    sed -i 's#Value::String(name)#Value::String(name.to_string())#g' "$RUST/format.rs"
    sed -i 's#Value::String(id)#Value::String(id.to_string())#g' "$RUST/format.rs"
    # With blocks now &Vec in the helpers, `for b in &blocks` is &&Vec. Fix the
    # two helper loops (lines shift per transpile; target by the surrounding
    # signature context via awk).
    awk '
        /^fn all_tool_results/ { in_altr=1 }
        /^fn has_tool_use/     { in_hast=1 }
        in_altr && /for b in &blocks \{/ { sub(/&blocks/, "blocks"); in_altr=0 }
        in_hast  && /for b in &blocks \{/ { sub(/&blocks/, "blocks"); in_hast=0 }
        { print }
    ' "$RUST/format.rs" > "$RUST/format.rs.tmp" && mv "$RUST/format.rs.tmp" "$RUST/format.rs"
    # tier_router.rs has_provider_for_tier: same &&Vec issue (cands from map.get).
    if [ -f "$RUST/tier_router.rs" ]; then
        awk '
            /^fn has_provider_for_tier/ { in_hpf=1 }
            in_hpf && /for c in &cands \{/ { sub(/&cands/, "cands"); in_hpf=0 }
            { print }
        ' "$RUST/tier_router.rs" > "$RUST/tier_router.rs.tmp" && mv "$RUST/tier_router.rs.tmp" "$RUST/tier_router.rs"
    fi
fi

echo "[retranspile] assembly complete."

if [ "${1:-}" = "check" ]; then
    echo "[retranspile] running cargo check..."
    cd "$RUST/.."
    n=$(cargo check --color never 2>&1 | grep -cE "^error" || true)
    echo "[retranspile] error count: $n"
    if [ "$n" -ne 0 ]; then
        exit 1
    fi
fi
