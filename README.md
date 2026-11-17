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

**New here?** [INSTALL.md](INSTALL.md) covers prerequisites, installation,
the `CTRL + SPACE` binding, the Omarchy bar widget and troubleshooting.

## What is supported

| espanso YAML        | hyprspanso |
|---------------------|------------|
| `trigger`           | ✅ |
| `triggers: [...]`   | ✅ |
| `replace`           | ✅ (multi-line, unicode) |
| `word: true`        | ✅ |
| `propagate_case`    | ✅ (simple: first letter / ALL CAPS) |
| `force_clipboard`   | ✅ (via `wl-copy` + paste, clipboard restored) |
| `vars: type: date`  | ✅ (`format`, `offset`) |
| `vars: type: echo`  | ✅ |
| `vars: shell`, `random`, `script`, `form`, `type: markdown/image/html` | ❌ skipped and reported |

### Starter examples

`hyprspanso init` writes **one file with three examples** to
`~/.config/espanso/match/matches.yml` — a plain replacement, a unicode one, and
a dynamic `:date` — but only when the match directory has no `.yml`/`.yaml`
files yet, so it never touches an existing espanso setup:

```bash
hyprspanso init            # fresh setups only
hyprspanso init --force    # overwrite match/matches.yml
```

The same file is in [`examples/matches.yml`](examples/matches.yml) in the repo.
`install.sh` runs `hyprspanso init` for you.

## Dynamic matches

```yaml
- trigger: ":date"
  replace: "{{fecha}} a las {{hora}}"
  vars:
    - name: fecha
      type: date
      params:
        format: "%Y-%m-%d"
    - name: hora
      type: date
      params:
        format: "%H:%M"
        offset: 3600        # seconds, for "in one hour"
```

Templates are rendered when the trigger fires, so identical YAML behaves like
espanso's. The search window previews the rendered value.

Skipped matches are never silently dropped: `hyprspanso check` lists them and
the daemon logs a summary at startup.

## Install

**Full guide: [INSTALL.md](INSTALL.md)** — prerequisites, the `input` group,
keybinding, bar widget and troubleshooting.

```bash
# prebuilt x86_64 binary
curl -LO https://github.com/letoloke/hyprspanso/releases/latest/download/hyprspanso-x86_64-linux.tar.gz
tar xf hyprspanso-x86_64-linux.tar.gz
./install.sh

# or from source
git clone https://github.com/letoloke/hyprspanso && cd hyprspanso && ./install.sh

# once the AUR package is published
yay -S hyprspanso
```

Then:

```bash
sudo usermod -aG input "$USER"           # log out and back in
systemctl --user disable --now espanso   # if espanso is installed
systemctl --user enable --now hyprspanso
```

`install.sh` never starts the daemon by itself, so it cannot fight a running
espanso. Rollback: `systemctl --user disable --now hyprspanso` and re-enable
espanso.

## Hyprland integration

Add to `~/.config/hypr/bindings.lua`:

```lua
-- CTRL+SPACE is bound by Omarchy (non-consuming, terminal prefix). Take it over:
hl.unbind("CTRL + SPACE")
o.bind("CTRL + SPACE", "Search snippets", "$HOME/.local/bin/hyprspanso launcher")
```

If espanso is still installed, set `search_shortcut: OFF` in
`~/.config/espanso/config/default.yml` — otherwise both search windows open.

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

## Omarchy bar widget

`contrib/omarchy/` contains a status widget for the Omarchy (Quickshell) bar:

![hyprspanso bar widget](contrib/omarchy/bar-widget.png)

```bash
./contrib/omarchy/install-bar-widget.sh
```

- solid keyboard icon: daemon running and expanding
- faded icon: paused, or daemon not running
- left click: open the search window
- right click: pause/resume expansion
- middle click: reload match files
- hover: match count and state

It installs into `~/.config/omarchy/plugins/letoloke.hyprspanso/`, which is
user-owned and survives Omarchy updates.

## Search window

* type to fuzzy-filter (matches trigger **and** replacement text)
* `↑`/`↓` or `Ctrl-N`/`Ctrl-P` to move, `Enter` to insert, `Esc` to cancel
* `Ctrl-E` opens the highlighted match's file in `$VISUAL`/`$EDITOR`
  (falls back to `omarchy-launch-editor`, then `xdg-open`); terminal editors are
  suspended and resumed in place, GUI editors open alongside
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

## Troubleshooting

Nothing expands?

1. `hyprspanso status` must say `enabled=true` with a non-zero match count.
2. `hyprspanso check` shows whether a trigger was loaded or skipped (only
   `trigger`/`triggers` + `replace` are supported).
3. Wireless keyboards, USB docks and suspend/resume can re-enumerate a
   keyboard under a recycled `/dev/input/eventN` node. Since 0.2.1 the daemon
   re-opens those automatically; on older builds restart it.
4. Some Electron/webview and XWayland apps ignore the virtual keyboard. Add
   `force_clipboard: true` to that match so hyprspanso pastes instead.
5. Verbose diagnosis: `systemctl --user edit hyprspanso`, add
   `[Service]` + `Environment=HYPRSPANSO_DEBUG=1`, restart, and watch
   `journalctl --user -u hyprspanso -f`. Add `HYPRSPANSO_DEBUG_KEYS=1` to log
   every decoded key.

## Status

v0.1, developed and tested against Hyprland 0.56 on Omarchy/Arch. The Hyprland
event names and dispatcher syntax are version-sensitive: 0.56 needs the Lua
dispatcher form for focusing windows, older versions use `focuswindow`. Both are
handled, but please open an issue if a Hyprland upgrade breaks it.

## License

MIT — see [LICENSE](LICENSE).

