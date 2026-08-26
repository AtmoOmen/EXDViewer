#!/usr/bin/env bash
# Builds the native app and drives it offscreen against the local sqpack install, failing on any
# panic or ERROR-level log. Faster than smoke/run.sh because there is no wasm build, no browser and
# no network, but it does not cover what only a wasm-in-a-real-canvas run can: see the "native vs
# browser" section of smoke/README.md before trusting a green run here over a red smoke/run.sh.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sqpack="${EXDVIEWER_SQPACK:-$HOME/.xlcore/ffxiv/game/sqpack}"
out="$root/smoke/native-shots"
build=1
for arg in "$@"; do
    [ "$arg" = "--no-build" ] && build=0
done

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

XVFB="${XVFB:-$(need Xvfb xvfb)}"

if [ ! -d "$sqpack" ]; then
    echo "no sqpack install at $sqpack (set EXDVIEWER_SQPACK)" >&2
    exit 2
fi

if [ "$build" = 1 ]; then
    echo "== building viewer (dev profile, debug_assertions on for check_for_gl_error)"
    (cd "$root" && cargo build -q -p viewer --bin viewer)
fi

bin="${CARGO_TARGET_DIR:-$root/target}/debug/viewer"

rm -rf "$out"
mkdir -p "$out"

# -displayfd picks a free display rather than guessing one, so concurrent runs do not collide.
displayfile="$(mktemp)"
"$XVFB" -displayfd 3 3>"$displayfile" -screen 0 1600x1000x24 &
xvfb_pid=$!
trap 'kill "$xvfb_pid" 2>/dev/null; rm -f "$displayfile"' EXIT
for _ in $(seq 1 50); do
    [ -s "$displayfile" ] && break
    sleep 0.1
done
display=":$(cat "$displayfile")"

steps=(
    "model:bg/ex1/01_roc_r2/dun/r2d1/bgparts/r2d1_u1_yam04.mdl"
    "scene:bg/ex1/01_roc_r2/dun/r2d1/level/bg.lgb"
    "scene:bg/ex1/01_roc_r2/dun/r2d1/level/r2d1.lvb"
)

DISPLAY="$display" "$bin" --smoke "$sqpack" "$out" "${steps[@]}"
status=$?

kill "$xvfb_pid" 2>/dev/null
trap - EXIT

exit "$status"
