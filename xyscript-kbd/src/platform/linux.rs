//! Linux interception via evdev (§7).
//!
//! We read `/dev/input/event*` **without** `EVIOCGRAB`, i.e. non-exclusively —
//! exactly what §7 requires so `KF` can let keys reach the OS. Reading without a
//! grab lets us *observe* clean, distinguishable down/up events for every key,
//! including the historically-difficult ones (Caps Lock, function keys, the
//! numpad incl. Numpad 5). That observation IS the §12 experiment on Linux.
//!
//! Note on consumption: a non-grabbing read observes events but cannot *swallow*
//! them — the OS still delivers them too. Selective swallow on Linux therefore
//! needs a grab (`device.grab()`) plus re-injection of the pass-through keys via
//! a `uinput` virtual device. That is the Linux form of "non-exclusive" capture:
//! grab the real device, and re-emit only the `KF` keys. This prototype
//! implements the observe path (sufficient to prove §12); the grab+uinput
//! consumption path is called out in the README as the remaining Linux work.
//!
//! Requires read access to `/dev/input/event*` (run as root or in the `input`
//! group).

use super::{Handler, RawEvent};
use crate::event::KeyPhase;
use std::sync::mpsc;
use std::thread;

/// Open every keyboard-like device, fan their events into one channel, and call
/// `handler` for each. Blocks until all sources end.
pub fn run_capture(handler: &mut Handler<'_>) -> std::io::Result<()> {
    let devices = enumerate_keyboards()?;
    if devices.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no keyboard devices found under /dev/input (need read access)",
        ));
    }

    let (tx, rx) = mpsc::channel::<RawEvent>();
    for mut dev in devices {
        let tx = tx.clone();
        thread::spawn(move || {
            // NB: no dev.grab() — non-exclusive observe (§7).
            loop {
                match dev.fetch_events() {
                    Ok(events) => {
                        for ev in events {
                            if ev.event_type() == evdev::EventType::KEY {
                                let phase = match ev.value() {
                                    1 => KeyPhase::Down,
                                    0 => KeyPhase::Up,
                                    _ => continue, // 2 = auto-repeat: ignore
                                };
                                let raw = RawEvent { native: ev.code() as u32, phase };
                                if tx.send(raw).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(_) => return,
                }
            }
        });
    }
    drop(tx); // so rx ends when every device thread exits

    for raw in rx {
        let _capture = handler(raw);
        // In the observe path the capture decision is logged by the handler but
        // not enforced (see module note). Wire grab+uinput here to enforce it.
    }
    Ok(())
}

/// Devices that advertise KEY_A are treated as keyboards.
fn enumerate_keyboards() -> std::io::Result<Vec<evdev::Device>> {
    let mut out = Vec::new();
    for (_path, dev) in evdev::enumerate() {
        let is_keyboard = dev
            .supported_keys()
            .map(|keys| keys.contains(evdev::Key::KEY_A))
            .unwrap_or(false);
        if is_keyboard {
            out.push(dev);
        }
    }
    Ok(out)
}
