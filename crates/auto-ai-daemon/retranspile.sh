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

# fix_provider_impl — common a2r fixes for a concrete AiProvider impl block
# (openai/anthropic/ollama). Each impl's complete/complete_stream must match the
# trait signature (Phase 3 sed changed the trait to take &CompletionRequest, so
# the impls must too — otherwise E0195 lifetime mismatch). Also rewrites the
# provider_glue delegation `.` → `crate::provider_glue::`.
fix_provider_impl() {
    local f="$1"
    # E0195: impl method signatures must match the trait. The trait (provider.rs)
    # was fixed to `req: &CompletionRequest`; apply the same to each impl.
    sed -i 's|async fn complete(&self, req: CompletionRequest)|async fn complete(\&self, req: \&CompletionRequest)|g' "$f"
    sed -i 's|async fn complete_stream(&self, req: CompletionRequest,|async fn complete_stream(\&self, req: \&CompletionRequest,|g' "$f"
    # build_body takes owned CompletionRequest, but complete now has &req — clone.
    sed -i 's|self.build_body(req)|self.build_body(req.clone())|g' "$f"
    # Constructors: a2r renders `pub static fn new(name str, ...)` params as
    # `&str`, but rust-ref (and provider_glue callers) pass owned String. The
    # struct fields are owned String, so new must take owned. (OpenAi/Anthropic/
    # Ollama all share this shape.)
    sed -i 's|pub fn new(name: \&str, base_url: \&str, api_key: \&str, models: Vec<String>)|pub fn new(name: String, base_url: String, api_key: String, models: Vec<String>)|g' "$f"
    sed -i 's|pub fn new(name: \&str, base_url: \&str, models: Vec<String>)|pub fn new(name: String, base_url: String, models: Vec<String>)|g' "$f"
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
    # Plan 028: a2r renders cross-crate struct literals as positional tuple
    # ctors (same class as the ProviderConfig fixup above) — rewrite the
    # model_meta return into a true struct literal. Cost u32 -> u64 widens.
    sed -i 's#return ai_config::ModelDefinition(id, "", tier, Some(window), Some(max_out), Some(ai_config::CostPerMtok(cost_in, cost_out, cache_read)), Some(ai_config::ModelCapabilities(vision, thinking)));#return ai_config::ModelDefinition { id: id.to_string(), name: String::new(), tier: tier, context_window: Some(window), max_output_tokens: Some(max_out), cost_per_mtok: Some(ai_config::CostPerMtok { input: cost_in as u64, output: cost_out as u64, cache_read: cache_read as u64 }), capabilities: Some(ai_config::ModelCapabilities { vision: vision, thinking: thinking }) };#' "$RUST/config.rs"

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
    # from_entries iterates `for entry in &entries` (Phase 4) — entry.0/.1 are
    # behind a shared ref (String / Arc). Clone before moving into insert/push.
    sed -i 's|providers.insert(name.clone(), provider)|providers.insert(name.clone(), provider.clone())|' "$RUST/provider.rs"
    sed -i 's|let name = entry.0;|let name = entry.0.clone();|' "$RUST/provider.rs"
    sed -i 's|let provider = entry.1;|let provider = entry.1.clone();|' "$RUST/provider.rs"
fi

# ── openai.rs (Phase 4) ─────────────────────────────────────────────────────
if [ -f "$RUST/openai.rs" ]; then
    fix_provider_impl "$RUST/openai.rs"
    # SseParser is crate-local (crate::sse), but a2r routes `use sse:` to
    # a2r_std::sse (which doesn't exist). Restore the crate path.
    sed -i 's|use a2r_std::sse::{SseParser};|use crate::sse::SseParser;|' "$RUST/openai.rs"
    sed -i 's|use a2r_std::sse::SseParser;|use crate::sse::SseParser;|' "$RUST/openai.rs"
    # complete_stream delegates to provider_glue (`.` → `::`).
    sed -i 's|provider_glue\.openai_complete_stream|crate::provider_glue::openai_complete_stream|' "$RUST/openai.rs"
    # build_body returns Value; provider_glue needs &serde_json::Value + mut index.
    # Value::Number(usize/f64) → Number::from (json! coerces implicitly).
    sed -i 's|Value::Number(n)|Value::Number(serde_json::Number::from(n))|g' "$RUST/openai.rs"
    sed -i 's|Value::Number(t)|Value::Number(serde_json::Number::from(t))|g' "$RUST/openai.rs"
    # temperature (t) is f64 — Number::from(f64) doesn't exist; use from_f64.
    sed -i 's|Value::Number(serde_json::Number::from(t))|Value::Number(serde_json::Number::from_f64(t).unwrap_or(serde_json::Number::from(0)))|g' "$RUST/openai.rs"
    sed -i 's|Value::Number(4096)|Value::Number(serde_json::Number::from(4096))|g' "$RUST/openai.rs"
    # from_upstream_status takes (&StatusCode, &str); status is u32 (resp.status_
    # code()), text is String — pass &text. Also status is u32 not StatusCode:
    # error.at's from_upstream_status bridges to the real signature post-sed.
    sed -i 's|from_upstream_status(status, text)|from_upstream_status(status, \&text)|g' "$RUST/openai.rs"
    # Value::String(<&str/&String>) needs owned String — .to_string() at each.
    sed -i 's|Value::String(content)|Value::String(content.to_string())|g' "$RUST/openai.rs"
    sed -i 's|Value::String(r.tool_call_id)|Value::String(r.tool_call_id.to_string())|g' "$RUST/openai.rs"
    sed -i 's|Value::String(r.content)|Value::String(r.content.to_string())|g' "$RUST/openai.rs"
    # Usage tokens: json.as_int returns i64; Usage.input_tokens is u32 — cast.
    # (Match the a2r-fully-qualified form a2r_std::json::as_int(&a2r_std::json::get(...)).)
    sed -i 's|input_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "prompt_tokens"))|input_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "prompt_tokens")) as u32|' "$RUST/openai.rs"
    sed -i 's|output_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "completion_tokens"))|output_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "completion_tokens")) as u32|' "$RUST/openai.rs"
    # tool_calls list binding: raw_tool_calls.as_array() is serde_json's
    # (Option<&Vec>); use a2r-std json::as_array (→ owned Vec).
    sed -i 's|parse_openai_tool_calls(raw_tool_calls.as_array().clone())|parse_openai_tool_calls(a2r_std::json::as_array(\&raw_tool_calls))|' "$RUST/openai.rs"
    sed -i 's|parse_openai_tool_calls(raw_tool_calls.as_array())|parse_openai_tool_calls(a2r_std::json::as_array(\&raw_tool_calls))|' "$RUST/openai.rs"
    # tool_to_openai(&t): t is already &ToolDefinition (iterating &req.tools),
    # &t is &&ToolDefinition. tool_to_openai now takes &ToolDefinition — pass t.
    sed -i 's|tool_to_openai(&t)|tool_to_openai(t)|g' "$RUST/openai.rs"
    # complete_stream delegates to an async glue fn — the return needs .await.
    sed -i 's|return crate::provider_glue::openai_complete_stream(self, req, on_delta, cancel);|return crate::provider_glue::openai_complete_stream(self, req, on_delta, cancel).await;|' "$RUST/openai.rs"
    # from_upstream_status takes reqwest::StatusCode; resp.status_code() is u32.
    sed -i 's|from_upstream_status(status, \&text)|from_upstream_status(reqwest::StatusCode::from_u16(status as u16).unwrap_or(reqwest::StatusCode::BAD_GATEWAY), \&text)|g' "$RUST/openai.rs"
    # tool_to_openai takes owned ToolDefinition; the for-loop yields &ToolDefinition.
    # Match the a2r-emitted form `t: ToolDefinition` (with colon).
    sed -i 's|fn tool_to_openai(t: ToolDefinition)|fn tool_to_openai(t: \&ToolDefinition)|' "$RUST/format.rs"
    # t is &ToolDefinition (for-in &req.tools); pass it directly (no extra &).
    sed -i 's|tool_to_openai(&t)|tool_to_openai(t)|g' "$RUST/openai.rs"
    # tool_to_openai body: t is now &ToolDefinition — clone the borrowed fields.
    sed -i 's|Value::String(t.name)|Value::String(t.name.clone())|g' "$RUST/format.rs"
    sed -i 's|Value::String(t.description)|Value::String(t.description.clone())|g' "$RUST/format.rs"
    sed -i 's|func.insert("parameters".to_string(), t.parameters)|func.insert("parameters".to_string(), t.parameters.clone())|' "$RUST/format.rs"
    # header() takes &str; self.api_key is String — borrow it (& the f-string
    # Bearer interpolation already produces a String, but a2r wraps it; pass &).
    sed -i 's|.header("Authorization", format!("Bearer {}", self.api_key))|.header("Authorization", \&format!("Bearer {}", self.api_key))|g' "$RUST/openai.rs"
    # build_body message loop: m is &Message (iterating &req.messages); openai_
    # content takes (&str, Vec<ContentBlock>) — borrow role, clone content.
    sed -i 's|openai_content(m.role, m.content)|openai_content(m.role.as_str(), m.content.clone())|g' "$RUST/openai.rs"
    # `let model = match model_field.is_empty() { true => req.model, ... }` — req
    # is &CompletionRequest, req.model can't move. Clone it.
    sed -i 's|true => req.model,|true => req.model.clone(),|g' "$RUST/openai.rs"
fi

# ── anthropic.rs (Phase 4) ──────────────────────────────────────────────────
if [ -f "$RUST/anthropic.rs" ]; then
    fix_provider_impl "$RUST/anthropic.rs"
    sed -i 's|use a2r_std::sse::{SseParser};|use crate::sse::SseParser;|' "$RUST/anthropic.rs"
    sed -i 's|use a2r_std::sse::SseParser;|use crate::sse::SseParser;|' "$RUST/anthropic.rs"
    sed -i 's|provider_glue\.anthropic_complete_stream|crate::provider_glue::anthropic_complete_stream|' "$RUST/anthropic.rs"
    sed -i 's|Value::Number(n)|Value::Number(serde_json::Number::from(n))|g' "$RUST/anthropic.rs"
    sed -i 's|Value::Number(t)|Value::Number(serde_json::Number::from(t))|g' "$RUST/anthropic.rs"
    sed -i 's|Value::Number(serde_json::Number::from(t))|Value::Number(serde_json::Number::from_f64(t).unwrap_or(serde_json::Number::from(0)))|g' "$RUST/anthropic.rs"
    sed -i 's|Value::Number(4096)|Value::Number(serde_json::Number::from(4096))|g' "$RUST/anthropic.rs"
    sed -i 's|from_upstream_status(status, text)|from_upstream_status(status, \&text)|g' "$RUST/anthropic.rs"
    # Usage tokens i64 → u32.
    sed -i 's|input_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "input_tokens"))|input_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "input_tokens")) as u32|' "$RUST/anthropic.rs"
    sed -i 's|output_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "output_tokens"))|output_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "output_tokens")) as u32|' "$RUST/anthropic.rs"
    # content blocks: `blocks.as_array()` resolves to serde_json::Value::as_array
    # (→ Option<&Vec>), not a2r-std's json::as_array (→ Vec owned). Use the
    # a2r-std fn so for-in yields owned Value elements; &b is then &Value.
    sed -i 's|for b in blocks.as_array() {|for b in a2r_std::json::as_array(\&blocks) {|' "$RUST/anthropic.rs"
    # content_blocks_to_anthropic: blocks param is now &Vec; iterate blocks
    # directly (not &blocks — that's &&Vec).
    sed -i 's|for b in &blocks {|for b in blocks {|g' "$RUST/anthropic.rs"
    # Value::String(&str/&String fields) → owned.
    sed -i 's|Value::String(text)|Value::String(text.to_string())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(id)|Value::String(id.to_string())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(tc_name)|Value::String(tc_name.to_string())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(tool_use_id)|Value::String(tool_use_id.to_string())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(content)|Value::String(content.to_string())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(name)|Value::String(name.to_string())|g' "$RUST/anthropic.rs"
    # Value::Bool(&bool) → deref.
    sed -i 's|Value::Bool(is_error)|Value::Bool(*is_error)|g' "$RUST/anthropic.rs"
    # tool_to_anthropic takes &ToolDefinition; the for-loop yields &ToolDefinition
    # (a2r borrows), so pass t directly (already a ref). The .at call is
    # tool_to_anthropic(t) where t is &ToolDefinition — but the fn takes owned.
    # Make tool_to_anthropic take a reference instead.
    sed -i 's|fn tool_to_anthropic(t ToolDefinition)|fn tool_to_anthropic(t: \&ToolDefinition)|' "$RUST/anthropic.rs"
    # complete_stream + StatusCode (same fixes as openai).
    sed -i 's|return crate::provider_glue::anthropic_complete_stream(self, req, on_delta, cancel);|return crate::provider_glue::anthropic_complete_stream(self, req, on_delta, cancel).await;|' "$RUST/anthropic.rs"
    sed -i 's|from_upstream_status(status, \&text)|from_upstream_status(reqwest::StatusCode::from_u16(status as u16).unwrap_or(reqwest::StatusCode::BAD_GATEWAY), \&text)|g' "$RUST/anthropic.rs"
    # header() &str + build_body message loop (m is &Message).
    sed -i 's|.header("x-api-key", self.api_key)|.header("x-api-key", \&self.api_key)|g' "$RUST/anthropic.rs"
    sed -i 's|obj.insert("role".to_string(), Value.String(m.role))|obj.insert("role".to_string(), Value::String(m.role.clone()))|g' "$RUST/anthropic.rs"
    # content_blocks_to_anthropic takes &Vec<ContentBlock>; m.content is Vec.
    sed -i 's|content_blocks_to_anthropic(m.content)|content_blocks_to_anthropic(\&m.content)|g' "$RUST/anthropic.rs"
    # And update the fn signature to match (&Vec, not owned Vec).
    sed -i 's|fn content_blocks_to_anthropic(blocks: Vec<ContentBlock>)|fn content_blocks_to_anthropic(blocks: \&Vec<ContentBlock>)|' "$RUST/anthropic.rs"
    # tool_to_anthropic: same as openai's tool_to_openai — take &ToolDefinition,
    # pass t directly (for-in yields &ToolDefinition).
    sed -i 's|fn tool_to_anthropic(t: ToolDefinition)|fn tool_to_anthropic(t: \&ToolDefinition)|' "$RUST/anthropic.rs"
    sed -i 's|tool_to_anthropic(&t)|tool_to_anthropic(t)|g' "$RUST/anthropic.rs"
    # a2r may auto-clone the loop var (for t in req.tools → tool_to_anthropic(t.clone()));
    # tool_to_anthropic now takes &ToolDefinition — drop the clone.
    sed -i 's|tool_to_anthropic(t.clone())|tool_to_anthropic(t)|g' "$RUST/anthropic.rs"
    sed -i 's|tool_to_openai(t.clone())|tool_to_openai(t)|g' "$RUST/openai.rs"
    # tool_to_anthropic body: t is now &ToolDefinition — clone borrowed fields.
    sed -i 's|Value::String(t.name)|Value::String(t.name.clone())|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(t.description)|Value::String(t.description.clone())|g' "$RUST/anthropic.rs"
    sed -i 's|obj.insert("input_schema".to_string(), t.parameters)|obj.insert("input_schema".to_string(), t.parameters.clone())|' "$RUST/anthropic.rs"
    # req.model + m.role can't move out of &req/&Message — clone.
    sed -i 's|true => req.model,|true => req.model.clone(),|g' "$RUST/anthropic.rs"
    sed -i 's|Value::String(m.role)|Value::String(m.role.clone())|g' "$RUST/anthropic.rs"
fi

# ── ollama.rs (Phase 4) ─────────────────────────────────────────────────────
# Plan 028: cache-dimension parses need the same as u32 widening as the
# input/output tokens above, and the quota classifier's status compare needs a
# deref (match bindings are references).
if [ -f "$RUST/anthropic.rs" ]; then
    sed -i 's#cache_read_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "cache_read_input_tokens"))#cache_read_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "cache_read_input_tokens")) as u32#;
            s#cache_write_tokens: a2r_std::json::as_int(&a2r_std::json::get(&u, "cache_creation_input_tokens"))#cache_write_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&u, "cache_creation_input_tokens")) as u32#' "$RUST/anthropic.rs"
fi
if [ -f "$RUST/openai.rs" ]; then
    sed -i 's#cache_read_tokens: a2r_std::json::as_int(&a2r_std::json::get(&a2r_std::json::get(&u, "prompt_tokens_details"), "cached_tokens")),#cache_read_tokens: a2r_std::json::as_int(\&a2r_std::json::get(\&a2r_std::json::get(\&u, "prompt_tokens_details"), "cached_tokens")) as u32,#' "$RUST/openai.rs"
fi
if [ -f "$RUST/error.rs" ]; then
    sed -i 's#if status == 402 {#if *status == 402 {#' "$RUST/error.rs"
fi

if [ -f "$RUST/ollama.rs" ]; then
    fix_provider_impl "$RUST/ollama.rs"
    # ollama delegates complete/complete_stream to self.inner — pass &req to
    # match the (Phase-3-fixed) complete signature.
    sed -i 's|self.inner.complete(req)|self.inner.complete(\&req)|g' "$RUST/ollama.rs"
    sed -i 's|self.inner.complete_stream(req,|self.inner.complete_stream(\&req,|g' "$RUST/ollama.rs"
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
