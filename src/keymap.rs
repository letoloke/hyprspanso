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
        let name = name.trim();
        if name.is_empty()
            || name.eq_ignore_ascii_case("error")
            || name.eq_ignore_ascii_case("unknown")
        {
            return;
        }

        if std::env::var_os("HYPRSPANSO_DEBUG").is_some() {
            eprintln!(
                "hyprspanso: set_layout {name:?} layouts={:?}",
                self.layouts
            );
        }

        if let Some(index) = self.find_layout(name) {
            if std::env::var_os("HYPRSPANSO_DEBUG").is_some() {
                eprintln!("hyprspanso: set_layout -> group {index}");
            }
            self.apply_group(index);
            return;
        }

        // The keymap may be stale if layouts were added at runtime
        // (`hyprctl keyword input:kb_layout ...`). Re-read it once and retry.
        if self.reload_keymap() {
            if let Some(index) = self.find_layout(name) {
                self.apply_group(index);
                return;
            }
        }

        // Unknown name: keep the current group instead of guessing. Guessing
        // used to flip us<->latam on bogus events, decoding punctuation wrong.
    }

    fn find_layout(&self, name: &str) -> Option<u32> {
        let needle = name.to_lowercase();
        self.layouts.iter().position(|layout| {
            let layout = layout.to_lowercase();
            layout == needle || layout.contains(&needle) || needle.contains(&layout)
        }).map(|index| index as u32)
    }

    fn apply_group(&mut self, index: u32) {
        let depressed = self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let latched = self.state.serialize_mods(xkb::STATE_MODS_LATCHED);
        let locked = self.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        self.state
            .update_mask(depressed, latched, locked, 0, 0, index);
    }

    fn reload_keymap(&mut self) -> bool {
        let Some(text) = crate::hypr::dump_keymap() else {
            return false;
        };
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(keymap) = xkb::Keymap::new_from_string(
            &ctx,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) else {
            return false;
        };
        self.layouts = (0..keymap.num_layouts())
            .map(|index| keymap.layout_get_name(index).to_string())
            .collect();
        self.keymap = keymap;
        self.state = xkb::State::new(&self.keymap);
        self.modifier_since = None;
        true
    }

    /// Throw the whole modifier/latch state away and start from the keymap.
    /// Stronger than releasing individual keys: it also clears anything that
    /// ended up latched or locked, which `update_key(Up)` cannot undo.
    pub fn reset_state(&mut self) {
        self.state = xkb::State::new(&self.keymap);
        self.modifier_since = None;
    }

    /// No one holds a modifier for a minute. If the state still thinks one is
    /// down, its release was lost (device re-pair, dropped event): clear it so
    /// typing does not silently turn into uppercase/shortcuts.
    pub fn tick(&mut self) {
        const LIMIT: Duration = Duration::from_secs(60);
        if let Some(since) = self.modifier_since {
            if since.elapsed() > LIMIT {
                self.reset_state();
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
            "g{} d{:x} l{:x} k{:x}",
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder_for_test() -> Decoder {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&ctx, "", "", "us,latam", "", None, 0).expect("keymap");
        let layouts = (0..keymap.num_layouts())
            .map(|index| keymap.layout_get_name(index).to_string())
            .collect();
        Decoder {
            state: xkb::State::new(&keymap),
            keymap,
            layouts,
            modifier_since: None,
        }
    }

    #[test]
    fn switching_layout_changes_the_group() {
        let mut decoder = decoder_for_test();
        assert_eq!(decoder.layouts.len(), 2);
        decoder.set_layout(&decoder.layouts[1].clone());
        assert_eq!(
            decoder
                .state
                .serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            1
        );
        decoder.set_layout(&decoder.layouts[0].clone());
        assert_eq!(
            decoder
                .state
                .serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            0
        );
    }
}
