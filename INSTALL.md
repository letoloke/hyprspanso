# Installing hyprspanso on a new machine

hyprspanso is a single Rust binary plus a systemd **user** unit. It runs
entirely as your user: no root, no setcap, no XWayland.

Tested on Hyprland 0.56 (Arch/Omarchy); the daemon itself only needs Hyprland's
Wayland socket and read access to `/dev/input`.

---

## 1. Prerequisites

```bash
# Arch / Omarchy
sudo pacman -S wtype wl-clipboard libxkbcommon
```

- `wtype` — injects text through the Wayland virtual-keyboard protocol
- `wl-clipboard` — clipboard fallback for long or `force_clipboard: true` matches
- `libxkbcommon` — provides `xkbcli`, used to read Hyprland's keymap

Then give your user read access to the input devices:

```bash
sudo usermod -aG input "$USER"
```

**Log out and back in** (or reboot). Group changes do not apply to a running
session, and without this the daemon starts but never sees a keystroke.

## 2. Install

### Prebuilt binary (x86_64, glibc)

```bash
curl -LO https://github.com/letoloke/hyprspanso/releases/latest/download/hyprspanso-x86_64-linux.tar.gz
tar xf hyprspanso-x86_64-linux.tar.gz
./install.sh
```

Installs `~/.local/bin/hyprspanso` and
`~/.config/systemd/user/hyprspanso.service`, and writes one starter match file
with three examples **only if you have no matches yet**.

### From source

Needs a Rust toolchain (`rustup` or your distro's `rust` package) and
`pkgconf`:

```bash
git clone https://github.com/letoloke/hyprspanso
cd hyprspanso
./install.sh
```

### AUR

Once the package is published:

```bash
yay -S hyprspanso
hyprspanso init        # three starter examples, only when no matches exist
```

## 3. Choose your snippet folder (first run)

hyprspanso can read snippets from **any folder** — it does not have to be
`~/.config/espanso`, and you do not need a symlink. On the first run it asks
where they are:

```bash
hyprspanso init
# hyprspanso reads every .yml/.yaml under <dir>/match/.
# Directory [/home/you/.config/espanso]: ~/Documents/snippets
```

The answer is saved to `~/.config/hyprspanso/config.json`, so the systemd
service (which has no terminal) finds it on every start. Non-interactive
installations can pass it directly:

```bash
hyprspanso init --config-dir ~/Documents/snippets
hyprspanso config set ~/Documents/snippets   # change it later
hyprspanso config                            # show what is in effect
```

Resolution order, highest first:

1. `HYPRSPANSO_CONFIG=/path` (env, temporary override)
2. the saved `config_dir` in `~/.config/hyprspanso/config.json`
3. `$XDG_CONFIG_HOME/espanso` (default, espanso-compatible)

`install.sh` runs `hyprspanso init` for you, and the AUR package's post-install
message points at it.

## 4. Start it

```bash
systemctl --user daemon-reload
systemctl --user disable --now espanso   # only if espanso is installed
systemctl --user enable --now hyprspanso

hyprspanso status                        # enabled=true matches=N
```

`install.sh` deliberately does **not** start the daemon, so it can never fight a
running espanso.

## 4b. Floating windows (Automatic)

The search, picker and form windows run inside a terminal. To make them float
centred instead of opening tiled, `install.sh` runs:

```bash
hyprspanso setup-hyprland
```

It writes `~/.config/hypr/hyprspanso.lua` and adds
`require("hypr.hyprspanso")` to `hyprland.lua` (or appends a `windowrule` line
to `hyprland.conf` on non-Omarchy setups), makes a timestamped backup, and
reloads Hyprland. It is idempotent.

If the windows ever open tiled:

```bash
hyprspanso setup-hyprland          # add the rule
hyprspanso setup-hyprland --print  # show the snippet to paste manually
```

## 5. Keybinding (Hyprland)

Add to `~/.config/hypr/bindings.lua`:

```lua
hl.unbind("CTRL + SPACE")   -- Omarchy's terminal-prefix bind, if present
o.bind("CTRL + SPACE", "Search snippets", "$HOME/.local/bin/hyprspanso launcher")
```

Then `hyprctl reload` (or just save the file, Hyprland reloads it).

Pick another key if `CTRL + SPACE` is taken: any free binding works, e.g.
`SUPER + ALT + E`.

## 6. Optional: Omarchy bar indicator

Only for the Omarchy (Quickshell) bar, not plain Hyprland:

```bash
./contrib/omarchy/install-bar-widget.sh
```

A keyboard icon appears next to the clock: solid when expanding, faded when
paused or stopped. Left click opens the search window, right click pauses,
middle click reloads the match files.

## 7. Verify

```bash
hyprspanso check     # files loaded, matches, and anything skipped
```

Type `:date` in any application — it should insert today's date. If the starter
file was not written because you already had matches, type a trigger from your
own files.

## Where match files live

hyprspanso reads every `.yml`/`.yaml` file under **`<config>/match/`**, recursively:

| Config root | Matches read from |
|---|---|
| chosen on first run (`hyprspanso init`) | `<chosen>/match/` |
| `hyprspanso config set DIR` | `DIR/match/` |
| default (espanso-compatible) | `~/.config/espanso/match/` |
| `HYPRSPANSO_CONFIG=/path/to/espanso` | `/path/to/espanso/match/` (env wins) |

```bash
hyprspanso config                     # which folder is in effect?
cp examples/matches.yml "$(hyprspanso config | awk '/^matches dir/{print $4}')/mine.yml"
hyprspanso reload                     # or wait ~2 s, it watches the directory
```

Notes:

- The file name is free — anything ending in `.yml` or `.yaml`. `hyprspanso init`
  writes `match/matches.yml`.
- Subdirectories are read too, so espanso packages (`match/packages/…`) work.
- Files directly in `~/.config/espanso/` are **ignored**: that is espanso's
  settings area (`config/default.yml`), not matches.

## Using your existing snippets

hyprspanso reads espanso's YAML (read-only) from `<folder>/match/`. If your
snippets live in a synced folder (Insync/Drive, Nextcloud, a git checkout),
point hyprspanso straight at it — no symlink required:

```bash
hyprspanso config set "$HOME/Insync/you@example.com/Google Drive/espanso"
systemctl --user restart hyprspanso
```

The folder is stored in `~/.config/hyprspanso/config.json`, so it survives
reboots and works for the systemd service. The daemon reloads on file changes
within ~2 s, including files written by a cloud client.

## Updating

Your matches and settings live in `~/.config/espanso` (or wherever you pointed
`config_dir`) plus `~/.config/hyprspanso/config.json`, so updating only replaces
the binary and the unit.

### From a git checkout

```bash
cd hyprspanso
git pull
./install.sh
systemctl --user restart hyprspanso
```

### From the prebuilt tarball

```bash
curl -LO https://github.com/letoloke/hyprspanso/releases/latest/download/hyprspanso-x86_64-linux.tar.gz
tar xf hyprspanso-x86_64-linux.tar.gz
./install.sh
systemctl --user restart hyprspanso
```

### From the AUR

```bash
yay -Syu hyprspanso
systemctl --user restart hyprspanso     # pacman does not restart services
```

**The restart matters**: `install.sh` and pacman replace the file on disk, but
the running daemon keeps the old code in memory until it is restarted.

### Verify

```bash
hyprspanso --version
hyprspanso check
hyprspanso diag
```

`install.sh` re-runs `hyprspanso init`, which is a no-op when you already have
match files, so your snippets are never touched.

## Troubleshooting

```bash
hyprspanso status                 # running/enabled and match count
hyprspanso diag                   # keys seen, matches, buffer, held modifiers
hyprspanso diag --keys            # …plus the last 64 decoded characters
hyprspanso check                  # config loaded? anything skipped?
systemctl --user restart hyprspanso
journalctl --user -u hyprspanso -f
```

Verbose logs:

```bash
systemctl --user edit hyprspanso
# [Service]
# Environment=HYPRSPANSO_DEBUG=1
systemctl --user restart hyprspanso
journalctl --user -u hyprspanso -f
```

Add `HYPRSPANSO_DEBUG_KEYS=1` to log every decoded key.

Common causes:

| Symptom | Cause |
|---|---|
| nothing expands, bar says active | user not in `input` group, or the keyboard re-enumerated on an old build — restart the daemon (fixed in 0.2.1) |
| nothing expands, wireless keyboard | some dongles expose two event nodes (`input0`/`input2`) and a re-pair can move the keys to the other one; 0.5.1+ reads every node and de-duplicates, older builds only read one — upgrade and restart |
| modifier stuck (text turns uppercase) | `diag --keys` now shows the xkb masks (`d…/l…/k…`); 0.7.3 rebuilds the state on focus/device changes and after 60 s, which clears even latched/locked bits |
| modifier stuck (text turns uppercase) | a second event node of the same keyboard was replaying presses after the real release; 0.7.1 reads one node per physical keyboard (echo nodes ignored, takeover after 2 s of silence) — upgrade and restart |
| worked, then stopped silently | run `hyprspanso diag --keys` **before** restarting: `events` frozen means no keys reach the daemon; `events` growing with `held_mods` set means a lost key release; `buffer` staying at 0–1 with `last_decoded` incomplete means something is resetting the buffer (fixed in 0.5.3 for title changes) |
| several snippets share a trigger | 0.7.0+ opens a picker (previously only the first match was used); type it and choose, or remove the duplicates |
| search/picker/form window opens tiled | the floating rule is missing: run `hyprspanso setup-hyprland` (install.sh does this since 0.7.2) |
| form window does not appear | the daemon launches it with `omarchy-launch-tui` (or `xdg-terminal-exec`); check `journalctl --user -u hyprspanso` for `cannot open form window` |
| triggers fail only in some apps | 0.5.3 fixed window-title changes resetting the trigger buffer (`activewindow` also fires on title updates); upgrade |
| one match never fires | it uses `vars: shell`/`random`, `form:`, or `type: markdown` — see `hyprspanso check` |
| text appears in some apps only | Electron/XWayland apps ignore the virtual keyboard; add `force_clipboard: true` to that match |
| triggers expand twice | both espanso and hyprspanso are running — stop one |

## Security note

Reading `/dev/input` is keylogger-level access by nature. hyprspanso keeps a
64-character buffer in memory only, writes no logs of typing, and contains no
network code.
