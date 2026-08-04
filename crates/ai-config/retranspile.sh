#!/usr/bin/env bash
# re-transpile ALL .at → a2r → assemble into rust/src/
#
# Flat layout (no sub-directory hoisting):
#   src/X.at  → rust/src/X.rs
#   src/lib.at → rust/src/lib.rs  (crate root + injected pub mod decls)
#
# Note: ai-config uses `use.rust auto_atom` / `use.rust auto_val` which a2r
# maps to direct external-crate `use` statements — no extern shims needed
# (unlike auto-ai-agent/auto-ai-client which reference sibling crates via
# `use ai_config:` Auto-syntax that a2r renders as `use crate::ai_config::`).
#
# Usage: ./retranspile.sh [check]
set -euo pipefail

# The Auto→Rust transpiler. Assumed on PATH (cargo install from auto-lang, or
# built locally). Override with $AUTO.
AUTO="${AUTO:-auto}"
CRATE_DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$CRATE_DIR/src"
RUST="$CRATE_DIR/rust/src"

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

# wire.rs uses `JsonValue` (a short alias a2r can't express — Auto has no
# `type X = Y` alias and rejects `serde_json::Value as JsonValue` / full-path
# enum field types). Inject the alias as a use-statement at the top of wire.rs,
# mirroring rust-ref's `use serde_json::Value as JsonValue;`.
if [ -f "$RUST/wire.rs" ]; then
    sed -i '1a use serde_json::Value as JsonValue;' "$RUST/wire.rs"
    echo "  [wire] injected JsonValue alias"
fi

# Assemble lib.rs: transpiled crate-root + pub mod decls (no extern shims).
if [ -f "$SRC/lib.a2r.rs" ]; then
    awk -v pubmods="$(read_pub_mods)" '
        /^#!\[allow/ { print; print ""; print pubmods; next }
        { print }
    ' "$SRC/lib.a2r.rs" > "$RUST/lib.rs"
    echo "  [lib] assembled lib.rs (crate-root transpile + pub mod decls)"
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
