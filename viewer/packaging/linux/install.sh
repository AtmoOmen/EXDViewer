#!/usr/bin/env bash
# Installs the XIViewer .desktop entry and hicolor icon for the current user, so Wayland
# compositors (which have no way for a client to set its own icon) can find one by matching
# the running window's app_id against this desktop file's id, and so launchers/file managers
# list the app with the real icon instead of nothing.
set -euo pipefail

bin=${1:?"usage: install.sh /path/to/viewer-binary"}
bin=$(readlink -f "$bin")
[ -x "$bin" ] || { echo "not an executable: $bin" >&2; exit 1; }

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
assets="$here/../../assets"

data_home=${XDG_DATA_HOME:-$HOME/.local/share}
apps="$data_home/applications"
scalable="$data_home/icons/hicolor/scalable/apps"
size512="$data_home/icons/hicolor/512x512/apps"

mkdir -p "$apps" "$scalable" "$size512"

sed "s|@BIN@|$bin|" "$here/XIViewer.desktop" >"$apps/XIViewer.desktop"
cp "$assets/icon.svg" "$scalable/xiviewer.svg"
cp "$assets/icon.png" "$size512/xiviewer.png"

command -v update-desktop-database >/dev/null && update-desktop-database "$apps" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$data_home/icons/hicolor" || true

echo "Installed $apps/XIViewer.desktop"
