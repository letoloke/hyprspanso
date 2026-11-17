#!/usr/bin/env bash
# Install the hyprspanso status widget into the Omarchy bar.
#
# The widget lives in ~/.config/omarchy/plugins/ (user-owned), so Omarchy
# updates never touch it. Re-run this script after pulling a new version.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
plugin_id="letoloke.hyprspanso"
dest="$HOME/.config/omarchy/plugins/$plugin_id"

mkdir -p "$dest"
install -Dm644 "$here/letoloke.hyprspanso/manifest.json" "$dest/manifest.json"
install -Dm644 "$here/letoloke.hyprspanso/BarWidget.qml" "$dest/BarWidget.qml"

# Put it next to the clock, then reload the shell so the new plugin is mounted.
omarchy bar put "$plugin_id" --after omarchy.system-update >/dev/null 2>&1 || true
omarchy restart shell >/dev/null 2>&1 || true

cat <<'EOF'
hyprspanso bar widget installed.

  solid keyboard  - daemon running and expanding
  faded keyboard  - paused (right click to resume) or daemon not running
  left click      - open the snippet search
  right click     - pause / resume expansion
  middle click    - reload match files

If the icon does not appear, run: omarchy restart shell
EOF
