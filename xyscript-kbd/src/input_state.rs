//! Mutable runtime state (§4, §5, §11).
//!
//! The `Keymap` is immutable; `InputState` is the only thing that changes as
//! keys are pressed. It owns: the held (momentary) bits, the latched (toggle)
//! bits, the set of physically-down keys (to dedupe auto-repeat), and the
//! per-dual-role "consumed since press" flags for the tap-vs-hold machine (§5).

use crate::model::{Bitmask, ModBehavior, ModifierDef, KeyCode};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct InputState {
    held: Bitmask,
    latched: Bitmask,
    down_keys: HashSet<KeyCode>,
    /// Per dual-role modifier *bit*: has a non-modifier key been pressed since
    /// this modifier went down? If so the modifier is "consumed" and its tap is
    /// suppressed on release (§5).
    consumed: HashMap<Bitmask, bool>,
}

/// What a modifier *up* edge wants the resolver to do next.
pub enum ModifierUp {
    /// Nothing further; the event is swallowed.
    Swallow,
    /// This dual-role modifier was tapped (not consumed): resolve and dispatch
    /// the cell at `(current_bitmask, key)` — its own tap assignment (§5).
    FireTap(KeyCode),
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    /// current_bitmask = held | latched (§4).
    pub fn current_bitmask(&self) -> Bitmask {
        self.held | self.latched
    }

    pub fn held(&self) -> Bitmask {
        self.held
    }
    pub fn latched(&self) -> Bitmask {
        self.latched
    }

    /// Handle a modifier-trigger key going **down** (§4, §5). Returns whether the
    /// edge was real (false = ignored auto-repeat).
    pub fn modifier_down(&mut self, def: &ModifierDef, key: KeyCode) -> bool {
        if self.down_keys.contains(&key) {
            return false; // auto-repeat: no edge, swallow without re-flipping
        }
        self.down_keys.insert(key);
        match def.behavior {
            ModBehavior::Hold => {
                self.held |= def.bit;
                if def.dual_role {
                    self.consumed.insert(def.bit, false);
                }
            }
            ModBehavior::Toggle => {
                // A tap flips the latched bit and it stays flipped (§4).
                self.latched ^= def.bit;
            }
        }
        true
    }

    /// Handle a modifier-trigger key going **up** (§4, §5).
    pub fn modifier_up(&mut self, def: &ModifierDef, key: KeyCode) -> ModifierUp {
        self.down_keys.remove(&key);
        match def.behavior {
            ModBehavior::Hold => {
                // Clear the bit FIRST so the tap resolves in the layer formed by
                // the *other* still-held modifiers (tapping alone => unshifted).
                self.held &= !def.bit;
                let was_consumed = self.consumed.remove(&def.bit).unwrap_or(true);
                if def.dual_role && !was_consumed {
                    ModifierUp::FireTap(key)
                } else {
                    ModifierUp::Swallow
                }
            }
            ModBehavior::Toggle => ModifierUp::Swallow,
        }
    }

    /// A non-modifier key was pressed: every currently-held dual-role modifier is
    /// now "consumed" (it formed a chord), so its tap will be suppressed (§5).
    pub fn mark_chord(&mut self, modifiers: &[ModifierDef]) {
        for m in modifiers {
            if m.dual_role && (self.held & m.bit) != 0 {
                self.consumed.insert(m.bit, true);
            }
        }
    }

    /// Release all MOMENTARY state — the held (hold-behaviour) modifier bits, the set of
    /// physically-down keys, and the dual-role "consumed" flags. Call when the input source
    /// loses keyboard focus (e.g. the window is Alt+Tabbed away): the matching key-UP edges are
    /// then delivered to whatever gains focus, never to us, so a modifier held across the switch
    /// would otherwise stay stuck "down" forever and misroute every later key. LATCHED (toggle,
    /// e.g. Caps) bits are deliberately KEPT, since a latch isn't tied to a key being held.
    pub fn release_held(&mut self) {
        self.held = 0;
        self.down_keys.clear();
        self.consumed.clear();
    }

    /// Track a non-modifier physical key's down/up purely for auto-repeat dedupe
    /// (keeps `down_keys` honest if callers want it). Resolution happens on down.
    pub fn note_plain_down(&mut self, key: KeyCode) -> bool {
        self.down_keys.insert(key)
    }
    pub fn note_plain_up(&mut self, key: KeyCode) {
        self.down_keys.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModBehavior;

    fn modifier(name: &str, bit: Bitmask, key: KeyCode, behavior: ModBehavior) -> ModifierDef {
        ModifierDef { name: name.into(), bit, trigger_keys: vec![key], behavior, dual_role: false }
    }

    #[test]
    fn release_held_clears_held_but_keeps_latched() {
        let alt = modifier("ALT", 0b001, 56, ModBehavior::Hold);
        let caps = modifier("CAPS", 0b100, 58, ModBehavior::Toggle);
        let mut st = InputState::new();
        st.modifier_down(&caps, 58); // latch Caps on (toggle)
        st.modifier_down(&alt, 56); // hold Alt down
        assert_eq!(st.current_bitmask(), 0b101); // Alt held | Caps latched

        // Simulate losing focus mid-Alt+Tab: the Alt-up never arrives.
        st.release_held();
        assert_eq!(st.held(), 0); // Alt no longer stuck
        assert_eq!(st.latched(), 0b100); // Caps latch survives
        assert_eq!(st.current_bitmask(), 0b100);
        // The released key reads as up, so a fresh press is a real edge again (not auto-repeat).
        assert!(st.note_plain_down(56));
    }
}
