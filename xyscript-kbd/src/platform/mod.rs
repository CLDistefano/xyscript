//! Per-OS interception layers (§7).
//!
//! Each platform provides one function, `run_capture`, that installs a
//! **non-exclusive** keyboard hook and calls a handler for every native event.
//! The handler returns `Capture::Consume` or `Capture::PassThrough`; the hook
//! honors it so `KF` (§6.1) can release specific keys to the OS. Non-exclusive
//! capture is a hard requirement on every platform (§7 "Capture must be
//! non-exclusive"): an exclusive grab would make `KF` impossible.
//!
//! Only the module matching the host target is compiled, so the OS-specific
//! crates never need to build on the other platforms.

use crate::event::KeyPhase;
use crate::pipeline::Capture;

/// A native event as the hook sees it, before normalization.
#[derive(Debug, Clone, Copy)]
pub struct RawEvent {
    /// Platform-native code (set-1 scancode [| WIN_E0] on Windows, evdev code on
    /// Linux, kVK_* on macOS).
    pub native: u32,
    pub phase: KeyPhase,
}

/// The handler the capture loop invokes per event. It returns the capture
/// decision; the platform hook must obey it (consume vs let-through).
pub type Handler<'a> = dyn FnMut(RawEvent) -> Capture + 'a;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::run_capture;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::run_capture;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::run_capture;

/// Fallback so the crate builds (the binary's run/diag modes just report) on
/// platforms without a hook implementation.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn run_capture(_handler: &mut Handler<'_>) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no keyboard-capture backend for this platform",
    ))
}
