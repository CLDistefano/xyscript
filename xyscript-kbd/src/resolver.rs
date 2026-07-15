//! The shared resolution stage (§6, §8, §10.1).
//!
//! Every source feeds this one stage. It turns a canonical `KeyEvent` into a
//! `Disposition`: swallow, dispatch an opaque XPL string to the interpreter, or
//! flip the event to the OS. The reserved `KF` directive is recognized here,
//! synchronously, because the consume-vs-release decision must be made inside the
//! OS event callback before default processing (§6.1).

use crate::event::{KeyEvent, KeyPhase};
use crate::input_state::{InputState, ModifierUp};
use crate::model::{Assignment, Keymap};

/// The three outcomes of resolution (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Consumed, nothing happens (a miss, a modifier edge, a suppressed tap).
    Swallow,
    /// Consumed; hand this opaque assignment string to the XPL interpreter.
    Dispatch(String),
    /// NOT consumed; release the keystroke to the OS for normal handling (`KF`).
    Flip,
}

/// Owns the immutable keymap and the mutable input state, and resolves events.
pub struct Resolver {
    pub keymap: Keymap,
    pub state: InputState,
}

impl Resolver {
    pub fn new(keymap: Keymap) -> Self {
        Resolver { keymap, state: InputState::new() }
    }

    /// Hot-swap the operative keymap (transactional load already validated it,
    /// §9.2). Runtime state is reset so stale held/latched bits cannot leak
    /// across a layout change.
    pub fn install_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
        self.state = InputState::new();
    }

    /// Drop all momentary held-modifier state (see [`InputState::release_held`]). Call on focus
    /// loss so a modifier held during an app switch (Alt+Tab) doesn't stay stuck down and
    /// misroute later keys. Latched (toggle) state is preserved.
    pub fn release_held(&mut self) {
        self.state.release_held();
    }

    fn disposition_for(&self, assignment: Option<&Assignment>) -> Disposition {
        match assignment {
            None => Disposition::Swallow,                 // miss (§6)
            Some(Assignment::Flip) => Disposition::Flip,  // KF (§6.1)
            Some(Assignment::Xpl(s)) => Disposition::Dispatch(s.clone()),
        }
    }

    /// Resolve one canonical event end-to-end (§8 steps 2–4).
    pub fn resolve(&mut self, ev: KeyEvent) -> Disposition {
        // ----- Injection source: skip all physical bookkeeping (§3.5) ---------
        if ev.phase == KeyPhase::Inject {
            let a = self.keymap.resolve(ev.bitmask, ev.key_code);
            // KF is N/A for a virtual key — there is no OS event to release, so a
            // poke that lands on a KF cell is a no-op (§3.5).
            return match self.disposition_for(a) {
                Disposition::Flip => Disposition::Swallow,
                other => other,
            };
        }

        // ----- Is this key a registered modifier trigger? (§8 step 2) ---------
        // Clone the matched def so we don't hold an immutable borrow of the
        // keymap across the mutable state calls.
        let modifier = self
            .keymap
            .modifier_for_key(ev.key_code)
            .cloned();

        if let Some(def) = modifier {
            match ev.phase {
                KeyPhase::Down => {
                    self.state.modifier_down(&def, ev.key_code);
                    Disposition::Swallow // modifier edges are swallowed (§6)
                }
                KeyPhase::Up => match self.state.modifier_up(&def, ev.key_code) {
                    ModifierUp::Swallow => Disposition::Swallow,
                    ModifierUp::FireTap(key) => {
                        // Dual-role tap (§5): resolve the modifier key's own cell
                        // in the layer formed by whatever else is still held.
                        let mask = self.state.current_bitmask();
                        let a = self.keymap.resolve(mask, key);
                        self.disposition_for(a)
                    }
                },
                KeyPhase::Inject => unreachable!(),
            }
        } else {
            // ----- Ordinary key (§8 steps 3–4) -------------------------------
            match ev.phase {
                KeyPhase::Down => {
                    // Any non-modifier press consumes held dual-role modifiers.
                    self.state.mark_chord(&self.keymap.modifiers);
                    self.state.note_plain_down(ev.key_code);
                    let mask = self.state.current_bitmask();
                    let a = self.keymap.resolve(mask, ev.key_code);
                    self.disposition_for(a)
                }
                KeyPhase::Up => {
                    // XyWrite resolves on the down edge; plain ups are swallowed.
                    self.state.note_plain_up(ev.key_code);
                    Disposition::Swallow
                }
                KeyPhase::Inject => unreachable!(),
            }
        }
    }
}
