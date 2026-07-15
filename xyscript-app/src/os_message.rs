//! OS-native message box (`MESSAGE [/T:"title"] <text>`) — a dialog the desktop shows, unlike the
//! in-app `MSG` modal which needs the running GUI. This one works from ANY context, in particular a
//! **headless background (`RUN/BG`) child**, which is why it exists: a background alarm can pop a
//! real box.
//!
//! The box is launched as a **separate, detached process** so it (a) never blocks the caller and
//! (b) SURVIVES the script — a background child typically exits the instant it fires the alarm, and
//! an in-process box would vanish with it. Per platform:
//! - **Windows** re-execs this binary in `--message <text> [--title <title>]` mode, which calls the
//!   native `MessageBoxW` (no console — spawned with `CREATE_NO_WINDOW`). There is no built-in
//!   "msgbox.exe", so re-execing ourselves is the dependency-free way to get a native box in its own
//!   process.
//! - **macOS** spawns `osascript` (`display dialog …`).
//! - **Linux/BSD** tries `zenity`, then `kdialog`, then `xmessage`, then `notify-send` — the first
//!   one present wins.
//!
//! No external crates: Windows uses a tiny `user32` FFI, the rest shell out to the desktop's own
//! dialog tool (directly via `std::process`, NOT the trust-gated `X` verb — so `MESSAGE` needs no
//! `TRUST`).

/// The default window caption when `MESSAGE` is given no `/T:"title"`.
const DEFAULT_TITLE: &str = "XyScript";

/// Fire off an OS message box showing `text` under `title` (`None` = the default app name), detached:
/// non-blocking, and it outlives this process.
pub fn show(title: Option<&str>, text: &str) {
    let title = title.unwrap_or(DEFAULT_TITLE);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .arg("--message")
                .arg(text)
                .arg("--title")
                .arg(title)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    {
        spawn_desktop_dialog(title, text);
    }
}

/// The `--message <text> [--title <title>]` re-exec mode: show a native box and return an exit code,
/// or `None` when `--message` isn't in `args` (the caller then proceeds with normal startup). Wired
/// first-thing in both binaries' `main()`, alongside `boot::run_batch`. Only Windows spawns this
/// mode; on other platforms `show` calls the desktop tool directly, but the mode stays functional as
/// a fallback.
pub fn maybe_render(args: &[String]) -> Option<i32> {
    let arg_after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let text = arg_after("--message").or_else(|| arg_after("-message"))?;
    let title = arg_after("--title").or_else(|| arg_after("-title"));
    render(title.as_deref().unwrap_or(DEFAULT_TITLE), &text);
    Some(0)
}

#[cfg(windows)]
fn render(title: &str, text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            u_type: u32,
        ) -> i32;
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONINFORMATION: u32 = 0x0000_0040;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    const MB_TOPMOST: u32 = 0x0004_0000;
    let (t, c) = (wide(text), wide(title));
    // Safety: valid NUL-terminated UTF-16 pointers, null owner window; MessageBoxW runs its own
    // modal loop on this (dedicated `--message`) process and returns when the user clicks OK.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            t.as_ptr(),
            c.as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(not(windows))]
fn render(title: &str, text: &str) {
    spawn_desktop_dialog(title, text);
}

/// macOS / Linux: launch the desktop's own dialog program (a separate, surviving process).
#[cfg(not(windows))]
fn spawn_desktop_dialog(title: &str, text: &str) {
    use std::process::Command;
    #[cfg(target_os = "macos")]
    {
        // AppleScript string-quote `text` and the title, then `display dialog`.
        let script = format!(
            "display dialog {} with title {} buttons {{\"OK\"}} default button \"OK\" with icon note",
            applescript_quote(text),
            applescript_quote(title),
        );
        let _ = Command::new("osascript").arg("-e").arg(script).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Try the common Linux/BSD dialog tools in order; each takes its text as a plain argument
        // (no shell), so nothing needs escaping. First one that spawns wins.
        let attempts: [(&str, Vec<String>); 4] = [
            (
                "zenity",
                vec![
                    "--info".into(),
                    format!("--title={title}"),
                    format!("--text={text}"),
                ],
            ),
            (
                "kdialog",
                vec![
                    "--title".into(),
                    title.into(),
                    "--msgbox".into(),
                    text.into(),
                ],
            ),
            ("xmessage", vec!["-center".into(), text.into()]),
            ("notify-send", vec![title.into(), text.into()]),
        ];
        for (bin, args) in attempts {
            if Command::new(bin).args(&args).spawn().is_ok() {
                break;
            }
        }
    }
}

/// Wrap `s` as an AppleScript string literal (escape `\` and `"`).
#[cfg(target_os = "macos")]
fn applescript_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '\\' || ch == '"' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_render_ignores_absent_flag() {
        // No `--message` → None, so the caller boots normally.
        assert_eq!(maybe_render(&["notes.md".to_string()]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_quote_escapes_specials() {
        assert_eq!(applescript_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
