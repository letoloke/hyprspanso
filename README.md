# hyprspanso

A tiny, espanso-compatible text expander built specifically for **Hyprland / Wayland**.
It reads your existing espanso match files (read-only) and only implements the
`trigger` → `replace` feature, plus a fuzzy search window.

```
/dev/input/event*  ──evdev──▶  xkbcommon (Hyprland keymap)  ──▶  trigger buffer
                                                                    │
                                       wtype (zwp_virtual_keyboard) ◀┘
```

* **Capture**: evdev (`/dev/input`) — no XWayland, no rdev, no grabbing.
  Devices are only observed, so your keyboard keeps working.
* **Decode**: the compositor's own keymap (`xkbcli dump-keymap-wayland`), so
  `us`/`latam` groups, shift, altgr and caps lock behave exactly like Hyprland.
* **Inject**: `wtype` over `zwp_virtual_keyboard_v1` — never uinput, so
  injected text cannot feed back into the capture loop.
* **Layout changes**: followed via Hyprland's `.socket2.sock` (`activelayout`),
  the buffer is cleared on window/workspace changes (`activewindow`).

## What is supported

| espanso YAML        | hyprspanso |
|---------------------|------------|
| `trigger`           | ✅ |
| `triggers: [...]`   | ✅ |
| `replace`           | ✅ (multi-line, unicode) |
| `word: true`        | ✅ |
| `propagate_case`    | ✅ (simple: first letter / ALL CAPS) |
| `force_clipboard`   | ✅ (via `wl-copy` + paste, clipboard restored) |
| `vars`, `form`, `type: markdown/image/html`, `script` | ❌ skipped and reported |

Skipped matches are never silently dropped: `hyprspanso check` lists them and
the daemon logs a summary at startup.

## Requirements

* Hyprland (uses its event socket and keymap)
* `wtype`, `wl-clipboard` (`wl-copy`/`wl-paste`), `libxkbcommon` (`xkbcli`)
* Your user in the `input` group:

```bash
sudo usermod -aG input "$USER"   # then log out and back in
```

## Install without building

Prebuilt x86_64 binary (Arch/Omarchy, glibc):

```bash
curl -LO https://github.com/letoloke/hyprspanso/releases/latest/download/hyprspanso-x86_64-linux.tar.gz
tar xf hyprspanso-x86_64-linux.tar.gz
./install.sh
```

## Build & install

```bash
cargo build --release
./install.sh
```

`install.sh` installs the binary to `~/.local/bin/hyprspanso` and the systemd
user unit, but **does not start it** (espanso may still be running).

Enable it:

```bash
systemctl --user daemon-reload
systemctl --user enable --now hyprspanso
```

Stop espanso first or you get double expansions:

```bash
systemctl --user disable --now espanso
```

Rollback is the same in reverse (`systemctl --user disable --now hyprspanso`,
`systemctl --user enable --now espanso`).

## Hyprland integration

Add to `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + ALT + E", "Search snippets", "$HOME/.local/bin/hyprspanso launcher")
```

Add to `~/.config/hypr/hyprland.lua` (or any loaded config file):

```lua
-- hyprspanso: floating snippet search window
o.window("^org\\.omarchy\\.hyprspanso$", {
  float = true, size = "1000 620", center = true,
  rounding = 6, border_size = 2,
})
```

`hyprspanso launcher` opens the search TUI through `omarchy-launch-tui` (styled
floating terminal) and remembers the window you came from, so the insertion
lands back where you were.

## CLI

```
hyprspanso daemon            run the expander
hyprspanso check             validate match files, list skipped matches
hyprspanso list [filter]     print triggers
hyprspanso search            fuzzy search TUI (in the current terminal)
hyprspanso launcher          search TUI in a floating terminal
hyprspanso reload            reload match files (or: kill -USR2)
hyprspanso toggle            pause/resume expansion (or: kill -USR1)
hyprspanso status            daemon state
```

Config dir defaults to `$XDG_CONFIG_HOME/espanso`; override with
`HYPRSPANSO_CONFIG=/path/to/espanso`. The socket is
`$XDG_RUNTIME_DIR/hyprspanso.sock` (`HYPRSPANSO_SOCKET` to override).

Match files are re-read automatically when they change (2 s poll, works on
synced folders like Insync/Google Drive). A half-written or invalid file keeps
the previous configuration instead of crashing the daemon.

## Search window

* type to fuzzy-filter (matches trigger **and** replacement text)
* `↑`/`↓` or `Ctrl-N`/`Ctrl-P` to move, `Enter` to insert, `Esc` to cancel
* `Ctrl-U` clear query, `Ctrl-W` delete word, `F5` reload matches
* expansion is paused while the window is open, so typing in it never triggers
  a snippet

## Security

By design this is a keylogger: it reads every keystroke from `/dev/input` to
detect triggers. The buffer is memory-only (64 characters), never written to
disk, never sent anywhere. The binary has no network code. Keep that in mind
before running it on a shared machine.

## Known limitations

* Dead keys (accent composition) are not followed, because libxkbcommon's
  per-key text conversion does not run the compose state machine. Triggers with
  accents typed via compose sequences will not match; triggers typed with a
  direct keycap (e.g. `ñ` on a Latin American layout) work.
* Switching keyboard layout resets the trigger buffer.
* `vars`/`form` matches are intentionally out of scope.
* Apps that ignore `zwp_virtual_keyboard_v1` (some XWayland/Electron windows)
  will not receive injected text; use the clipboard fallback there
  (`force_clipboard: true`), or an X11 tool under XWayland.

## Status

v0.1, developed and tested against Hyprland 0.56 on Omarchy/Arch. The Hyprland
event names and dispatcher syntax are version-sensitive: 0.56 needs the Lua
dispatcher form for focusing windows, older versions use `focuswindow`. Both are
handled, but please open an issue if a Hyprland upgrade breaks it.

## License

MIT — see [LICENSE](LICENSE).

