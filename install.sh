#!/usr/bin/env bash
# Build (or reuse the prebuilt binary) and install hyprspanso for this user.
# Does NOT start or enable the daemon, so it cannot fight a running espanso.
set -euo pipefail

cd "$(dirname "$0")"

BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"

# A source checkout builds from Cargo.toml; the release tarball already
# contains the compiled binary next to this script.
if [[ -f Cargo.toml ]]; then
  cargo build --release
  BIN=target/release/hyprspanso
elif [[ -x ./hyprspanso ]]; then
  BIN=./hyprspanso
else
  echo "install.sh: no Cargo.toml and no ./hyprspanso binary" >&2
  exit 1
fi

install -Dm755 "$BIN" "$BIN_DIR/hyprspanso"
install -Dm644 contrib/hyprspanso.service "$UNIT_DIR/hyprspanso.service"

systemctl --user daemon-reload 2>/dev/null || true

# One starter file with three examples, only when no matches exist yet.
"$BIN_DIR/hyprspanso" init || true

cat <<EOF

Installed:
  $BIN_DIR/hyprspanso
  $UNIT_DIR/hyprspanso.service

Next steps:
  1. Make sure your user is in the 'input' group:
       sudo usermod -aG input "\$USER"   # then log out and back in
  2. Add the keybinding (see README.md):
       hl.unbind("CTRL + SPACE")
       o.bind("CTRL + SPACE", "Search snippets", "\$HOME/.local/bin/hyprspanso launcher")
  3. Stop espanso, then enable hyprspanso:
       systemctl --user disable --now espanso
       systemctl --user enable --now hyprspanso
  4. Optional, on Omarchy: ./contrib/omarchy/install-bar-widget.sh
  5. Diagnostics:
       hyprspanso check
       journalctl --user -u hyprspanso -f
EOF
