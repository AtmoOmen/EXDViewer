#!/usr/bin/env bash
# Builds the wasm app, serves it, and drives a real headless browser over it, failing on any GL
# error, panic or ERROR-level log. See smoke/README.md.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build=1
for arg in "$@"; do
    [ "$arg" = "--no-build" ] && build=0
done

# nix is how a browser and a script runtime are obtained here; both come out of the binary cache.
need() {
    local bin="$1" attr="$2"
    if command -v "$bin" >/dev/null 2>&1; then
        command -v "$bin"
        return
    fi
    local out
    out="$(nix build --no-link --print-out-paths "nixpkgs#$attr" 2>/dev/null)" || {
        echo "cannot obtain $bin: install it or make nixpkgs#$attr buildable" >&2
        exit 2
    }
    echo "$out/bin/$bin"
}

CHROMIUM="${CHROMIUM:-$(need chromium chromium)}"
BUN="${BUN:-$(need bun bun)}"
export CHROMIUM

# A dev build's own decode times run 10-25x slower than release's, which is what starves a
# streaming zone under load; the "smoke" cargo profile (root Cargo.toml) is release everywhere
# except egui_glow, which keeps its own check_for_gl_error active, and keeps overflow-checks on so
# a malformed or oversized file still panics instead of wrapping a usize on the 32-bit wasm target.
if [ "$build" = 1 ]; then
    echo "== building viewer/dist"
    (cd "$root/viewer" && trunk build index.html --release --cargo-profile smoke)
fi

exec "$BUN" "$root/smoke/smoke.ts" "$@"
