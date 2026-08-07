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
    echo "// Auto-assembled by retranspile.sh (Plan 025 Phase 1+2)."
    echo "// lib stub — Phase 1/2 modules appended below as they come online."
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
    # record is called via &Arc<AppState> (state.tracker.record) — it can't be
    # &mut self. The body only uses the inner parking_lot Mutex (interior
    # mutability), so &self suffices. a2r made it &mut self by default.
    sed -i 's|pub fn record(&mut self, app: &str|pub fn record(\&self, app: \&str|' "$RUST/tracker.rs"
    # all() iterates `names`, which is now a MutexGuard<Vec<String>> (Phase 3.4
    # wrapped names in Mutex for &self record). Iterate via .iter() — the guard
    # derefs to Vec but for-in needs an explicit iterator.
    sed -i 's|for name in &names {|for name in names.iter() {|' "$RUST/tracker.rs"
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
    # from_config is a constructor, but a2r adds `&self` to `ext` methods AND
    # takes config by value. Make it an associated fn taking &DaemonConfig
    # (Phase 3 — server.at passes &config, config is reused).
    sed -i 's#pub fn from_config(&self, config: DaemonConfig)#pub fn from_config(config: \&DaemonConfig)#' "$RUST/tier_router.rs"
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

# ── pool.rs (Phase 2) ───────────────────────────────────────────────────────
# Same HashMap::get borrow/turbofish class as tier_router: `&name` (String) and
# `&name` (&String) aren't Borrow<str>-inferrable; materialize the keys with a
# turbofish and look up via .as_str(); clone the &String before pushing.
if [ -f "$RUST/pool.rs" ]; then
    sed -i 's|for name in config\.providers\.keys()\.cloned()\.collect() {|for name in config.providers.keys().cloned().collect::<Vec<String>>() {|' "$RUST/pool.rs"
    sed -i 's|match config\.providers\.get(&name) {|match config.providers.get(name.as_str()) {|' "$RUST/pool.rs"
    sed -i 's|match self\.pools\.get(&name) {|match self.pools.get(name.as_str()) {|' "$RUST/pool.rs"
    sed -i 's|let max = self\.limits\.get(&name)\.copied()\.unwrap_or(0);|let max = self.limits.get(name.as_str()).copied().unwrap_or(0);|' "$RUST/pool.rs"
    sed -i 's|out\.push((name, sem\.available_permits(), max));|out.push((name.clone(), sem.available_permits(), max));|' "$RUST/pool.rs"
    # from_config is a constructor (returns Self), but a2r adds `&self` to every
    # `ext` method AND takes config by value. rust-ref's from_config takes
    # &DaemonConfig (config is reused — moved into AppState's RwLock afterwards).
    # Convert the signature: drop &self, make config a reference. Phase 3
    # surfaced this — it was latent while from_config had no caller.
    sed -i 's|pub fn from_config(&self, config: DaemonConfig)|pub fn from_config(config: \&DaemonConfig)|' "$RUST/pool.rs"
fi

# ── error.rs (Phase 2) ──────────────────────────────────────────────────────
if [ -f "$RUST/error.rs" ]; then
    # Struct-variant match binds fields by reference: `retryable` is &bool, deref.
    sed -i 's|=> return retryable,|=> return *retryable,|' "$RUST/error.rs"
    # Upstream.status field is uint (u32); status.as_u16() is u16; body is &str.
    sed -i 's|status: code, message: body|status: code as u32, message: body.to_string()|' "$RUST/error.rs"
    # a2r auto-generates impl Display + impl Error from the .message() method,
    # but NOT impl From<reqwest::Error> (Auto has no impl From). Append it so the
    # Phase-4 providers' `?` / `.into()` on reqwest::Error resolve. Mirrors the
    # .from_reqwest_error() factory in error.at.
    if ! grep -q "impl From<reqwest::Error> for LlmError" "$RUST/error.rs"; then
        cat >> "$RUST/error.rs" <<'FROMEOF'

// Injected by retranspile.sh: impl From<reqwest::Error> (a2r can't emit impl
// From; mirrors the from_reqwest_error() factory. Classifies timeouts so the
// tier router treats them as retryable.)
impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Self::Timeout(e.to_string());
        }
        Self::Http(e.to_string())
    }
}
FROMEOF
    fi
fi

# ── provider.rs (Phase 2) ───────────────────────────────────────────────────
if [ -f "$RUST/provider.rs" ]; then
    # from_daemon_config delegates to the hand-written provider_glue.rs build
    # fn; a2r renders the module-qualified call as a method (`.` → use `::`).
    sed -i 's|return provider_glue\.build_registry(config);|return crate::provider_glue::build_registry(\&config);|' "$RUST/provider.rs"
    # default_provider / get return &Arc<dyn AiProvider> from map.get — clone.
    sed -i 's|Some(p) => return Ok(p),|Some(p) => return Ok(p.clone()),|' "$RUST/provider.rs"
    sed -i 's|Some(p) => return Some(p),|Some(p) => return Some(p.clone()),|' "$RUST/provider.rs"
    # from_daemon_config is a constructor, but a2r adds `&self` to `ext`
    # methods AND takes config by value. Make it an associated fn taking
    # &DaemonConfig (Phase 3 — server.at passes &config, config is reused).
    sed -i 's|pub fn from_daemon_config(&self, config: DaemonConfig)|pub fn from_daemon_config(config: \&DaemonConfig)|' "$RUST/provider.rs"
    # complete/complete_stream take &CompletionRequest (rust-ref signature), but
    # a2r renders the ext-method param by value. Make them references so
    # server.at's provider.complete(&req) resolves (Phase 3.4). These are trait
    # method signatures (async fn in the spec), not pub fn.
    sed -i 's|async fn complete(&self, req: CompletionRequest)|async fn complete(\&self, req: \&CompletionRequest)|' "$RUST/provider.rs"
    # complete_stream: same — rust-ref takes &CompletionRequest, a2r renders by
    # value. Phase 3.6's streaming_response (in server_glue.rs) passes &req.
    sed -i 's|async fn complete_stream(&self, req: CompletionRequest,|async fn complete_stream(\&self, req: \&CompletionRequest,|' "$RUST/provider.rs"
fi

echo "[retranspile] assembly complete."

# ── main.rs (Phase 3) ───────────────────────────────────────────────────────
# main.rs is HAND-WRITTEN (not transpiled) — same pattern as auto-ai-agent's
# bin. a2r can't emit println!/eprintln! macros (renders them as fn calls),
# routes env.args() to a nonexistent a2r_std::env::args, and double-emits
# #[tokio::main]. The bin is pure glue (arg parse + bind + serve); the axum
# validation milestone lives in server.at. So no main.at exists and this block
# is intentionally empty — main.rs is edited by hand.

# ── server.rs (Phase 3) ─────────────────────────────────────────────────────
# server_glue.build_router: a2r renders the module-qualified call as a method
# (`.` → use `::`), same class as provider_glue/tier_router_glue.
if [ -f "$RUST/server.rs" ]; then
    sed -i 's|return server_glue\.build_router(state);|return crate::server_glue::build_router(state);|' "$RUST/server.rs"
    # server_glue.config_provider_models: same `.` → `::` class as build_router.
    sed -i 's|server_glue\.config_provider_models|crate::server_glue::config_provider_models|g' "$RUST/server.rs"
    # config_provider_models takes &DaemonConfig; `cfg` is a RwLockReadGuard
    # (from state.cfg()) — pass &cfg so it derefs to &DaemonConfig.
    sed -i 's|config_provider_models(cfg)|config_provider_models(\&cfg)|' "$RUST/server.rs"
    # resolve_model_id takes &[ModelDefinition]; models is Vec — borrow it.
    sed -i 's|resolve_model_id(tier, models)|resolve_model_id(tier, \&models)|' "$RUST/server.rs"
    # `~IntoResponse` should lower to `-> impl IntoResponse` (golden 015), but
    # with an extractor in the param list a2r drops the `impl` keyword → bare
    # `-> IntoResponse` (E0782: expected a type, found a trait). Re-insert it.
    sed -i 's|-> IntoResponse {|-> impl IntoResponse {|g' "$RUST/server.rs"
    # serde_json::Value::Number takes a `serde_json::Number`, not a raw integer
    # (the json! macro coerces implicitly; we build Value by hand). Wrap each
    # Value::Number(<expr>) in serde_json::Number::from(...). The sed targets
    # the exact expressions we emit in server.at.
    sed -i 's|Value::Number(available)|Value::Number(serde_json::Number::from(available))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(max)|Value::Number(serde_json::Number::from(max))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(max - available)|Value::Number(serde_json::Number::from(max - available))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(u.total_input_tokens)|Value::Number(serde_json::Number::from(u.total_input_tokens))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(u.total_output_tokens)|Value::Number(serde_json::Number::from(u.total_output_tokens))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(u.total_tokens())|Value::Number(serde_json::Number::from(u.total_tokens()))|g' "$RUST/server.rs"
    sed -i 's|Value::Number(u.request_count)|Value::Number(serde_json::Number::from(u.request_count))|g' "$RUST/server.rs"
    # resolve_tier_model: ModelTier::parse_name takes &str; tier_name is String
    # (from .to_ascii_lowercase()). Same borrow class as tier_router.
    sed -i 's|ModelTier::parse_name(tier_name)|ModelTier::parse_name(tier_name.as_str())|g' "$RUST/server.rs"
    # a2r renders `provider.models` (struct field access) as `provider::models`
    # (module path) — wrong. Restore the field-access dot. (Only occurrence in
    # resolve_tier_model; the AppState field `state.pool` etc. render fine.)
    sed -i 's|let models = provider::models;|let models = provider.models.clone();|' "$RUST/server.rs"
    # models handler: `m` is &ModelDefinition (from iterating &Vec); m.id is a
    # borrowed String — Value::String needs owned. Same .to_string() class as
    # format.rs's Value::String(name).
    sed -i 's|Value::String(m.id)|Value::String(m.id.to_string())|' "$RUST/server.rs"
    # ── chat_completions (Phase 3.4) ──
    # `var req = req` picks up the Json<CompletionRequest> type from the
    # extractor param, but the extractor already unwrapped it to CompletionRequest.
    # Drop the spurious Json<> type annotation.
    sed -i 's|let mut req: Json<CompletionRequest> = req;|let mut req = req;|' "$RUST/server.rs"
    # a2r renders `provider.complete(req)` (trait method call) as
    # `provider::complete` (module path) — wrong. Restore the method dot. The
    # complete signature is fixed to &CompletionRequest in provider.rs (below),
    # so pass &req here.
    sed -i 's|provider::complete(req)|provider.complete(\&req)|' "$RUST/server.rs"
    # streaming_response: server_glue. → crate::server_glue:: (same as build_router).
    sed -i 's|server_glue\.streaming_response|crate::server_glue::streaming_response|' "$RUST/server.rs"
    # streaming_response is async (Phase 3.6 made it a real impl) — the call in
    # chat_completions's streaming branch needs .await.
    sed -i 's|return crate::server_glue::streaming_response(state, app_name, provider, req, permit);|return crate::server_glue::streaming_response(state, app_name, provider, req, permit).await;|' "$RUST/server.rs"
    # candidates.push((c.provider, c.model)): c is &TierCandidate (iterating
    # &chain) — clone the borrowed String fields before moving into the tuple.
    sed -i 's|candidates.push((c.provider, c.model))|candidates.push((c.provider.clone(), c.model.clone()))|' "$RUST/server.rs"
    # resolve_tier_model(req.model, cfg.clone()): cfg is a RwLockReadGuard; the
    # fn takes &DaemonConfig. Pass &cfg (deref-coerces), drop the .clone().
    sed -i 's|resolve_tier_model(req.model, cfg.clone())|resolve_tier_model(\&req.model, \&cfg)|' "$RUST/server.rs"
    # acquire_with_timeout(provider_name, ...): provider_name is &String (entry.0
    # from &candidates); the fn takes &str — as_str().
    sed -i 's|acquire_with_timeout(provider_name,|acquire_with_timeout(provider_name.as_str(),|' "$RUST/server.rs"
    # state.registry.get(provider_name): same &String → as_str().
    sed -i 's|state.registry.get(provider_name)|state.registry.get(provider_name.as_str())|' "$RUST/server.rs"
    # ok_response(resp.clone()): resp is owned (from provider.complete), no
    # clone needed.
    sed -i 's|ok_response(resp.clone())|ok_response(resp)|' "$RUST/server.rs"
    # req.model[5..].to_string(): a2r renders slice(5) as [5..] on a &str field —
    # borrow issues. Use the .slice() → [5..] form but it needs the right type.
    # (Leave as-is; [5..] on String works after the mut req fix above.)
    # match arms incompatible: last_error match returns String or &str — unify.
    sed -i 's|let msg = match last_error { Some(e) => e, None => "unknown", };|let msg = match last_error { Some(e) => e, None => "unknown".to_string(), };|' "$RUST/server.rs"
    # resolve_tier_model(&req.model, &cfg): &cfg is &RwLockReadGuard, not
    # &DaemonConfig — deref-coerce with &*cfg.
    sed -i 's|resolve_tier_model(&req.model, &cfg)|resolve_tier_model(\&req.model, \&*cfg)|' "$RUST/server.rs"
    # tracker.record(app_name, ...): app_name is String, record takes &str.
    sed -i 's|state.tracker.record(app_name,|state.tracker.record(app_name.as_str(),|g' "$RUST/server.rs"
    # error_response: message is &str, Value::String needs owned String.
    sed -i 's|Value::String(message)|Value::String(message.to_string())|g' "$RUST/server.rs"
    # ok_response: serde_json::to_value returns Result — unwrap it.
    sed -i 's|let body = serde_json::to_value(resp);|let body = serde_json::to_value(\&resp).unwrap_or(Value::Null);|' "$RUST/server.rs"
    # resolve_tier_model signature: config by value, but caller passes &DaemonConfig
    # (via &*cfg deref of the RwLockReadGuard). Make the param a reference.
    sed -i 's|fn resolve_tier_model(token: &str, config: DaemonConfig)|fn resolve_tier_model(token: \&str, config: \&DaemonConfig)|' "$RUST/server.rs"
    # ── borrow/move fixes (Phase 3.4 borrow-checker fallout) ──
    # cfg is a RwLockReadGuard; cfg.default_provider is an owned String behind the
    # guard deref — clone before moving into the tuple.
    sed -i 's|candidates.push((cfg.default_provider, resolved))|candidates.push((cfg.default_provider.clone(), resolved))|' "$RUST/server.rs"
    # `for (name, models)` — name is moved into `found = name` then reused by the
    # loop; clone at the assignment.
    sed -i 's|found = name;|found = name.clone();|' "$RUST/server.rs"
    # `for entry in &candidates` — entry is &(String, String); entry.0/.1 are
    # behind a shared ref. Clone before binding.
    sed -i 's|let provider_name = entry.0;|let provider_name = entry.0.clone();|' "$RUST/server.rs"
    sed -i 's|let model_id = entry.1;|let model_id = entry.1.clone();|' "$RUST/server.rs"
    # req.preferred_provider is moved into candidates_preferred, but req is reused
    # after — clone the field.
    sed -i 's|candidates_preferred(tier, req.preferred_provider)|candidates_preferred(tier, req.preferred_provider.clone())|' "$RUST/server.rs"
    # resp.usage is moved into the record() call, but resp is reused in
    # ok_response — borrow it instead (Option takes &Option<Usage>).
    sed -i 's|match resp.usage {|match \&resp.usage {|' "$RUST/server.rs"
    # AppState::new passes owned `config` to from_daemon_config/from_config, but
    # those take &DaemonConfig (and config is moved into the RwLock afterwards,
    # so borrowing at the calls is safe). Same borrow class as pool/tier_router
    # HashMap::get — add the & at each call site.
    sed -i 's|ProviderRegistry::from_daemon_config(config)|ProviderRegistry::from_daemon_config(\&config)|' "$RUST/server.rs"
    sed -i 's|ConcurrencyManager::from_config(config)|ConcurrencyManager::from_config(\&config)|' "$RUST/server.rs"
    sed -i 's|TierRouter::from_config(config)|TierRouter::from_config(\&config)|' "$RUST/server.rs"
    # from_config / from_daemon_config are CONSTRUCTORS (return Self), but a2r
    # renders `ext` methods with a leading `&self` (treating the first param as
    # the receiver). They're associated functions in rust-ref (no self). Strip
    # the spurious `&self, ` from these three constructor signatures so the
    # Type::from_config(&c) calls in server.rs resolve. (Applied in the source
    # modules' own sed blocks below — but those run before server.at exists, so
    # repeat here isn't needed; the fixes live in pool/provider/tier_router.)
fi

if [ "${1:-}" = "check" ]; then
    echo "[retranspile] running cargo check..."
    cd "$RUST/.."
    n=$(cargo check --color never 2>&1 | grep -cE "^error" || true)
    echo "[retranspile] error count: $n"
    if [ "$n" -ne 0 ]; then
        exit 1
    fi
fi
