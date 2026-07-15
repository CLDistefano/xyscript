//! Cross-platform scancode normalization (§7).
//!
//! This is the layer's hardest, most platform-specific work and the ONLY
//! platform-specific piece above the OS hooks. Each OS hands us a different
//! native numbering; a per-OS table maps `native_code -> canonical KeyCode`
//! (the editor's 1..=104 classic numbering, extensible past 104 per §3.4).
//! Above this table, everything is identical on all three platforms.
//!
//! The tables are deliberately **data, not code**: a `ScancodeTable` is just a
//! map, so a keyboard quirk can be fixed by editing data rather than logic. In a
//! finished editor these would load from a small per-platform data file and be
//! the maintained "canonical key-code chart" artifact (§3.4). Here they are
//! built in code, kept faithful for the keys the §12 experiment cares about
//! (Caps Lock, the function keys, and the numpad including Numpad 5 = 76).
//!
//! **Virtual-band rule (§3.5):** a normalization table must NEVER emit a code in
//! the reserved virtual band. `ScancodeTable::insert` enforces this, so a
//! physical keystroke can never produce a virtual code.

use crate::model::KeyCode;
use std::collections::HashMap;

/// Maps a platform-native code to the editor's canonical key code (§7, §11).
pub trait ScancodeMap {
    fn to_canonical(&self, native: u32) -> Option<KeyCode>;
}

/// A data-driven normalization table with the virtual-band guarantee baked in.
#[derive(Debug, Clone)]
pub struct ScancodeTable {
    map: HashMap<u32, KeyCode>,
    /// Codes `>= virtual_base` are virtual and must never be produced here.
    virtual_base: Option<KeyCode>,
    /// Human label for diagnostics ("linux/evdev", "windows", "macos").
    pub platform: &'static str,
}

impl ScancodeTable {
    pub fn new(platform: &'static str, virtual_base: Option<KeyCode>) -> Self {
        ScancodeTable { map: HashMap::new(), virtual_base, platform }
    }

    /// Insert a native->canonical mapping. Panics if `canonical` lands in the
    /// reserved virtual band — that is a build-time bug in the table, not a
    /// runtime condition (§3.5).
    pub fn insert(&mut self, native: u32, canonical: KeyCode) -> &mut Self {
        if let Some(vb) = self.virtual_base {
            assert!(
                canonical < vb,
                "normalization table for {} would emit virtual code {canonical} (>= {vb})",
                self.platform
            );
        }
        self.map.insert(native, canonical);
        self
    }

    /// Convenience: map a contiguous identity run native==canonical for native in
    /// `lo..=hi`. Used where a platform's numbering coincides with canonical.
    pub fn identity_run(&mut self, lo: KeyCode, hi: KeyCode) -> &mut Self {
        for c in lo..=hi {
            self.insert(c as u32, c);
        }
        self
    }
}

impl ScancodeMap for ScancodeTable {
    fn to_canonical(&self, native: u32) -> Option<KeyCode> {
        self.map.get(&native).copied()
    }
}

// ===========================================================================
// Linux / evdev (input-event-codes.h). This is the TESTED path here.
//
// For the main block, evdev codes coincide with the classic set-1 numbering the
// canonical chart uses (KEY_ESC=1 .. KEY_KPDOT=83, KEY_F11=87, KEY_F12=88), so a
// large identity run is correct. The right-hand and navigation keys diverge and
// are remapped explicitly to the canonical codes CLD4R.KBD expects (e.g. right
// Ctrl -> 99, right Alt -> 98, the second trigger codes in CTRL=29,99 / ALT=56,98).
// ===========================================================================
pub fn linux_table(virtual_base: Option<KeyCode>) -> ScancodeTable {
    let mut t = ScancodeTable::new("linux/evdev", virtual_base);
    // Main block + numpad: evdev == canonical for 1..=83 (and 87, 88).
    t.identity_run(1, 83);
    t.insert(87, 87) // KEY_F11
        .insert(88, 88); // KEY_F12
    // Right-hand modifiers and nav cluster -> canonical chart codes.
    // evdev names on the left, canonical (CLD4R) codes on the right.
    t.insert(96, 104) // KEY_KPENTER   -> 104 (numpad Enter)
        .insert(97, 99) // KEY_RIGHTCTRL -> 99  (CTRL second trigger)
        .insert(98, 94) // KEY_KPSLASH   -> 94  (numpad /)
        .insert(100, 98) // KEY_RIGHTALT  -> 98  (ALT second trigger)
        // Gray nav cluster -> XyWrite classic 91-104 (distinct from the numpad 71-83).
        .insert(110, 91) // KEY_INSERT    -> 91
        .insert(102, 92) // KEY_HOME      -> 92
        .insert(104, 93) // KEY_PAGEUP    -> 93
        .insert(111, 95) // KEY_DELETE    -> 95
        .insert(107, 96) // KEY_END       -> 96
        .insert(109, 97) // KEY_PAGEDOWN  -> 97
        .insert(103, 100) // KEY_UP        -> 100
        .insert(105, 101) // KEY_LEFT      -> 101
        .insert(108, 102) // KEY_DOWN      -> 102
        .insert(106, 103); // KEY_RIGHT     -> 103
    t
}

// ===========================================================================
// Windows. WM_KEYDOWN/raw input gives a set-1 scancode plus an extended (E0)
// flag for right Ctrl/Alt, the nav cluster, and numpad Enter/slash. We model the
// native code as the set-1 scancode, OR'd with 0x100 when the E0 flag is set, so
// the same physical "Enter" and "numpad Enter" map to distinct natives.
// Partial but faithful for the §12 offenders; the rest is chart work (§3.4).
// ===========================================================================
pub const WIN_E0: u32 = 0x100;

pub fn windows_table(virtual_base: Option<KeyCode>) -> ScancodeTable {
    let mut t = ScancodeTable::new("windows", virtual_base);
    // Non-extended set-1 scancodes coincide with canonical for the main block.
    t.identity_run(1, 88);
    // Observed quirk (Win11, CLD hardware, 2026-06): right Shift arrives with the
    // E0 flag set (native 0x36|E0 = 310) even though its base scancode 0x36 is
    // already unambiguous. Map the extended form back to 54 so it isn't treated
    // as an unmapped key. (Left Shift 0x2A=42 arrives non-extended, as expected.)
    t.insert(0x36 | WIN_E0, 54); // right Shift (extended form) -> 54
    // Extended (E0-prefixed) keys -> the XyWrite classic chart codes (the gray nav cluster is
    // DISTINCT from the numpad: numpad 7/8/9… keep 71-83 via the identity run above; the gray
    // Home/arrows/etc. get 91-104). See xy-kbd.txt.
    t.insert(0x1D | WIN_E0, 99) // right Ctrl  -> 99
        .insert(0x38 | WIN_E0, 98) // right Alt   -> 98
        .insert(0x1C | WIN_E0, 104) // numpad Enter-> 104 (distinct from main Enter=28)
        .insert(0x35 | WIN_E0, 94) // numpad /    -> 94
        .insert(0x52 | WIN_E0, 91) // Insert      -> 91
        .insert(0x47 | WIN_E0, 92) // Home        -> 92
        .insert(0x49 | WIN_E0, 93) // PageUp      -> 93
        .insert(0x53 | WIN_E0, 95) // Delete      -> 95
        .insert(0x4F | WIN_E0, 96) // End         -> 96
        .insert(0x51 | WIN_E0, 97) // PageDown    -> 97
        .insert(0x48 | WIN_E0, 100) // Up          -> 100
        .insert(0x4B | WIN_E0, 101) // Left        -> 101
        .insert(0x50 | WIN_E0, 102) // Down        -> 102
        .insert(0x4D | WIN_E0, 103); // Right       -> 103
    t
}

// ===========================================================================
// macOS. kVK_* virtual keycodes are a completely different numbering, and the OS
// may intercept Caps Lock before clean up/down events surface (§7) — the single
// highest-risk item, which is why the experiment targets macOS first. Mapped
// here for the offenders (Caps, F-keys, numpad incl. Numpad 5) plus the alpha
// and number rows so the diagnostic is meaningful; rest is chart work (§3.4).
// ===========================================================================
pub fn macos_table(virtual_base: Option<KeyCode>) -> ScancodeTable {
    let mut t = ScancodeTable::new("macos", virtual_base);
    // kVK_ANSI_* -> canonical. (Hex on the left = Apple virtual keycode.)
    let pairs: &[(u32, KeyCode)] = &[
        // Number row
        (0x12, 2), (0x13, 3), (0x14, 4), (0x15, 5), (0x17, 6),
        (0x16, 7), (0x1A, 8), (0x1C, 9), (0x19, 10), (0x1D, 11),
        (0x1B, 12), (0x18, 13),
        // QWERTY row
        (0x0C, 16), (0x0D, 17), (0x0E, 18), (0x0F, 19), (0x11, 20),
        (0x10, 21), (0x20, 22), (0x22, 23), (0x1F, 24), (0x23, 25),
        // Home row
        (0x00, 30), (0x01, 31), (0x02, 32), (0x03, 33), (0x05, 34),
        (0x04, 35), (0x26, 36), (0x28, 37), (0x25, 38),
        // Bottom row
        (0x06, 44), (0x07, 45), (0x08, 46), (0x09, 47), (0x0B, 48),
        (0x2D, 49), (0x2E, 50),
        // Offenders: modifiers + Caps + space + function/numpad
        (0x39, 58), // kVK_CapsLock      -> 58 (CAPS, T:C)
        (0x38, 42), // kVK_Shift         -> 42
        (0x3C, 54), // kVK_RightShift    -> 54
        (0x3A, 56), // kVK_Option        -> 56 (ALT)
        (0x3D, 98), // kVK_RightOption   -> 98
        (0x3B, 29), // kVK_Control       -> 29 (CTRL)
        (0x3E, 99), // kVK_RightControl  -> 99
        (0x31, 57), // kVK_Space         -> 57
        (0x53, 76), // kVK_ANSI_Keypad5  -> 76 (HOTKEY: the load-bearing key)
        (0x7A, 59), // kVK_F1            -> 59
        (0x78, 60), // kVK_F2            -> 60
        (0x63, 61), // kVK_F3            -> 61
        (0x76, 62), // kVK_F4            -> 62
        (0x60, 63), // kVK_F5            -> 63
        (0x61, 64), // kVK_F6            -> 64
        (0x62, 65), // kVK_F7            -> 65
        (0x64, 66), // kVK_F8            -> 66
        (0x65, 67), // kVK_F9            -> 67
        (0x6D, 68), // kVK_F10           -> 68
        // Numpad Enter / slash + the gray nav cluster -> XyWrite classic 91-104.
        (0x4C, 104), // kVK_ANSI_KeypadEnter  -> 104
        (0x4B, 94),  // kVK_ANSI_KeypadDivide -> 94
        (0x72, 91),  // kVK_Help/Insert       -> 91
        (0x73, 92),  // kVK_Home              -> 92
        (0x74, 93),  // kVK_PageUp            -> 93
        (0x75, 95),  // kVK_ForwardDelete     -> 95
        (0x77, 96),  // kVK_End               -> 96
        (0x79, 97),  // kVK_PageDown          -> 97
        (0x7E, 100), // kVK_UpArrow           -> 100
        (0x7B, 101), // kVK_LeftArrow         -> 101
        (0x7D, 102), // kVK_DownArrow         -> 102
        (0x7C, 103), // kVK_RightArrow        -> 103
    ];
    for &(n, c) in pairs {
        t.insert(n, c);
    }
    t
}

/// Pick the right table for the host platform at runtime.
pub fn host_table(virtual_base: Option<KeyCode>) -> ScancodeTable {
    #[cfg(target_os = "linux")]
    {
        linux_table(virtual_base)
    }
    #[cfg(target_os = "windows")]
    {
        windows_table(virtual_base)
    }
    #[cfg(target_os = "macos")]
    {
        macos_table(virtual_base)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        ScancodeTable::new("unknown", virtual_base)
    }
}
