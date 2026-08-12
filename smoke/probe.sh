#!/usr/bin/env bash
# Runs smoke/probe.ts with chromium and bun resolved the way smoke/run.sh resolves them.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
need() {
    local bin="$1" attr="$2" out
    if command -v "$bin" >/dev/null 2>&1; then
        command -v "$bin"
        return
    fi
    out="$(nix build --no-link --print-out-paths "nixpkgs#$attr")"
    echo "$out/bin/$bin"
}
# Serves viewer/dist as it stands, so a probe of unbuilt source would capture the last build.
[ "${1:-}" = "--no-build" ] && shift || (cd "$root/viewer" && trunk build index.html >/dev/null)
CHROMIUM="${CHROMIUM:-$(need chromium chromium)}"
BUN="${BUN:-$(need bun bun)}"
export CHROMIUM
exec "$BUN" "$root/smoke/probe.ts" "$@"
