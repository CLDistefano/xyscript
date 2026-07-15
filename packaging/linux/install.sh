#!/bin/sh
# XyScript desktop integration for Linux (per-user install into ~/.local).
#
# Installs the .desktop launcher and the hicolor icon set so XyScript appears in the
# application menu / dock with its icon, on both X11 and Wayland. Also installs the
# binary to ~/.local/bin/xyscript if one is found (or pass its path as the first arg).
#
#   ./install.sh [path/to/xyscript-binary]
#
# The window's own titlebar/taskbar icon is set at runtime by the app (winit) on X11;
# on Wayland the compositor takes the icon from this .desktop file, matched to the
# window via StartupWMClass=xyscript (the app sets its Wayland app-id to "xyscript").

set -eu
here=$(CDPATH= cd "$(dirname "$0")" && pwd)
prefix="${HOME}/.local"

# Locate the binary: explicit arg, then a copy beside this script, then a release build.
bin="${1:-}"
if [ -z "$bin" ]; then
  for c in \
    "$here/xyscript" \
    "$here/../../xyscript-gui/target/release/xyscript-gui" \
    "$here/../../target/release/xyscript-gui"; do
    if [ -x "$c" ]; then bin="$c"; break; fi
  done
fi
if [ -n "$bin" ] && [ -x "$bin" ]; then
  install -Dm755 "$bin" "$prefix/bin/xyscript"
  echo "installed binary  -> $prefix/bin/xyscript"
else
  echo "note: no binary found — make sure 'xyscript' is on your PATH (the .desktop uses Exec=xyscript)"
fi

install -Dm644 "$here/xyscript.desktop" "$prefix/share/applications/xyscript.desktop"
echo "installed launcher -> $prefix/share/applications/xyscript.desktop"

for s in 16 24 32 48 64 128 256 512; do
  install -Dm644 "$here/icons/xyscript-$s.png" \
    "$prefix/share/icons/hicolor/${s}x${s}/apps/xyscript.png"
done
echo "installed icons    -> $prefix/share/icons/hicolor/*/apps/xyscript.png"

# Refresh caches (best-effort; harmless if the tools are absent).
update-desktop-database "$prefix/share/applications" 2>/dev/null || true
gtk-update-icon-cache -f "$prefix/share/icons/hicolor" 2>/dev/null || true

echo "done. XyScript should show in your app menu; if the icon lags, log out and back in."
