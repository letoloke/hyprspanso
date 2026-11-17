#!/usr/bin/env bash
# Build and install hyprspanso for the current user.
# Does NOT start or enable the daemon (so it cannot fight a running espanso).
set -euo pipefail

cd "$(dirname "$0")"

BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"

cargo build --release

install -Dm755 target/release/hyprspanso "$BIN_DIR/hyprspanso"
install -Dm644 contrib/hyprspanso.service "$UNIT_DIR/hyprspanso.service"

systemctl --user daemon-reload 2>/dev/null || true

cat <<EOF

Installed:
  $BIN_DIR/hyprspanso
  $UNIT_DIR/hyprspanso.service

Next steps:
  1. Make sure your user is in the 'input' group:
       sudo usermod -aG input "\$USER"   # then log out and back in
  2. Add the keybinding (see README.md):
       o.bind("SUPER + ALT + E", "Search snippets", "\$HOME/.local/bin/hyprspanso launcher")
  3. Stop espanso, then enable hyprspanso:
       systemctl --user disable --now espanso
       systemctl --user enable --now hyprspanso
  4. Diagnostics:
       hyprspanso check
       journalctl --user -u hyprspanso -f
EOF
