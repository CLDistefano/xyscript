//! Windows interception via a low-level keyboard hook, `WH_KEYBOARD_LL` (§7).
//!
//! `WH_KEYBOARD_LL` is non-exclusive: the hook proc decides per event whether to
//! swallow it (return a nonzero `LRESULT`) or release it to the OS (call
//! `CallNextHookEx`). That per-event choice is exactly what `KF` needs (§6.1) and
//! why an exclusive grab is the wrong model on Windows.
//!
//! Extended keys (right Ctrl/Alt, the nav cluster, numpad Enter/slash) arrive
//! with `LLKHF_EXTENDED` set; we fold that into the native code as `WIN_E0` so
//! the normalization table can distinguish them (see normalize::windows_table).
//!
//! Untested in this sandbox (no Windows host); the API calls below are the real
//! `windows` crate surface and are gated to `target_os = "windows"`.

use super::{Handler, RawEvent};
use crate::event::KeyPhase;
use crate::normalize::WIN_E0;
use crate::pipeline::Capture;
use std::cell::Cell;
use std::ffi::c_void;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

thread_local! {
    // A THIN pointer (`*mut c_void`) to the caller's `&mut Handler`, valid for the
    // duration of run_capture. The hook proc is a global C callback and cannot
    // capture environment, so we route through this thread-local. We cannot store
    // a `*mut Handler` directly: `Handler` is `dyn FnMut`, so `*mut Handler` is a
    // *fat* pointer and `null_mut()` only builds *thin* nulls. Storing a thin
    // pointer to the (sized) reference sidesteps that.
    static HANDLER: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Negative code => must pass to the next hook without processing.
    if code < 0 {
        return CallNextHookEx(HHOOK(std::ptr::null_mut()), code, wparam, lparam);
    }
    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let phase = match wparam.0 as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(KeyPhase::Down),
        WM_KEYUP | WM_SYSKEYUP => Some(KeyPhase::Up),
        _ => None,
    };

    if let Some(phase) = phase {
        let extended = (kb.flags.0 & LLKHF_EXTENDED.0) != 0;
        let native = kb.scanCode | if extended { WIN_E0 } else { 0 };
        let raw = RawEvent { native, phase };

        let pp = HANDLER.with(|h| h.get()) as *mut &mut Handler<'static>;
        if !pp.is_null() {
            // pp points to a `&mut Handler`; reborrow through it to call the FnMut.
            let handler: &mut Handler<'static> = &mut **pp;
            if handler(raw) == Capture::Consume {
                // Nonzero return swallows the keystroke (the OS never sees it).
                return LRESULT(1);
            }
        }
    }
    // PassThrough (KF) or a non-key message: hand it on for normal handling.
    CallNextHookEx(HHOOK(std::ptr::null_mut()), code, wparam, lparam)
}

/// Install the hook and pump messages until the thread is told to quit. The
/// message loop is mandatory: `WH_KEYBOARD_LL` is only serviced while the
/// installing thread dispatches messages.
pub fn run_capture(handler: &mut Handler<'_>) -> std::io::Result<()> {
    // Store a thin pointer to the `&mut Handler` (not to the unsized `Handler`).
    // The pointer is cleared before run_capture returns, so it never outlives
    // `handler`; the lifetime is erased through `*mut c_void` and reconstructed as
    // `'static` in the hook proc (sound because the hook only runs while we are
    // inside this function and the message loop below).
    let mut handler = handler;
    let pp: *mut &mut Handler<'_> = &mut handler;
    HANDLER.with(|h| h.set(pp as *mut c_void));

    unsafe {
        // For a low-level hook whose proc lives in the current process, `hMod` is NULL
        // (the proc is not in a separate DLL). Passing the EXE module handle can stop the
        // OS from delivering events on some systems.
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut msg = MSG::default();
        // GetMessageW returns 0 on WM_QUIT, -1 on error, else >0.
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(hook);
    }

    HANDLER.with(|h| h.set(std::ptr::null_mut()));
    Ok(())
}
