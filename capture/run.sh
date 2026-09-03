#!/usr/bin/env bash
# Launches the game with RenderDoc armed. Touch $CAPTURE_WHEN to take a frame.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
out=${OUT:-$HOME/rdcaps}
sentinel=${CAPTURE_WHEN:-$out/take}

if [ -x /usr/bin/renderdoccmd ]; then
    renderdoc=/usr
    header=/usr/include/renderdoc_app.h
else
    renderdoc=${RENDERDOC:-$HOME/.local/opt/renderdoc}
    header=$renderdoc/include/renderdoc_app.h
fi

# The game's vulkan goes through winevulkan to the host loader, which LD_PRELOAD does not reach.
# Only a registered layer gets renderdoc into that path, and the instance is made at startup, so a
# missing layer means a whole session captures nothing. Two of them is worse than none: each names
# its own librenderdoc.so and both would load.
mine=$HOME/.local/share/vulkan/implicit_layer.d/renderdoc_capture.json
packaged=$(ls /etc/vulkan/implicit_layer.d/renderdoc_capture.json \
              /usr/share/vulkan/implicit_layer.d/renderdoc_capture.json 2>/dev/null | head -1 || true)
if [ -n "$packaged" ]; then
    rm -f "$mine"
else
    mkdir -p "$(dirname "$mine")"
    sed "s#/io/dist/lib/librenderdoc.so#$renderdoc/lib/librenderdoc.so#" \
        "$renderdoc/etc/vulkan/implicit_layer.d/renderdoc_capture.json" > "$mine"
fi

mkdir -p "$out"
cc -shared -fPIC -O1 -I"$(dirname "$header")" "$here/trigger.c" \
    -o "$here/libtrigger.so" -ldl -lpthread

# /tmp is tmpfs and a frame of this game is gigabytes, so captures go under $HOME.
# RenderDoc 1.45 speaks xlib and xcb and not wayland, so the launcher's SDL window has to be
# XWayland or it never appears; wine is already on x11.
LD_PRELOAD=$here/libtrigger.so \
CAPTURE_WHEN=$sentinel \
ENABLE_VULKAN_RENDERDOC_CAPTURE=1 \
SDL_VIDEODRIVER=x11 \
DXVK_CONFIG='dxvk.enableGraphicsPipelineLibrary = False' \
    exec "$renderdoc/bin/renderdoccmd" capture --capture-file "$out/ffxiv" \
        --opt-hook-children \
        /usr/bin/XIVLauncher.Core "$@"
