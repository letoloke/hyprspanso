//! keycode -> text, using the compositor's own keymap.

use anyhow::{anyhow, Result};
use std::time::{Duration, Instant};
use xkbcommon::xkb;

pub struct Decoder {
    keymap: xkb::Keymap,
    state: xkb::State,
    layouts: Vec<String>,
    /// When the currently held modifier was pressed; used to recover from a
    /// release that never arrived.
    modifier_since: Option<Instant>,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

        // Best case: the exact keymap Hyprland is using, groups included.
        let from_compositor = crate::hypr::dump_keymap().and_then(|text| {
            xkb::Keymap::new_from_string(
                &ctx,
                text,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        });

        let keymap = from_compositor
            .or_else(|| {
                let (layout, variant) = crate::hypr::main_layout();
                xkb::Keymap::new_from_names(
                    &ctx,
                    "",
                    "",
                    &layout,
                    &variant,
                    None,
                    xkb::KEYMAP_COMPILE_NO_FLAGS,
                )
            })
            .ok_or_else(|| anyhow!("could not build an xkb keymap from Hyprland"))?;

        let layouts: Vec<String> = (0..keymap.num_layouts())
            .map(|i| keymap.layout_get_name(i).to_string())
            .collect();
        let state = xkb::State::new(&keymap);

        Ok(Self {
            keymap,
            state,
            layouts,
            modifier_since: None,
        })
    }

    pub fn layout_names(&self) -> &[String] {
        &self.layouts
    }

    /// Feed every key event (including releases) so modifier state stays sane.
    /// Returns the typed text for key presses and repeats.
    pub fn feed(&mut self, code: u16, value: i32) -> Option<String> {
        let keycode = xkb::Keycode::new(u32::from(code) + 8);
        if value == 0 {
            self.state.update_key(keycode, xkb::KeyDirection::Up);
            if is_modifier(code) {
                self.modifier_since = None;
            }
            return None;
        }
        if is_modifier(code) && self.modifier_since.is_none() {
            self.modifier_since = Some(Instant::now());
        }
        self.state.update_key(keycode, xkb::KeyDirection::Down);
        let text = self.state.key_get_utf8(keycode);
        if text.is_empty() || text.chars().any(char::is_control) {
            None
        } else {
            Some(text)
        }
    }

    /// Hyprland emits `activelayout>>kbd,<name>`. Match it against the keymap
    /// groups; if the name does not match, just move to the next group (which
    /// is correct for the usual two-layout setup).
    pub fn set_layout(&mut self, name: &str) {
        if self.layouts.len() <= 1 {
            return;
        }
        let needle = name.to_lowercase();
        let matched = self.layouts.iter().position(|l| {
            let l = l.to_lowercase();
            l == needle || l.contains(&needle) || needle.contains(&l)
        });

        let index = match matched {
            Some(i) => i as u32,
            None => {
                let current = self.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED);
                (current + 1) % self.layouts.len() as u32
            }
        };

        let depressed = self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let latched = self.state.serialize_mods(xkb::STATE_MODS_LATCHED);
        let locked = self.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        self.state
            .update_mask(depressed, latched, locked, 0, 0, index);
    }

    /// No one holds a modifier for a minute. If the state still thinks one is
    /// down, its release was lost (device re-pair, dropped event): clear it so
    /// typing does not silently turn into uppercase/shortcuts.
    pub fn tick(&mut self) {
        const LIMIT: Duration = Duration::from_secs(60);
        if let Some(since) = self.modifier_since {
            if since.elapsed() > LIMIT {
                self.reset_modifiers();
            }
        }
    }

    /// Compact description of the xkb state, for `diag --keys`:
    /// `S` shift, `C` caps lock, `T` control, `A` alt, `M` logo, `g<n>` group.
    pub fn state_summary(&self) -> String {
        let mut out = String::new();
        for (name, letter) in [
            ("Shift", 'S'),
            ("Lock", 'C'),
            ("Control", 'T'),
            ("Mod1", 'A'),
            ("Mod4", 'M'),
        ] {
            if self
                .state
                .mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE)
            {
                out.push(letter);
            }
        }
        out.push_str(&format!(
            "g{}",
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE)
        ));
        out
    }

    /// Drop any modifier the state still believes is held. A key release can
    /// be lost when a keyboard re-enumerates (wireless dongle, USB reset),
    /// which otherwise leaves everything shifted/control'd until a restart.
    pub fn reset_modifiers(&mut self) {
        self.modifier_since = None;
        const MODIFIERS: [u16; 8] = [29, 97, 42, 54, 56, 100, 125, 126];
        for code in MODIFIERS {
            let keycode = xkb::Keycode::new(u32::from(code) + 8);
            self.state.update_key(keycode, xkb::KeyDirection::Up);
        }
    }

    #[allow(dead_code)]
    pub fn keymap(&self) -> &xkb::Keymap {
        &self.keymap
    }
}

/// evdev keycodes for Shift/Ctrl/Alt/Meta on both sides.
fn is_modifier(code: u16) -> bool {
    matches!(code, 29 | 97 | 42 | 54 | 56 | 100 | 125 | 126)
}
