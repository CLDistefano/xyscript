//! Core data model for the keyboard layer.
//!
//! See `keyboard-layer-design.md` §2, §3, §4, §11. The single load-bearing idea
//! is that a layout is a function `(modifier_bitmask, key_code) -> assignment`.
//! Key codes and modifier bits are kept as a *pair* internally; the on-disk
//! "doubling" numbering (256, 512, ...) is a file-format detail only (§2).

use std::collections::HashMap;

/// Canonical key-code numbering (§3.4).
///
/// `1..=104` is the classic IBM set-1 range that DOS exposed; codes `105+`
/// extend the editor's own numbering and are mapped into existence per-platform
/// by the normalization tables (§7). `u8` covers up to 255 keys, far beyond any
/// realistic large keyboard, so widening is never needed in practice.
pub type KeyCode = u8;

/// OR of currently-active modifier bits (§2, §4).
///
/// `u32` deliberately, so the number of user-definable shift keys is effectively
/// unbounded. Internally a modifier may occupy *any* bit, fully decoupled from
/// the key-code range — the on-disk packing above 256 is not honored internally.
pub type Bitmask = u32;

/// How a declared modifier behaves while its trigger key is operated (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModBehavior {
    /// Momentary: the bit is set only while the trigger key is physically down.
    /// Shift, Alt, Ctrl, and most custom shift keys are Hold.
    Hold,
    /// Latching: a tap flips the bit and it stays flipped (Caps Lock). Declared
    /// with the `T:C` attribute in the KBD header.
    Toggle,
}

/// One declared shifting key (§4, §11).
///
/// There is no hardcoded notion of Ctrl/Alt/Shift — these are ordinary
/// `ModifierDef`s. A new shift key is *only* a new entry here: a name, a bit,
/// the trigger key code(s), and a behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModifierDef {
    /// "CAPS", "SHIFT", "ALT", "CTRL", "HOTKEY", or any user-chosen name.
    pub name: String,
    /// Bit assigned at parse time from the header order (§2).
    pub bit: Bitmask,
    /// Physical key code(s) that trigger this modifier (Shift can be 42 and 54).
    pub trigger_keys: Vec<KeyCode>,
    /// Hold (momentary) by default; Toggle (latching) via `T:C`.
    pub behavior: ModBehavior,
    /// True when this key also has its own tap assignment in some table, i.e. it
    /// is dual-role (§5) and needs the tap-vs-hold state machine. Computed by the
    /// parser after both header and tables are known.
    pub dual_role: bool,
}

/// A resolved cell (§6.1, §10.1, §11).
///
/// Either a reserved input-layer directive the keyboard layer acts on *itself*,
/// or an opaque XPL string handed verbatim to the interpreter. The keyboard
/// layer never parses, validates, or executes the XPL — that is the
/// interpreter's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignment {
    /// `KF` (Keyboard Flip): do NOT consume; release the event to the OS (§6.1).
    /// The reserved set is kept deliberately minimal; this is its first member.
    Flip,
    /// Opaque assignment. The interpreter parses/executes it as XPL.
    Xpl(String),
}

impl Assignment {
    /// Recognize the small reserved set of input-control directives (§10.1).
    /// Everything else is opaque XPL. The match is exact and case-insensitive on
    /// the trimmed cell text so `KF` / `kf` both flip.
    pub fn parse(raw: &str) -> Assignment {
        if raw.trim().eq_ignore_ascii_case("KF") {
            Assignment::Flip
        } else {
            Assignment::Xpl(raw.to_string())
        }
    }
}

/// The immutable, parsed layout (§9.1, §11).
///
/// Produced by the parser and never mutated while keys are pressed — all runtime
/// change lives in `InputState`. `tables` is keyed on the `(bitmask, key_code)`
/// pair so lookup is a single strict map probe with no fallthrough (§6).
#[derive(Debug, Clone)]
pub struct Keymap {
    /// Declared shifting keys in header order (bit assignment order).
    pub modifiers: Vec<ModifierDef>,
    /// Strict lookup table. A missing key is a miss => swallow (§6).
    pub tables: HashMap<(Bitmask, KeyCode), Assignment>,
    /// Highest legal key code from `KEYS=<n>` (§3.4); used for range validation.
    pub keys_max: KeyCode,
    /// First key code of the reserved *virtual* band (§3.5). Codes `>= virtual_base`
    /// are never emitted by any normalization table; they are reachable only via
    /// the injection channel. `None` means no virtual band reserved.
    pub virtual_base: Option<KeyCode>,
}

impl Keymap {
    /// Strict resolution (§6). Returns `None` on a miss (the caller swallows).
    pub fn resolve(&self, bitmask: Bitmask, key: KeyCode) -> Option<&Assignment> {
        self.tables.get(&(bitmask, key))
    }

    /// Find the modifier (if any) triggered by a physical key code.
    pub fn modifier_for_key(&self, key: KeyCode) -> Option<&ModifierDef> {
        self.modifiers.iter().find(|m| m.trigger_keys.contains(&key))
    }

    /// Sum the bits of the named modifiers, e.g. "ALT+CAPS" -> bit(ALT)|bit(CAPS).
    /// Order is irrelevant (§3.2). Returns `None` if any name is unknown.
    pub fn bitmask_for_names(&self, names: &[&str]) -> Option<Bitmask> {
        let mut mask = 0;
        for n in names {
            let m = self.modifiers.iter().find(|m| m.name.eq_ignore_ascii_case(n))?;
            mask |= m.bit;
        }
        Some(mask)
    }
}
