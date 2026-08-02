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
    echo "  [lib] assembled lib.rs (crate-root transpile + shims + pub mod decls)"
else
    echo "  [skip] lib.at failed to transpile — keeping existing lib.rs"
fi

find "$SRC" -name "*.a2r.rs" -delete
echo "[retranspile] assembly complete."

if [ "${1:-}" = "check" ]; then
    echo "[retranspile] running cargo check..."
    cd "$RUST/.."
    n=$(cargo check --color never 2>&1 | grep -cE "^error" || true)
    echo "[retranspile] error count: $n"
fi
