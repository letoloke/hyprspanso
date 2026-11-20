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
where match files live (`~/.config/espanso/match/`), the `CTRL + SPACE`
binding, the Omarchy bar widget and troubleshooting.

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
| `vars: type: choice` | ✅ picker window |
| `vars: type: random` | ✅ random value |
| `form:` + `form_fields:` (`text`, `choice`, `list`) | ✅ interactive window |
| `label`             | ✅ (search + picker) |
| duplicate triggers  | ✅ disambiguation picker |
| `vars: shell`, `script`, `vars: type: form`, `type: markdown/image/html` | ❌ skipped and reported |

### Export to a phone keyboard

Phones have no `/dev/input`, so hyprspanso itself cannot run there — but the
snippets can. LeanType (F-Droid, GPL-3.0) has a text expander that is edited as
plain text, which is exactly what `hyprspanso export` produces:

```bash
hyprspanso export --output leantype.txt        # default format
hyprspanso export --forms clipboard --output leantype.txt
hyprspanso export --format tsv   --output snippets.tsv
hyprspanso export --format json  --output snippets.json
```

Then on the phone: **LeanType → Settings → Text expander → "all shortcuts as
text"**, paste the file's content, apply.

What converts: `trigger`/`replace`, multi-line and unicode text, and
`{{date}}` variables (`%Y-%m-%d` becomes `%date:yyyy-MM-dd%`; literal text and
emoji in the format stay outside the token, so
`%F %H horas %M min ➡️` → `%date:yyyy-MM-dd HH% horas %date:mm% min ➡️`).

Forms have no prompt on a phone keyboard, so their template is exported with
the fields handled by `--forms`:

| `--forms` | Result |
|---|---|
| `placeholder` (default) | `Hola [[name]]!` — replace it by hand after inserting |
| `clipboard` | `Hola %clipboard%!` — copy the value (e.g. the guest's name), then type the trigger |
| `empty` | `Hola !` — the field is removed |

Choice fields use their first value. `%clipboard%` is the closest thing to a
form on Android: copy the name to the clipboard, type `ingresoan1`, and the
message goes out with that name in place.

Duplicate triggers are **all exported**, numbered: `airfm`, `airfm2`, `airfm3`, …
so every variant stays available; the plain name stays the first (and
shortest) one. `choice`/`random` variables are expanded the same way — one
numbered shortcut per value (`rerere`, `rerere2`, …). `--duplicates skip`
keeps only the first of each. `-v` lists every skipped match by name.

Nothing is silently dropped: with the options above every match that has a
static text (or a form template) is exported, and the report on stderr says how
many triggers were collapsed, numbered or expanded. Obsolete matches (`href`,
scripts) never leave the machine. Triggers containing whitespace have
it **removed** (`my trigger` → `mytrigger`, `ytd ` → `ytd`), because phone
keyboards reject spaces in shortcut names; the report counts how many were
adjusted.

### Finding duplicates

```bash
hyprspanso duplicates              # same replacement text, different triggers
hyprspanso duplicates --triggers   # same trigger (the disambiguation groups)
hyprspanso duplicates --exact      # same trigger AND same output (redundant)
```

Each group lists the trigger and the file it came from. Matches with dynamic
templates (`{{date}}`, `vars:` …) are ignored for output comparison because
they render differently every time; `hyprspanso check` prints the group counts.

### Match disambiguation

Several matches may share one trigger. Typing it erases the trigger and opens a
picker with one row per candidate — espanso's behaviour:

```yaml
- trigger: ":quote"
  replace: Every moment is a fresh beginning.
- trigger: ":quote"
  label: Imagine
  replace: Everything you can imagine is real.
```

`↑↓` move, `1-9` pick directly, `Enter` inserts, `Esc` cancels. Rows show the
`label` when present, otherwise the first line of the replacement (or the form
template). Your config's many `airfm` variants work this way.

### Choice variables

`type: choice` opens the picker with the variable's values, then renders the
replacement with the one you picked — the same window as match disambiguation:

```yaml
- trigger: "rerere"
  replace: "{{output}}"
  vars:
    - name: output
      type: choice
      params:
        values:
          - "Huéspedes recomendados, muy amables, limpios y ordenados"
          - "Todo muy bien, los recomiendo ampliamente"
          - "ningún problema, super recomendados"
```

`type: random` takes the same `values:` but picks one automatically at
expansion time (no window). A match with several `choice` variables asks for
them one after another.

### Interactive forms

A `form:` match opens a small window with one field per `[[placeholder]]`,
then inserts the rendered template where you were typing:

```yaml
- trigger: ":hola"
  form: |
    Hola [[name]]! Tu habitación es [[room]].
  form_fields:
    room:
      type: choice
      values: [Búho, Tortuga, Ballena]
```

`Tab`/`↑↓` move between fields, `←→` pick a choice, `Enter` inserts, `Esc`
cancels. The right pane previews the result live. The typed trigger is erased
as soon as the form opens, like espanso.

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

The windows float because of a window rule that `install.sh` adds for you:

```bash
hyprspanso setup-hyprland          # writes ~/.config/hypr/hyprspanso.lua
hyprspanso setup-hyprland --print  # just print the snippet
```

Equivalent rule, if you prefer to add it yourself:

```lua
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
hyprspanso init              choose the match folder (first run) + examples
hyprspanso config            show where matches are read from
hyprspanso config set DIR    point at another folder, saved for next start
hyprspanso check             validate match files, list skipped matches
hyprspanso list [filter]     print triggers
hyprspanso export [--format leantype|tsv|json] [--output FILE]
                             Export snippets for a phone keyboard
hyprspanso duplicates        find matches sharing an output text
hyprspanso duplicates --triggers   find matches sharing a trigger
hyprspanso duplicates --exact      same trigger *and* same output
hyprspanso search            fuzzy search TUI (in the current terminal)
hyprspanso launcher          search TUI in a floating terminal
hyprspanso reload            reload match files (or: kill -USR2)
hyprspanso toggle            pause/resume expansion (or: kill -USR1)
hyprspanso status            daemon state
hyprspanso --version         installed version
hyprspanso diag [--keys]     counters: keys seen, matches, held modifiers
```

On first run, `hyprspanso init` asks for the folder that holds your snippets
(anywhere on disk; it reads `<folder>/match/**`) and saves it in
`~/.config/hyprspanso/config.json` — no symlink needed. Without a saved folder
it falls back to `$XDG_CONFIG_HOME/espanso`. `HYPRSPANSO_CONFIG=/path` still
overrides everything. The socket is `$XDG_RUNTIME_DIR/hyprspanso.sock`
(`HYPRSPANSO_SOCKET` to override).

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

