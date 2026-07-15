//! macOS interception via a `CGEventTap` (§7).
//!
//! A `CGEventTap` at session level with `Default` options is non-exclusive: the
//! callback returns the event to pass it through, or `None` to swallow it — the
//! per-event choice `KF` needs (§6.1).
//!
//! macOS is the project's highest-risk platform (§7, §12): modifier and Caps
//! Lock changes arrive as `FlagsChanged` events rather than KeyDown/KeyUp, and
//! the OS may intercept Caps before clean edges are visible. We turn
//! `FlagsChanged` into clean per-key down/up by tracking which modifier keycodes
//! are currently down — this is precisely the §12 thing to validate on real
//! hardware: that Caps Lock, the function keys, and Numpad 5 deliver clean,
//! distinguishable edges before the runtime ever sees them.
//!
//! Requires Accessibility (Input Monitoring) permission. Untested in this
//! sandbox (no macOS host); the calls are the real core-graphics surface, gated
//! to `target_os = "macos"`.

use super::{Handler, RawEvent};
use crate::event::KeyPhase;
use crate::pipeline::Capture;
use std::cell::RefCell;
use std::collections::HashSet;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    EventField,
};

pub fn run_capture(handler: &mut Handler<'_>) -> std::io::Result<()> {
    // FlagsChanged carries no phase, so we derive down/up by remembering which
    // modifier keycodes are currently down.
    let mods_down: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    let handler = RefCell::new(handler);

    let events = vec![
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default, // active tap: may swallow (KF needs this)
        events,
        |_proxy, etype, event| {
            let keycode =
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u32;

            let phase = match etype {
                CGEventType::KeyDown => Some(KeyPhase::Down),
                CGEventType::KeyUp => Some(KeyPhase::Up),
                CGEventType::FlagsChanged => {
                    // Toggle our remembered down-state for this modifier keycode.
                    let mut set = mods_down.borrow_mut();
                    if set.contains(&keycode) {
                        set.remove(&keycode);
                        Some(KeyPhase::Up)
                    } else {
                        set.insert(keycode);
                        Some(KeyPhase::Down)
                    }
                }
                _ => None,
            };

            if let Some(phase) = phase {
                let raw = RawEvent { native: keycode, phase };
                let decision = (handler.borrow_mut())(raw);
                if decision == Capture::Consume {
                    return None; // swallow: the OS never sees it
                }
            }
            // PassThrough (KF) or uninteresting: return the event unchanged.
            Some(event.clone())
        },
    )
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "failed to create CGEventTap (grant Input Monitoring / Accessibility)",
        )
    })?;

    let loop_source = tap
        .mach_port
        .create_runloop_source(0)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "runloop source failed"))?;

    unsafe {
        CFRunLoop::get_current().add_source(&loop_source, kCFRunLoopCommonModes);
    }
    tap.enable();
    CFRunLoop::run_current(); // blocks until the run loop is stopped
    Ok(())
}
