//! XyScript keyboard layer — prototype.
//!
//! Implements the design in `keyboard-layer-design.md`: a data-driven `.KBD`
//! modifier model `(bitmask, key_code) -> assignment`, strict lookup with
//! swallow-on-miss and a single `KF` pass-through, hold/toggle modifiers, a
//! tap-vs-hold state machine for dual-role keys, an access-controlled injection
//! channel for virtual keys, transactional loading over an embedded default, and
//! per-OS non-exclusive interception wired into one shared resolver.
//!
//! The headless engine (everything except `platform`) has no OS dependencies and
//! is unit-tested in `tests/`. The `platform` module holds the three OS hooks.

pub mod diagnostic;
pub mod event;
pub mod inject;
pub mod input_state;
pub mod loader;
pub mod model;
pub mod normalize;
pub mod parser;
pub mod pipeline;
pub mod resolver;

pub mod platform;

pub use loader::{embedded_default, load, LoadOutcome};
pub use model::{Assignment, Bitmask, KeyCode, Keymap, ModBehavior, ModifierDef};
pub use resolver::{Disposition, Resolver};
