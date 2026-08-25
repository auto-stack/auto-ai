#!/usr/bin/env bash
# re-transpile ALL .at → a2r → assemble into rust/src/
#
# Simplified flat layout (no sub-directory hoisting, unlike auto-ai-agent):
#   src/X.at  → rust/src/X.rs
#   src/lib.at → rust/src/lib.rs  (crate root + injected shims + pub mod decls)
#
# Usage: ./retranspile.sh [check]
set -euo pipefail

# The Auto→Rust transpiler. Assumed on PATH (cargo install from auto-lang, or
# built locally). Override with $AUTO.
AUTO="${AUTO:-auto}"
CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$CRATE_DIR/src"
RUST="$CRATE_DIR/rust/src"

# ── extern-crate shims (a2r emits `use crate::<these>::...`) ──
read_shims() {
    cat <<'SHIMS'
// ── extern-crate shims (a2r emits `use crate::<these>::...`) ────────────────
pub mod ai_config {
    pub use ::ai_config::*;
}
// JsonValue alias (a2r can't express `serde_json::Value as JsonValue`; the .at
// sources use the short name JsonValue which resolves here via ai_config).
pub type JsonValue = serde_json::Value;

SHIMS
}

# ── pub mod declarations for every module file in rust/src/ ──
read_pub_mods() {
    for f in "$RUST"/*.rs; do
        local stem
        stem=$(basename "$f" .rs)
        case "$stem" in
            lib) continue ;;
        esac
        echo "pub mod ${stem};"
    done
}

echo "[retranspile] transpiling all .at files..."
transpile_one() {
    local f="$1"
    local base
    base=$(basename "$f" .at)
    local crate_root=0
    if [ "$base" = "lib" ]; then
        crate_root=1
    fi
    A2R_CRATE_ROOT="$crate_root" "$AUTO" trans --path "$f" rust >/dev/null 2>&1 || true
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
    [ "$bn" = "lib" ] && continue
    copy_if_exists "$SRC/${bn}.a2r.rs" "$RUST/${bn}.rs"
done

# Assemble lib.rs: transpiled crate-root + injected shims + pub mod decls.
if [ -f "$SRC/lib.a2r.rs" ]; then
    awk -v shims="$(read_shims)" -v pubmods="$(read_pub_mods)" '
        /^#!\[allow/ { print; print ""; print shims; print pubmods; next }
        { print }
    ' "$SRC/lib.a2r.rs" > "$RUST/lib.rs"
    # Remove `pub use crate::daemon;` — it conflicts with the injected
    # `pub mod daemon;` (E0255 name defined multiple times). The pub mod is
    # sufficient to export the daemon module.
    sed -i '/^pub use crate::daemon;$/d' "$RUST/lib.rs"
    echo "  [lib] assembled lib.rs (crate-root transpile + shims + pub mod decls)"
else
    echo "  [skip] lib.at failed to transpile — keeping existing lib.rs"
fi

find "$SRC" -name "*.a2r.rs" -delete

# Plan 020: a2r borrow/type-inference workarounds. Mechanical post-fixes for
# 5 classes of a2r defects, each targeting a specific pattern in lib.rs. They
# become no-ops once the a2r root causes are fixed.
if [ -f "$RUST/lib.rs" ]; then
    # E0658: redundant .as_str() on a value already typed &str (Plan 019 E-class).
    sed -i 's#\.unwrap_or_default()\.as_str() {#.unwrap_or_default() {#g' "$RUST/lib.rs"
    # E0596: `let stream` needs `mut` — HTTPStream::next takes &mut self (a2r mut inference).
    # (graduated 2026-08-25, Plan 032 G2.2: `next` added to a2r mutating-method lists — `let mut stream` natively emitted.)
    # E0308: `&tc` is a spurious extra borrow — tc is already &Value from `for tc in &arr`
    #         (a2r borrow + element-type inference defect, Plan 019 C/B-class variant).
    #         Also fix `get(...).as_array()`: a2r emits a method call (serde_json's
    #         Value::as_array -> Option<&Vec>) instead of the a2r_std free function
    #         (as_array(&Value) -> Vec<Value>), so unwrap the Option to get Vec<Value>.
    sed -i 's#get_str(&tc,#get_str(tc,#g; s#get(&tc,#get(tc,#g' "$RUST/lib.rs"
    sed -i 's#\.as_array();#.as_array().cloned().unwrap_or_default();#g' "$RUST/lib.rs"
    # E0308: as_int returns i64 but Usage fields are u32 (a2r narrowing-conversion gap).
    # Plan 028: cache dimensions need the same u32 widening.
    # E0507: str_find(self.buf, ...) moves the owned field — borrow instead (Plan 019 D-class).
    sed -i 's#a2r_std::str_find(self\.buf,#a2r_std::str_find(\&self.buf,#g' "$RUST/lib.rs"
fi

echo "[retranspile] assembly complete."

if [ "${1:-}" = "check" ]; then
    echo "[retranspile] running cargo check..."
    cd "$RUST/.."
    n=$(cargo check --color never 2>&1 | grep -cE "^error" || true)
    echo "[retranspile] error count: $n"
fi
