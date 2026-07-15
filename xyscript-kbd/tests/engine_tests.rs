//! Headless engine tests (§10.1: the layer is fully testable with no GUI and no
//! OS hooks by asserting that a given `(bitmask, key_code)` resolves to the
//! expected assignment). These mirror verify/oracle.py and add runtime-state and
//! loader coverage. Run with `cargo test`.

use std::path::PathBuf;
use xyscript_kbd::event::{KeyEvent};
use xyscript_kbd::inject::{Access, InjectError, InjectionChannel};
use xyscript_kbd::model::{Assignment, ModBehavior};
use xyscript_kbd::parser;
use xyscript_kbd::resolver::{Disposition, Resolver};
use xyscript_kbd::{embedded_default, event::EventSource, load, LoadOutcome};

fn cld4r() -> String {
    // The XyScript-native (UTF-8) CLD4R.KBD fixture lives at the crate root, so
    // the crate is self-contained for third-party testers. The original XyWrite
    // binary is reference-only at reference/CLD4R.xyw.KBD and is not parsed here.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("CLD4R.KBD");
    std::fs::read_to_string(&p).expect("read CLD4R.KBD")
}

fn parse_cld4r() -> xyscript_kbd::Keymap {
    parser::parse(&cld4r(), None).expect("CLD4R.KBD parses")
}

fn modifier(km: &xyscript_kbd::Keymap, name: &str) -> xyscript_kbd::ModifierDef {
    km.modifiers
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or_else(|| panic!("modifier {name} not found"))
}

// ----- §2: bottom-up bit assignment matches the design's worked example ------
#[test]
fn bit_assignment_matches_spec() {
    let km = parse_cld4r();
    assert_eq!(modifier(&km, "CAPS").bit, 4096);
    assert_eq!(modifier(&km, "SHIFT").bit, 2048);
    assert_eq!(modifier(&km, "ALT").bit, 1024);
    assert_eq!(modifier(&km, "CTRL").bit, 512);
    assert_eq!(modifier(&km, "HOTKEY").bit, 256);
}

// ----- §3.1: MOUSE is declared but, being unreferenced, is not a modifier ----
#[test]
fn mouse_is_not_a_modifier() {
    let km = parse_cld4r();
    assert!(km.modifiers.iter().all(|m| !m.name.eq_ignore_ascii_case("MOUSE")));
}

// ----- §3.1/§4: multi-trigger Shift and the Caps toggle attribute ------------
#[test]
fn shift_has_two_triggers_caps_is_toggle() {
    let km = parse_cld4r();
    assert_eq!(modifier(&km, "SHIFT").trigger_keys, vec![42, 54]);
    assert_eq!(modifier(&km, "CAPS").behavior, ModBehavior::Toggle);
    assert_eq!(modifier(&km, "SHIFT").behavior, ModBehavior::Hold);
}

// ----- §6: strict lookup of representative cells across layers ---------------
#[test]
fn strict_lookup_across_layers() {
    let km = parse_cld4r();
    assert_eq!(km.resolve(0, 16), Some(&Assignment::Xpl("q".into())));
    assert_eq!(km.resolve(2048, 16), Some(&Assignment::Xpl("Q".into())));
    assert_eq!(km.resolve(4096, 16), Some(&Assignment::Xpl("Q".into())));
    assert_eq!(km.resolve(2048 | 4096, 16), Some(&Assignment::Xpl("q".into())));
}

// ----- §3.2: one TABLE serving two bitmasks (ALT+CAPS,ALT) -------------------
#[test]
fn one_table_serves_two_bitmasks() {
    let km = parse_cld4r();
    let a = km.resolve(1024, 2);
    let b = km.resolve(1024 | 4096, 2);
    assert!(a.is_some() && b.is_some());
    assert_eq!(a, b);
}

// ----- §6: an entirely absent combination misses for every key --------------
#[test]
fn absent_combination_is_inert() {
    let km = parse_cld4r();
    let full = 512 | 1024 | 2048 | 4096; // CTRL+ALT+SHIFT+CAPS — not in the file
    for k in 1u8..=104 {
        assert_eq!(km.resolve(full, k), None, "code {k} should miss");
    }
}

// ----- §5: HOTKEY (Numpad 5 = 76) is dual-role and has a tap cell ------------
#[test]
fn hotkey_is_dual_role() {
    let km = parse_cld4r();
    assert!(modifier(&km, "HOTKEY").dual_role);
    assert!(km.resolve(0, 76).is_some());
}

// ----- §5: tap-vs-hold state machine — tap fires, chord suppresses -----------
#[test]
fn dual_role_tap_then_chord() {
    // Build a tiny purpose-made map: HOTKEY=76 (hold, dual-role via tap cell),
    // unshifted 76 -> "TAP", HOTKEY layer 16 -> "CHORD16", unshifted 16 -> "q".
    let src = "\
KEYS=104
HOTKEY=76
TABLE=
16=q
76=TAP
TABLE=HOTKEY
16=CHORD16
";
    let km = parser::parse(src, None).unwrap();
    assert!(modifier(&km, "HOTKEY").dual_role);
    let mut r = Resolver::new(km);

    // Tap alone: down (swallow), up (fires TAP).
    assert_eq!(r.resolve(KeyEvent::down(76)), Disposition::Swallow);
    assert_eq!(r.resolve(KeyEvent::up(76)), Disposition::Dispatch("TAP".into()));

    // Chord: hold 76, press 16 (resolves in HOTKEY layer), release 76 (suppressed).
    assert_eq!(r.resolve(KeyEvent::down(76)), Disposition::Swallow);
    assert_eq!(r.resolve(KeyEvent::down(16)), Disposition::Dispatch("CHORD16".into()));
    let _ = r.resolve(KeyEvent::up(16));
    assert_eq!(r.resolve(KeyEvent::up(76)), Disposition::Swallow, "tap suppressed after chord");
}

// ----- §4: toggle (Caps) latches and unlatches on successive taps -----------
#[test]
fn toggle_latches() {
    let src = "\
KEYS=104
CAPS=58,T:C
TABLE=
16=q
TABLE=CAPS
16=Q
";
    let km = parser::parse(src, None).unwrap();
    let mut r = Resolver::new(km);
    // Before caps: lowercase.
    assert_eq!(r.resolve(KeyEvent::down(16)), Disposition::Dispatch("q".into()));
    let _ = r.resolve(KeyEvent::up(16));
    // Tap caps (down flips latched; up swallow).
    let _ = r.resolve(KeyEvent::down(58));
    let _ = r.resolve(KeyEvent::up(58));
    assert_eq!(r.resolve(KeyEvent::down(16)), Disposition::Dispatch("Q".into()));
    let _ = r.resolve(KeyEvent::up(16));
    // Tap caps again -> back to lowercase.
    let _ = r.resolve(KeyEvent::down(58));
    let _ = r.resolve(KeyEvent::up(58));
    assert_eq!(r.resolve(KeyEvent::down(16)), Disposition::Dispatch("q".into()));
}

// ----- §6.1: KF flips, miss swallows, ordinary dispatches -------------------
#[test]
fn kf_flip_disposition() {
    let src = "\
KEYS=104
TABLE=
1=KF
16=q
";
    let km = parser::parse(src, None).unwrap();
    let mut r = Resolver::new(km);
    assert_eq!(r.resolve(KeyEvent::down(1)), Disposition::Flip);
    assert_eq!(r.resolve(KeyEvent::down(16)), Disposition::Dispatch("q".into()));
    assert_eq!(r.resolve(KeyEvent::down(99)), Disposition::Swallow); // no cell
}

// ----- §9.2/§9.3: transactional load — a bad file is rejected, good installs -
#[test]
fn transactional_load() {
    // Bad: references an undeclared modifier.
    let bad = "KEYS=104\nTABLE=NOPE\n16=x\n";
    assert!(matches!(load(bad, None), LoadOutcome::Rejected(_)));

    // Bad: key code out of range.
    let oob = "KEYS=10\nTABLE=\n50=x\n";
    assert!(matches!(load(oob, None), LoadOutcome::Rejected(_)));

    // Good.
    let good = "KEYS=104\nTABLE=\n16=q\n";
    assert!(matches!(load(good, None), LoadOutcome::Installed(_)));
}

// ----- §9.1: embedded default is self-sufficient — can reach LOADKBD ---------
#[test]
fn embedded_default_reaches_loadkbd() {
    let km = embedded_default();
    let reaches = km
        .tables
        .values()
        .any(|a| matches!(a, Assignment::Xpl(s) if s.eq_ignore_ascii_case("LOADKBD")));
    assert!(reaches, "default must bind a path to LOADKBD (§9.1)");
    // And it KFs a baseline of OS keys (§6.1).
    let kf_count = km.tables.values().filter(|a| matches!(a, Assignment::Flip)).count();
    assert!(kf_count >= 1, "default should KF some OS keys (§6.1)");
}

// ----- §6/§6.1: the OS baseline is KF in EVERY reachable layer ---------------
// Strict lookup has no fallthrough, so a KF that exists only in layer 0 would
// leave the user with no OS escape while a modifier is held. Assert the default
// replicates Esc/Backspace/Tab across all 2^n modifier subsets.
#[test]
fn embedded_default_baseline_is_in_every_layer() {
    let km = embedded_default();
    // Build all subsets of the modifier bits.
    let bits: Vec<u32> = km.modifiers.iter().map(|m| m.bit).collect();
    let mut masks = vec![0u32];
    for b in bits {
        let mut more = masks.clone();
        for m in more.iter_mut() {
            *m |= b;
        }
        masks.extend(more);
    }
    for mask in masks {
        for k in [1u8, 14, 15] {
            assert_eq!(
                km.resolve(mask, k),
                Some(&Assignment::Flip),
                "OS baseline key {k} must KF in layer bitmask {mask}"
            );
        }
    }
}

// ----- §3.5: injection is disabled by default and access-controlled ---------
#[test]
fn injection_is_secure_by_default() {
    let mut ch = InjectionChannel::new(Some(120));
    // Disabled by default.
    assert_eq!(ch.poke(130, 0, Some("tok")), Err(InjectError::Disabled));
    // Enable with a token; wrong/absent token is unauthorized.
    ch.enable(Access::Token("secret".into()));
    assert_eq!(ch.poke(130, 0, None), Err(InjectError::Unauthorized));
    assert_eq!(ch.poke(130, 0, Some("wrong")), Err(InjectError::Unauthorized));
    // A physical (non-virtual) code is rejected even when authorized.
    assert_eq!(ch.poke(16, 0, Some("secret")), Err(InjectError::NotVirtual));
    // A virtual code with the right token is accepted and yields an event.
    assert!(ch.poke(130, 0, Some("secret")).is_ok());
    assert!(ch.next_event().is_some());
}

// ----- §3.5: an injected poke carries its own bitmask, ignores physical state
#[test]
fn injection_is_deterministic() {
    let src = "KEYS=140\nSHIFT=42\nTABLE=\n130=plain\nTABLE=SHIFT\n130=shifted\n";
    let km = parser::parse(src, Some(128)).unwrap();
    // SHIFT is the only modifier here, so bottom-up assignment gives it bit 256.
    let shift_bit = modifier(&km, "SHIFT").bit;
    assert_eq!(shift_bit, 256);
    let mut r = Resolver::new(km);
    // Physically hold Shift.
    let _ = r.resolve(KeyEvent::down(42));
    // A poke carrying bitmask 0 must resolve in the UNSHIFTED layer regardless,
    // because injected events do not borrow live physical modifier state.
    assert_eq!(r.resolve(KeyEvent::inject(130, 0)), Disposition::Dispatch("plain".into()));
    // A poke carrying the SHIFT bit resolves in the shifted layer.
    assert_eq!(r.resolve(KeyEvent::inject(130, shift_bit)), Disposition::Dispatch("shifted".into()));
}
