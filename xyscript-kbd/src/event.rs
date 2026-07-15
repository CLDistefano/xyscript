//! Canonical events and the multi-source front of the pipeline (§3.5, §8, §11).
//!
//! Every input source — the OS normalizer and the programmatic injection
//! channel — produces the *same* internal `KeyEvent`, which enters the shared
//! resolver. Only the entry point differs.

use crate::model::{Bitmask, KeyCode};

/// Physical key phase. Injected (virtual) events are discrete and carry no
/// timed down/up, so they use `Inject` (§3.5: hold/toggle and the tap-vs-hold SM
/// are N/A for injection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
    /// A programmatic poke: "this cell fired", not a timed press (§3.5).
    Inject,
}

/// The internal canonical event fed to the resolver (§11).
///
/// For physical events `bitmask` is ignored by the resolver (it computes the
/// live bitmask from `InputState`); for injected events the carried `bitmask`
/// *is* the layer (§3.5) — injection is deterministic and independent of whatever
/// the user is physically holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key_code: KeyCode,
    pub bitmask: Bitmask,
    pub phase: KeyPhase,
}

impl KeyEvent {
    pub fn down(key_code: KeyCode) -> Self {
        KeyEvent { key_code, bitmask: 0, phase: KeyPhase::Down }
    }
    pub fn up(key_code: KeyCode) -> Self {
        KeyEvent { key_code, bitmask: 0, phase: KeyPhase::Up }
    }
    /// A poke carrying its full `(bitmask, key_code)` pair (§3.5).
    pub fn inject(key_code: KeyCode, bitmask: Bitmask) -> Self {
        KeyEvent { key_code, bitmask, phase: KeyPhase::Inject }
    }
}

/// An event source yields canonical `KeyEvent`s into the shared resolver (§8).
/// The OS normalizer is one source; the injection channel is another. The
/// resolver does not care which.
pub trait EventSource {
    /// Block for the next event, or return `None` when the source is exhausted
    /// or shut down.
    fn next_event(&mut self) -> Option<KeyEvent>;
}
