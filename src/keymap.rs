//! keycode -> text, using the compositor's own keymap.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use xkbcommon::xkb;

pub struct Decoder {
    keymap: xkb::Keymap,
    state: xkb::State,
    layouts: Vec<String>,
    /// When the currently held modifier was pressed; used to recover from a
    /// release that never arrived.
    modifier_since: Option<Instant>,
    /// Layout group per keyboard (`normalized name -> group`). Hyprland keeps
    /// a layout per keyboard, so two keyboards may be on different layouts at
    /// the same time; decoding has to follow the keyboard that typed the key.
    groups: HashMap<String, (u32, Instant)>,
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
            groups: HashMap::new(),
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
    /// Remember a keyboard's layout (from `activelayout`). Returns whether it
    /// was applied to the state we are currently decoding with.
    pub fn note_layout(&mut self, keyboard: &str, layout: &str, current: Option<&str>) -> bool {
        let Some(index) = self.resolve_group(layout) else {
            return false;
        };
        let key = crate::hypr::normalize_name(keyboard);
        self.groups.insert(key.clone(), (index, Instant::now()));

        let relevant = current
            .map(|device| crate::hypr::normalize_name(device) == key)
            .unwrap_or(true);
        if relevant {
            let current_group = self.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED);
            if current_group != index {
                self.apply_group(index);
            }
        }
        relevant
    }

    /// Called for every key: switches the decoding group to the layout of the
    /// keyboard that produced it. After the first key of a keyboard the group
    /// comes from the cache, so alternating keyboards stays cheap.
    pub fn prepare_device(&mut self, device: &str) {
        let key = crate::hypr::normalize_name(device);
        const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

        let cached = self
            .groups
            .get(&key)
            .filter(|(_, seen)| seen.elapsed() < CACHE_TTL)
            .map(|(index, _)| *index);

        let index = if let Some(index) = cached {
            Some(index)
        } else if let Some(layout) = crate::hypr::active_layout_for(device) {
            let index = self.resolve_group(&layout);
            if let Some(index) = index {
                self.groups.insert(key, (index, Instant::now()));
            }
            index
        } else {
            // Keep the last known group if hyprctl is unavailable.
            self.groups.get(&key).map(|(index, _)| *index)
        };

        if let Some(index) = index {
            if self.state.serialize_layout(xkb::STATE_LAYOUT_LOCKED) != index {
                self.apply_group(index);
            }
        }
    }

    fn resolve_group(&mut self, layout: &str) -> Option<u32> {
        if layout.trim().is_empty()
            || layout.eq_ignore_ascii_case("error")
            || layout.eq_ignore_ascii_case("unknown")
        {
            return None;
        }
        if let Some(index) = self.find_layout(layout) {
            return Some(index);
        }
        // Stale keymap after a runtime layout change: re-read it once.
        if self.reload_keymap() {
            return self.find_layout(layout);
        }
        None
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
            groups: HashMap::new(),
        }
    }

    #[test]
    fn layouts_are_tracked_per_keyboard() {
        let mut decoder = decoder_for_test();
        let latam = decoder.layouts[1].clone();
        let us = decoder.layouts[0].clone();

        // two keyboards on different layouts
        decoder.note_layout("kbd-a", &latam, Some("kbd-a"));
        decoder.note_layout("kbd-b", &us, Some("kbd-b"));

        decoder.prepare_device("kbd-a");
        assert_eq!(
            decoder.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            1
        );
        decoder.prepare_device("kbd-b");
        assert_eq!(
            decoder.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            0
        );
        decoder.prepare_device("kbd-a");
        assert_eq!(
            decoder.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            1
        );
    }

    #[test]
    fn switching_layout_changes_the_group() {
        let mut decoder = decoder_for_test();
        assert_eq!(decoder.layouts.len(), 2);
        let latam = decoder.layouts[1].clone();
        decoder.note_layout("test", &latam, Some("test"));
        assert_eq!(
            decoder
                .state
                .serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            1
        );
        let us = decoder.layouts[0].clone();
        decoder.note_layout("test", &us, Some("test"));
        assert_eq!(
            decoder
                .state
                .serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            0
        );
    }
}
