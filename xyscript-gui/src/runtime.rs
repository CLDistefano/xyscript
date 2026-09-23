//! The native runtime: we own the winit event loop (egui-winit + egui-wgpu over our
//! `GuiApp`) instead of going through eframe. This is what lets us intercept raw
//! `WindowEvent::KeyboardInput` and run it through the keymap *before* egui sees it —
//! eframe hid winit's native key events, and a global `WH_KEYBOARD_LL` hook does not fire
//! inside this process. Assigned command keys (function keys, or any Ctrl/Alt/Hotkey chord)
//! are consumed and dispatched as XPL; everything else (typing, IME, Enter, arrows, …) is
//! forwarded to egui exactly as before.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

use xyscript_kbd::event::KeyEvent as KbdKey;
use xyscript_kbd::ModifierDef;
use xyscript_kbd::normalize::{host_table, ScancodeMap, ScancodeTable, WIN_E0};
use xyscript_kbd::{Disposition, Keymap, Resolver};

use crate::GuiApp;

/// Canonical codes for F1–F12 (set-1 scancodes; F11/F12 = 87/88). Always-capture (no
/// modifier required) because they never carry text.
const FUNCTION_KEYS: &[u8] = &[59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88];

/// Tab (set-1 scancode 15). Captured when assigned so the keymap can make it insert a tab
/// rather than letting egui consume it for widget-focus navigation. It carries no text/IME.
const TAB: u8 = 15;

/// Numpad Enter (canonical/classic code 104; extended `0x1C|E0` — see `normalize.rs`). Always-
/// capture so the keymap can repurpose it; otherwise egui treats it as ordinary Enter (cmline
/// submit / newline) and the keymap cell is never reached. The MAIN Enter (28) is left to egui.
const NUMPAD_ENTER: u8 = 104;

/// A readable description of a captured key for `READKEY/D`: modifier prefixes plus the key name —
/// e.g. `CTRL+Y`, `SHIFT+F3`, `HOTKEY+A`, `Enter`, `A`. The modifier NAMES and BITS come from the
/// loaded keymap's `ModifierDef`s (or the internal default when no KBD is loaded) — so a custom KBD's
/// bit assignments and user-defined modifiers (e.g. `HOTKEY`) are honoured. Ordered by bit value.
fn describe_key(logical: &Key, mods: i64, modifiers: &[ModifierDef]) -> String {
    let mut held: Vec<&ModifierDef> =
        modifiers.iter().filter(|m| mods & i64::from(m.bit) != 0).collect();
    held.sort_by_key(|m| m.bit);
    let mut s = String::new();
    for m in held {
        s.push_str(&m.name);
        s.push('+');
    }
    s.push_str(&key_name(logical));
    s
}

/// The label for a captured key: a printable key's character (upper-cased, like a physical key cap),
/// or a short name for a named key (`Enter`, `Esc`, `Up`, `F3`, …).
fn key_name(logical: &Key) -> String {
    match logical {
        Key::Character(c) => c.to_uppercase(),
        Key::Named(n) => match n {
            NamedKey::Enter => "Enter".into(),
            NamedKey::Escape => "Esc".into(),
            NamedKey::Tab => "Tab".into(),
            NamedKey::Space => "Space".into(),
            NamedKey::Backspace => "Backspace".into(),
            NamedKey::Delete => "Del".into(),
            NamedKey::Insert => "Ins".into(),
            NamedKey::Home => "Home".into(),
            NamedKey::End => "End".into(),
            NamedKey::PageUp => "PgUp".into(),
            NamedKey::PageDown => "PgDn".into(),
            NamedKey::ArrowUp => "Up".into(),
            NamedKey::ArrowDown => "Down".into(),
            NamedKey::ArrowLeft => "Left".into(),
            NamedKey::ArrowRight => "Right".into(),
            other => format!("{other:?}"), // F1–F12 etc. — Debug is already "F3", "F11", …
        },
        _ => "?".into(),
    }
}

/// Whether a winit LOGICAL key is a pure modifier — checked ahead of the canonical-code list so a
/// `READKEY` ignores a bare modifier press regardless of scancode/keymap remapping (Super/Win, AltGr,
/// and the lock keys aren't in `MODIFIER_CODES`). The keystroke still carries the held modifiers in
/// `$SCC` once a REAL key is pressed.
fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
        )
    )
}

/// The virtual-key band base the GUI parses its keymap with (matches `load(.., Some(120))`).
const VIRTUAL_BASE: u8 = 120;

/// Launch the GUI: create the window, wire egui + wgpu, and run the event loop.
pub fn run(app: GuiApp) {
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut handler = Handler::new(app);
    event_loop.run_app(&mut handler).expect("run event loop");
}

/// The keymap resolver for raw key interception (mirrors the old hook handler, now fed by
/// winit instead of a `WH_KEYBOARD_LL` callback).
struct KeyCap {
    resolver: Resolver,
    table: ScancodeTable,
    /// Codes consumed on their down edge, so the matching up edge is mirror-consumed.
    consumed: HashSet<u8>,
    /// Modifiers that do NOT, on their own, turn an assigned printable into a captured
    /// command (Shift/Caps select a typing layer that must reach OS Text/IME).
    passthrough_mask: u32,
}

impl KeyCap {
    fn new(keymap: Keymap) -> Self {
        let passthrough_mask = {
            let mut m = 0;
            for name in ["SHIFT", "CAPS"] {
                if let Some(b) = keymap.bitmask_for_names(&[name]) {
                    m |= b;
                }
            }
            m
        };
        KeyCap {
            resolver: Resolver::new(keymap),
            table: host_table(Some(VIRTUAL_BASE)),
            consumed: HashSet::new(),
            passthrough_mask,
        }
    }
}

/// Text-like extensions the Open dialog shows by default — the common document/markup, data, and
/// XyScript-own types — with an "All files" entry alongside so anything else stays reachable, and
/// `TEXTFILES` to add more. Kept SHORT ON PURPOSE: Windows 11's file dialog (`IFileDialog`) silently
/// ignores an over-long filter spec (rfd joins these into one `*.ext;*.ext;…` string), which made the
/// "Text files" filter show everything and look identical to "All files" — the filter appeared not to
/// refresh when switched. Additions go through `TEXTFILES`; the combined list is length-capped below.
const TEXT_EXTENSIONS: &[&str] = &[
    // Documents / markup
    "txt", "text", "md", "markdown", "rst", "tex", "typ", "log",
    // XyScript's own files
    "xpl", "kbd", "lib", "cfg",
    // Data / config
    "json", "csv", "tsv", "xml", "yaml", "yml", "toml", "ini",
    // Web / style
    "html", "css",
];

/// Cap on how many extensions the "Text files" filter carries (built-in + `TEXTFILES`), so the joined
/// Windows filter spec stays short enough for `IFileDialog` to apply it. Well under any plausible limit.
const MAX_FILTER_EXTS: usize = 32;

/// A native file dialog running on its own thread (Windows): the channel the picked path arrives on,
/// and the request to apply it to. See [`Handler::about_to_wait`], which starts it non-blocking so the
/// owner-modal handshake can't deadlock (a blocking `join` on the main thread did — it stops pumping the
/// owner window's messages, which the cross-thread modal needs).
struct DialogJob {
    rx: std::sync::mpsc::Receiver<Option<std::path::PathBuf>>,
    req: crate::PendingDialog,
}

/// A parent window handle carried to the dialog thread on Windows. `winit::Window` isn't `Send`, but a
/// bare `HWND` (`isize`) is; this rebuilds the `raw-window-handle` traits `rfd` needs so the dialog is
/// still owner-modal to the main window from another thread.
#[cfg(windows)]
struct ParentWindow {
    hwnd: isize,
}

#[cfg(windows)]
impl winit::raw_window_handle::HasWindowHandle for ParentWindow {
    fn window_handle(
        &self,
    ) -> Result<winit::raw_window_handle::WindowHandle<'_>, winit::raw_window_handle::HandleError> {
        use winit::raw_window_handle::{HandleError, RawWindowHandle, Win32WindowHandle, WindowHandle};
        let hwnd = std::num::NonZeroIsize::new(self.hwnd).ok_or(HandleError::Unavailable)?;
        // SAFETY: the HWND belongs to the main window, which outlives the dialog thread.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd))) })
    }
}

#[cfg(windows)]
impl winit::raw_window_handle::HasDisplayHandle for ParentWindow {
    fn display_handle(
        &self,
    ) -> Result<winit::raw_window_handle::DisplayHandle<'_>, winit::raw_window_handle::HandleError> {
        use winit::raw_window_handle::{DisplayHandle, RawDisplayHandle, WindowsDisplayHandle};
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new())) })
    }
}

/// The main window's raw `HWND`, for parenting a dialog run on another thread.
#[cfg(windows)]
fn main_hwnd(window: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

/// Build + show the dialog for `req` on the Windows dialog thread, owner-modal to `hwnd`. Blocks THIS
/// (worker) thread until the user chooses/cancels; the main thread meanwhile keeps pumping, so the
/// modal handshake completes.
#[cfg(windows)]
fn run_file_dialog_threaded(
    req: &crate::PendingDialog,
    hwnd: Option<isize>,
    default_typst: bool,
) -> Option<std::path::PathBuf> {
    let base = match hwnd {
        Some(h) => rfd::FileDialog::new().set_parent(&ParentWindow { hwnd: h }),
        None => rfd::FileDialog::new(),
    };
    dialog_pick(req, base, default_typst)
}

/// Run a native file dialog for `req` on the CURRENT thread, parented to `window` (owner-modal). Blocks
/// until the user chooses or cancels. Used on non-Windows, where file dialogs MUST run on the main
/// thread; Windows runs [`run_file_dialog_threaded`] on a worker instead (see `about_to_wait`).
#[cfg(not(windows))]
fn run_file_dialog(
    req: &crate::PendingDialog,
    window: &winit::window::Window,
    default_typst: bool,
) -> Option<std::path::PathBuf> {
    let base = rfd::FileDialog::new().set_parent(window);
    dialog_pick(req, base, default_typst)
}

/// Apply `req`'s filters to an already-parented `base` dialog and show it (blocking). Shared by the
/// main-thread path and (on Windows) the dialog worker thread.
fn dialog_pick(
    req: &crate::PendingDialog,
    base: rfd::FileDialog,
    default_typst: bool,
) -> Option<std::path::PathBuf> {
    use crate::PendingDialog;
    match req {
        PendingDialog::Open { hex, dir, extra_exts, read_only: _ } => {
            // Seed the start folder on the first open ($CWD); later opens pass `None` so the OS
            // reopens at its last-used directory.
            let base = if let Some(dir) = dir { base.set_directory(dir) } else { base };
            // The "Text files" filter matches the built-in set PLUS the user's `TEXTFILES` additions,
            // deduped and length-capped so the joined Windows filter spec stays short enough to apply
            // (an over-long spec makes Win11's dialog ignore the filter — see `TEXT_EXTENSIONS`).
            let mut text_exts: Vec<&str> = TEXT_EXTENSIONS.to_vec();
            for e in extra_exts.iter().map(|s| s.as_str()) {
                if !text_exts.contains(&e) {
                    text_exts.push(e);
                }
            }
            text_exts.truncate(MAX_FILTER_EXTS);
            // A `/HEX` open is usually a binary, so lead with the All-files filter there.
            if *hex {
                base.add_filter("All files", &["*"])
                    .add_filter("Text files", &text_exts)
                    .pick_file()
            } else {
                base.add_filter("Text files", &text_exts)
                    .add_filter("All files", &["*"])
                    .pick_file()
            }
        }
        // Save-As offers the authoring formats plus an All-files escape hatch. The FIRST filter is
        // the default (its first ext is appended to a bare name), so lead with the active document's
        // flavor: Typst first for a Typst doc, else Markdown.
        PendingDialog::SaveAs => {
            let d = if default_typst {
                base.add_filter("Typst", &["typ"]).add_filter("Markdown", &["md", "markdown"])
            } else {
                base.add_filter("Markdown", &["md", "markdown"]).add_filter("Typst", &["typ"])
            };
            d.add_filter("Text", &["txt", "text"]).add_filter("All files", &["*"]).save_file()
        }
        PendingDialog::Export => base.add_filter("PDF", &["pdf"]).save_file(),
        PendingDialog::Run { save, filter, dir, .. } => {
            let mut d = base;
            if let Some(dir) = dir {
                d = d.set_directory(dir);
            }
            if let Some((label, exts)) = filter {
                d = d.add_filter(label.as_str(), exts.as_slice());
            }
            if *save { d.save_file() } else { d.pick_file() }
        }
    }
}

/// A separate native window showing a `TXT` dialog's scrollable text. Self-contained — its
/// own egui `Context`, winit input `State`, and wgpu `Painter` (own surface) — so it renders
/// independently of the main window. Created when a script parks on `TXT` and dropped (window
/// closed) when the script resumes.
struct TxtWindow {
    window: Arc<Window>,
    ctx: egui::Context,
    state: egui_winit::State,
    painter: Painter,
    /// The highlighted line (0-based), like the command-history picker: `↑/↓` move it and the list
    /// scrolls minimally to keep it visible (`scroll_to_me`), so scrolling is smooth + line-aligned.
    sel: usize,
    /// Visible row count, recomputed each frame — the page size for PageUp/PageDown.
    page_rows: usize,
    /// One-shot: scroll the highlight into view on the next render (set on open + each keyboard
    /// move). Cleared after, so the mouse wheel owns the scroll position otherwise.
    scroll_to_sel: bool,
    repaint_at: Option<Instant>,
    /// The `TXTFAMILY`+`TXTSTYLE` currently installed into this window's egui context (`None` =
    /// egui's built-in default). Compared each frame so we only `set_fonts` on a change.
    applied_family: Option<(String, xyscript_app::host::FontStyle)>,
}

/// The separate `DIR/W` listing window — a persistent, interactive child window (unlike the
/// short-lived `TxtWindow`). Its own egui `Context`, winit input `State`, and wgpu `Painter`.
/// Thin renderer of the host's `dir_window` state: navigation/activation are forwarded back to
/// the host, which owns the listing `Document`.
struct DirWindow {
    window: Arc<Window>,
    ctx: egui::Context,
    state: egui_winit::State,
    painter: Painter,
    /// Visible row count, recomputed each frame — the page size for PageUp/PageDown.
    page_rows: usize,
    /// One-shot: scroll the selected row into view on the next render (set on open + keyboard move).
    scroll_to_sel: bool,
    /// The `DWFAMILY`+`DWSTYLE` currently installed into this window's egui context (`None` = egui's
    /// default monospace). Compared each frame so we only `set_fonts` on a change.
    applied_family: Option<(String, xyscript_app::host::FontStyle)>,
    /// The window title currently set (the `$DP` path); re-applied only when it changes.
    title: String,
    repaint_at: Option<Instant>,
}

/// The separate Find/Replace window — a resizable, movable OS window (owned by the main window, so it
/// stays above it without blocking it). Its own egui `Context`, winit input `State`, and wgpu
/// `Painter`. Renders `GuiApp::find_window_ui`, which drives the MAIN document (caret + live
/// highlight-all) and asks the runtime to redraw the main window so the document updates live.
struct FindWindow {
    window: Arc<Window>,
    ctx: egui::Context,
    state: egui_winit::State,
    painter: Painter,
    repaint_at: Option<Instant>,
}

/// A floating `SCRATCH` window: a plain read/write text box for parking text while working on the
/// main document. No file, no save — closing it discards the text (after stashing it into
/// `$SCRATCHTEXT`). Its own egui `Context`, winit input `State`, and wgpu `Painter`, plus the owned
/// text buffer. The runtime keeps a `Vec` of these, so several can be open at once. Editing is
/// egui's built-in multiline `TextEdit` (caret, selection, mouse, system clipboard, undo) — no
/// XyScript keymap or verbs run inside it, matching the "nothing else" brief.
struct ScratchWindow {
    window: Arc<Window>,
    ctx: egui::Context,
    state: egui_winit::State,
    painter: Painter,
    /// The parked text — owned here and edited in place by the egui `TextEdit`.
    text: String,
    /// One-shot: focus the text box on the first frame so the user can type immediately.
    focus_pending: bool,
    /// Text produced by the keyboard file's typing layer (a captured `insert "…"` cell — custom
    /// layouts, etc.) awaiting injection into the TextEdit on the next frame, as an egui text event.
    pending_text: String,
    /// The `EDFAMILY`+`EDSTYLE` installed into this window's egui context (`None` = egui default).
    /// Compared each frame so we only `set_fonts` on a change.
    applied_family: Option<(String, xyscript_app::host::FontStyle)>,
    repaint_at: Option<Instant>,
}

struct Handler {
    app: GuiApp,
    key: Option<KeyCap>,
    egui_ctx: egui::Context,
    // Created on `resumed`:
    window: Option<Arc<Window>>,
    egui_state: Option<egui_winit::State>,
    painter: Option<Painter>,
    /// The physical size the wgpu surface is currently configured for. egui reads the window's live
    /// `inner_size`/`scale_factor` every frame (via `take_egui_input`), but the surface is only
    /// reconfigured on `Resized` — and a mixed-DPI monitor change can leave those out of step (the DPI
    /// update lands but the matching resize is skipped/reordered), so egui lays out for the new density
    /// and paints into a stale-sized surface → the whole window renders tiny until restart. `redraw`
    /// reconciles the surface to the window each frame using this; `None` = not yet configured.
    surface_px: Option<(u32, u32)>,
    /// When egui asked for the next repaint (for caret blink, IME, etc.).
    repaint_at: Option<Instant>,
    /// A native file dialog running on its own thread (Windows), delivered non-blocking so the main
    /// loop keeps pumping while it's open. `None` = no dialog in flight.
    dialog_job: Option<DialogJob>,
    /// The separate `TXT` window, when a script is paused on a `TXT` dialog.
    txt: Option<TxtWindow>,
    /// The separate `DIR/W` listing window (`None` = closed).
    dir: Option<DirWindow>,
    /// Remembered `DIR/W` window geometry for this session (set from the user's moves/resizes,
    /// reused on the next `DIR/W`). `None` → centre over the main window. Session-only.
    dir_pos: Option<PhysicalPosition<i32>>,
    dir_size: Option<PhysicalSize<u32>>,
    /// Remembered `TXT` window geometry (outer position + inner size) for THIS session: set
    /// from the user's moves/resizes, reused on the next `TXT`. `None` → centre over the main
    /// window. Session-only (not persisted to disk).
    txt_pos: Option<PhysicalPosition<i32>>,
    txt_size: Option<PhysicalSize<u32>>,
    /// The separate Find/Replace window (`None` = closed). Created/destroyed to match
    /// `GuiApp::find_dialog_open()`.
    find: Option<FindWindow>,
    /// Remembered FIND window geometry (outer position + inner size). Restored on open and saved on
    /// move/resize/close, PERSISTED to disk (`find-window.txt`) when `WINGEO` is on — falls back to
    /// this session value, else centred over the main window.
    find_pos: Option<PhysicalPosition<i32>>,
    find_size: Option<PhysicalSize<u32>>,
    /// Live keyboard modifier state from winit (`ModifiersChanged`), used to detect the
    /// Ctrl+Break script-abort chord independently of the keymap.
    mods: winit::keyboard::ModifiersState,
    /// Whether the OS title bar is currently shown on the main window. Synced to `TITLEBAR`/
    /// `$TITLEBAR` each loop so a toggle applies live via `set_decorations`.
    decorations: bool,
    /// Ids of OUR windows (main / TXT / DIR-W / Find / scratchpads) that currently hold OS focus.
    /// Non-empty ⇒ some XyScript window is active, so `DIMONBLUR` should NOT dim the main window (only
    /// a switch to ANOTHER app dims it). Updated on every `Focused` event.
    focused_windows: HashSet<WindowId>,
    /// Floating `SCRATCH` windows — plain read/write text boxes for parking text. A collection (unlike
    /// the single TXT/DIR/FIND windows) so several can be open simultaneously. Session-only.
    scratch: Vec<ScratchWindow>,
    /// Monotonic counter for scratchpad titles (`Scratch 1`, `Scratch 2`, …); never reused this session.
    next_scratch_num: u32,
    /// The most-recently-focused scratchpad (the target of `SCRATCHCLOSE`); cleared as windows close.
    scratch_focus: Option<WindowId>,
    /// Remembered scratchpad geometry (outer position + inner size) — the last one moved/resized/closed.
    /// Restored on the next `SCRATCH` and PERSISTED to disk (`scratch-window.txt`) when `WINGEO` is on;
    /// else this session's value, else a default cascaded from the main window.
    scratch_pos: Option<PhysicalPosition<i32>>,
    scratch_size: Option<PhysicalSize<u32>>,
}

impl Handler {
    fn new(mut app: GuiApp) -> Self {
        let key = app.take_capture_keymap().map(KeyCap::new);
        let egui_ctx = egui::Context::default();
        // Match the prior (light) chrome; eframe followed the light system theme.
        egui_ctx.set_theme(egui::ThemePreference::Light);
        Handler {
            app,
            key,
            egui_ctx,
            window: None,
            egui_state: None,
            painter: None,
            surface_px: None,
            repaint_at: None,
            dialog_job: None,
            txt: None,
            txt_pos: None,
            txt_size: None,
            dir: None,
            dir_pos: None,
            dir_size: None,
            find: None,
            find_pos: None,
            find_size: None,
            mods: winit::keyboard::ModifiersState::empty(),
            decorations: true,
            focused_windows: HashSet::new(),
            scratch: Vec::new(),
            next_scratch_num: 0,
            scratch_focus: None,
            scratch_pos: None,
            scratch_size: None,
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Rebuild the capture resolver after a `LOADKBD` swapped the keymap. The GUI intercepts
    /// keys with THIS resolver (`self.key`), which is separate from the editor's own — without
    /// this resync a `LOADKBD` would change nothing in the live GUI. The keymap is already
    /// desugared (D4); `KeyCap::new` recomputes the passthrough mask for the new modifiers.
    fn sync_capture_keymap(&mut self) {
        if let Some(km) = self.app.take_reloaded_keymap() {
            self.key = Some(KeyCap::new(km));
        }
    }

    /// Run the keymap against one raw key edge. Returns `true` if we consumed it (egui must
    /// NOT also see it). Any resolved command is run on the editor.
    fn intercept_key(&mut self, ev: &winit::event::KeyEvent) -> bool {
        // READKEY capture: a parked script consumes the next *non-modifier* keystroke RAW —
        // ahead of the keymap — so even assigned/function keys feed the script (with their
        // character and scancode → `$SC`) instead of running their command.
        if matches!(ev.state, ElementState::Pressed) && self.app.awaiting_key() {
            let Some(sc) = ev.physical_key.to_scancode() else {
                return true; // swallow undecodable keys rather than run commands mid-READKEY
            };
            let native = (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 };
            // Resolve the canonical code AND ask the loaded keymap whether it's a modifier trigger — so
            // a user-defined modifier (e.g. `HOTKEY`) is recognised, not just the standard ones.
            let (code, keymap_mod) = match self.key.as_ref() {
                Some(k) => {
                    let c = k.table.to_canonical(native);
                    (c, c.is_some_and(|cc| k.resolver.keymap.modifier_for_key(cc).is_some()))
                }
                None => (None, false),
            };
            // A bare modifier press must NOT count as the keystroke. FEED its down edge to the resolver
            // (so its bit is HELD → shows in `$SCC` and the description when a real key follows), then
            // keep waiting. Detect via the keymap OR the logical key (catches Super/Win when the keymap
            // doesn't define it). The matching UP edge arrives after READKEY resumes and balances via
            // the normal path below.
            let is_mod = keymap_mod || is_modifier_key(&ev.logical_key);
            if is_mod {
                if let (Some(k), Some(c)) = (self.key.as_mut(), code) {
                    k.resolver.resolve(KbdKey::down(c)); // record the modifier-down; updates state
                }
                return true; // consume; keep waiting for a real key
            }
            let Some(code) = code else {
                return true; // undecodable → don't run a command mid-READKEY
            };
            // The modifier bits held for THIS key → `$SCC` = `$SC` + these, so `KEY($SCC)` reads the
            // binding on the Shift/Ctrl/Alt layer.
            let mods = self.key.as_ref().map(|k| k.resolver.state.current_bitmask() as i64).unwrap_or(0);
            // `READKEY/D` writes a readable description (`CTRL+Y`, `Enter`, …) into the var; plain
            // READKEY writes the raw character. Modifier names come from the keymap's `ModifierDef`s.
            let value = if self.app.readkey_wants_description() {
                match self.key.as_ref() {
                    Some(k) => describe_key(&ev.logical_key, mods, &k.resolver.keymap.modifiers),
                    None => key_name(&ev.logical_key),
                }
            } else {
                ev.text.as_ref().map(|s| s.as_str()).unwrap_or("").to_string()
            };
            self.app.feed_input_key(&value, code as u16, mods);
            self.request_redraw();
            return true;
        }
        // Writer's View: `Esc` exits the distraction-free mode (restoring the previous view). It takes
        // priority over the editor's ordinary Esc, but NOT over a parked READKEY (handled above), so a
        // script waiting on a key still receives Esc.
        if matches!(ev.state, ElementState::Pressed)
            && matches!(ev.logical_key, Key::Named(NamedKey::Escape))
            && self.app.writer_view()
        {
            self.app.exit_writer_view();
            self.request_redraw();
            return true;
        }
        // While the keyboard menu is open it owns navigation (modal): divert nav keys to it and
        // swallow everything else, BEFORE the keymap runs — so menu keys never reach the editor.
        if self.app.menu_active() {
            // Translate a BOUND navigation verb (up/down/left/right) into its arrow code so the
            // user's own nav keys drive the menu — not just the physical arrows. Resolve BOTH edges
            // so the resolver's modifier state stays honest; we never RUN the dispatched command
            // here (the menu is modal), only read it to pick the direction. A key that isn't a nav
            // verb falls back to its physical code, which `menu_key` maps (arrows/enter/esc) or
            // swallows — so bound Esc/Enter still cancel/activate, and everything else is inert.
            let menu_code = self.key.as_mut().and_then(|key| {
                let code = ev.physical_key.to_scancode().and_then(|sc| {
                    let native = (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 };
                    key.table.to_canonical(native)
                })?;
                match ev.state {
                    ElementState::Pressed => Some(match key.resolver.resolve(KbdKey::down(code)) {
                        Disposition::Dispatch(xpl) => nav_verb_code(&xpl).unwrap_or(code),
                        _ => code,
                    }),
                    ElementState::Released => {
                        key.resolver.resolve(KbdKey::up(code)); // keep modifier state balanced
                        None
                    }
                }
            });
            if let Some(code) = menu_code {
                if is_menu_nav_code(code) {
                    self.app.menu_key(code);
                } else if !self.mods.control_key() && !self.mods.alt_key() {
                    // Not a nav key → offer a plain letter as an ACCELERATOR (jump to + activate the
                    // item whose underlined letter matches). Ctrl/Alt-held keys are commands, not
                    // accelerators, so they're left swallowed. (`menu_code` is `Some` only on the
                    // press edge, so this never fires on release.)
                    if let Some(ch) = ev
                        .text
                        .as_ref()
                        .and_then(|s| s.chars().next())
                        .filter(|c| c.is_ascii_alphanumeric())
                    {
                        self.app.menu_char(ch);
                    }
                }
                self.request_redraw();
            }
            return true; // consume; the menu is modal while open
        }
        let mut to_run: Option<String> = None;
        // winit's physical key → native scancode (extended = high byte 0xE0) → the kbd's WIN_E0
        // (0x100) encoding → canonical code, reusing the proven host table. Computed BEFORE the
        // block below so the physical `code` is also in scope at the `to_run` stage — used there
        // for the modal navigation fallback (a rebound nav key drives the modal by its identity).
        let Some(sc) = ev.physical_key.to_scancode() else {
            return false;
        };
        let native = (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 };
        let Some(code) = self.key.as_ref().and_then(|k| k.table.to_canonical(native)) else {
            return false;
        };
        // HEX view: Tab switches the edit column (hex pairs ↔ text). Handle it HERE, before the KBD
        // resolver (a bound `TAB` verb would insert a tab) AND before egui (which hijacks Tab for
        // focus navigation, so it never reaches the canvas). Plain Tab only; Ctrl/Alt+Tab fall through.
        const TAB_CODE: u8 = 15;
        if matches!(ev.state, ElementState::Pressed)
            && code == TAB_CODE
            && self.app.hex_view()
            && !self.mods.control_key()
            && !self.mods.alt_key()
        {
            self.app.hex_toggle_column();
            self.request_redraw();
            return true; // consumed — don't run the verb or forward to egui
        }
        // ---- Keyboard access to the menu bar (platform accelerators) --------------------------------
        // Open the menu bar from the keyboard the way each desktop does — but WITHOUT stealing a chord
        // the user has bound: we act only when this exact (modifier, key) combo is UNBOUND in the
        // keymap, so a user binding always wins. (The menu, once open, is modal and handled earlier at
        // `menu_active()`, so this only fires from the editor.)
        //   • Windows / Linux: Alt+<letter> opens the top menu whose underlined letter matches.
        //   • Every platform: F10 or Ctrl+F2 focuses (opens) the menu bar — and these are the ONLY menu
        //     keys on macOS, where Option+<letter> is reserved for typing special characters.
        if matches!(ev.state, ElementState::Pressed) && !self.app.menu_active() {
            // A chord defers to the keymap only when it runs a COMMAND (an XPL assignment). `KF`
            // (pass the key to the OS) and an unbound cell do NOT, so either one frees the chord for a
            // menu accelerator: a keymap can opt a chord out of its binder layer with `KF` to get the
            // Alt+<letter> menu behaviour — while that same `KF` still passes Option+<letter> through
            // for special-character input on macOS (where the Alt path below is compiled out).
            let runs_command = self.key.as_ref().is_some_and(|k| {
                matches!(
                    k.resolver.keymap.resolve(k.resolver.state.current_bitmask(), code),
                    Some(xyscript_kbd::Assignment::Xpl(_))
                )
            });
            if !runs_command {
                let focus_bar = matches!(ev.logical_key, Key::Named(NamedKey::F10))
                    || (self.mods.control_key()
                        && matches!(ev.logical_key, Key::Named(NamedKey::F2)));
                if focus_bar {
                    self.app.focus_menu_bar();
                    self.request_redraw();
                    return true;
                }
                // Alt+<letter> mnemonic — not on macOS (Option+<letter> types special characters).
                #[cfg(not(target_os = "macos"))]
                if self.mods.alt_key() && !self.mods.control_key() {
                    if let Key::Character(s) = &ev.logical_key {
                        if let Some(ch) =
                            s.chars().next().filter(|c| c.is_ascii_alphanumeric())
                        {
                            if let Some(idx) = self.app.top_accel_index(ch) {
                                self.app.open_top_menu(idx);
                                self.request_redraw();
                                return true;
                            }
                        }
                    }
                }
            }
        }
        let consumed = {
            let Some(key) = self.key.as_mut() else {
                return false;
            };
            match ev.state {
                ElementState::Pressed => {
                    // Resolve (also updates modifier state).
                    let disp = key.resolver.resolve(KbdKey::down(code));
                    if let Disposition::Dispatch(xpl) = disp {
                        // Capture (run the command, suppress OS typing) when a command modifier is
                        // held / it's a function-class key (`should_capture`), OR when the cell is a
                        // COMMAND rather than a typing self-insert — so an unshifted printable bound
                        // to a command (e.g. `94=XC`) runs instead of typing its character. Keys
                        // egui owns (`egui_owns`) are left to it even when bound. Typing self-insert
                        // cells with no command modifier fall through to egui (OS layout + IME).
                        let modified = key.resolver.state.current_bitmask() != 0;
                        // egui keeps Enter always; it keeps Up/Down AND PageUp/PageDown — both the
                        // gray cluster and the NumLock-off numpad equivalents — ONLY when UNMODIFIED
                        // (plain wrap-aware visual/page motion + picker scrolling). ANY modifier
                        // (incl. Shift) reclaims them for the keymap, so e.g. `Shift+PgUp = HI` fires.
                        // BUT only when the cell is a motion verb the GUI actually refines visually
                        // (arrows→visual line move, pages→page move); a nav key bound to ANYTHING else
                        // (GT/TH, a macro, …) belongs to the keymap, so such bindings aren't swallowed.
                        let nav_key = matches!(
                            code,
                            ARROW_UP | ARROW_DOWN | PAGE_UP | PAGE_DOWN
                                | NUMPAD_UP | NUMPAD_DOWN | NUMPAD_PAGE_UP | NUMPAD_PAGE_DOWN
                        );
                        // In the HEX view the vertical-nav keys must be CAPTURED (they drive the host's
                        // byte-row motion — ↑/↓ = ±16 bytes, PgUp/PgDn = ±a screen); handing them to
                        // egui would run wrap-aware TEXT scrolling over a grid that has no text layout.
                        // The byte-DELETE keys go the other way: Backspace/Delete must reach egui in the
                        // hex view (they run the GUI's byte delete), not the KBD's text-delete verb.
                        // (Tab is handled earlier — egui would hijack it here, so it can't come this way.)
                        let hex_edit_key =
                            self.app.hex_view() && (code == BACKSPACE || DELETE_CODES.contains(&code));
                        let egui_owns = ALWAYS_EGUI.contains(&code)
                            || hex_edit_key
                            || (nav_key && !modified && is_gui_motion(&xpl) && !self.app.hex_view());
                        let capture = should_capture(code, &key.resolver, key.passthrough_mask)
                            || (!is_typing_cell(&xpl) && !egui_owns);
                        if capture {
                            key.consumed.insert(code);
                            to_run = Some(xpl);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                ElementState::Released => {
                    // Feed the up edge so modifier/auto-repeat state stays honest; a dual-role
                    // modifier may fire its tap here.
                    if let Disposition::Dispatch(xpl) = key.resolver.resolve(KbdKey::up(code)) {
                        if should_capture(code, &key.resolver, key.passthrough_mask) {
                            to_run = Some(xpl);
                        }
                    }
                    key.consumed.remove(&code) // mirror the consumed down edge
                }
            }
        };
        if let Some(xpl) = to_run {
            // A key bound to the ENTER verb should, when a modal dialog / DIR-LS listing owns the
            // keyboard OR the command line is open, ACTIVATE (open file / run history command /
            // choose / SUBMIT THE CMLINE) exactly like the primary Enter — not insert a newline
            // (which the read-only listing ignores, and which on the cmline would drop a stray '\n'
            // instead of running the command). Hand egui a synthetic Enter so every existing handler
            // (modal AND the cmline's Enter→submit) responds, and don't run the verb. (The primary
            // Enter, scancode 28, is in ALWAYS_EGUI so it already reaches egui directly; this brings
            // any OTHER key bound to ENTER — e.g. numpad Enter, 104 — inline with it.)
            //
            // Also handle a CHAIN that ends in ENTER, e.g. `104=AR;ENTER`: on the cmline, run the
            // part before ENTER (auto-replace, etc.) FIRST, then submit. `enter_submit_prefix`
            // returns the pre-ENTER text (`""` for a bare ENTER), or `None` if ENTER isn't the last
            // `;`-separated token.
            if let Some(prefix) = enter_submit_prefix(&xpl) {
                let modal = self.app.modal_owns_keyboard();
                if modal || self.app.cmline_open() {
                    // On the cmline (not a blocking modal, where bindings are inert), run everything
                    // before the trailing ENTER against the cmline before the submit.
                    if !modal && !prefix.is_empty() {
                        self.app.run_captured(prefix);
                    }
                    if let Some(state) = self.egui_state.as_mut() {
                        state.egui_input_mut().events.push(egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                    self.request_redraw();
                    return true;
                }
            }
            // While a genuine blocking modal owns the keyboard (INPUT/PICK/history/MSG/breakpoint/
            // close-open/verify/quit prompt), a bound NAVIGATION verb (up/down/left/right/page)
            // should drive the modal itself — so the user's own nav keys work in a PICK/history
            // list or an INPUT field, not just the physical arrows. Translate it into the matching
            // synthetic egui arrow (same mechanism as the ENTER special-case above). Any OTHER
            // bound command stays INERT: it must not run against the document behind the modal.
            // Only CAPTURED keys reach here; the modal's own uncaptured keys (physical arrows/
            // typing/Esc/Enter/Y-N) reach its handlers directly. A DIR/LS listing is excluded from
            // `modal_blocks_bindings`, so bindings run normally there.
            if self.app.modal_blocks_bindings() {
                // Drive the modal with a bound NAVIGATION verb; failing that (e.g. the binding is an
                // `IF…LD…EI` chain whose first word isn't a nav verb), fall back to the PHYSICAL
                // key's direction if it's itself a nav key — so a rebound Numpad-Down still moves
                // the list. Any non-nav key stays inert (its document command must not run here).
                // Physical Backspace/Delete drive a modal's filter/list by identity (edit the
                // type-to-find filter, delete a highlighted row) AHEAD of the nav mapping — else a
                // Backspace bound to a jump verb (`BS`→bottom) would move the list instead of editing.
                if let Some(key) = edit_code_egui_key(code)
                    .or_else(|| nav_verb_egui_key(&xpl))
                    .or_else(|| nav_code_egui_key(code))
                {
                    if let Some(state) = self.egui_state.as_mut() {
                        state.egui_input_mut().events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::default(),
                        });
                    }
                    self.request_redraw();
                }
                return true; // consumed: nav/edit drove the modal, or a non-nav command was swallowed
            }
            // A key bound to a vertical-motion verb (LU/LD/UP/DOWN) moves by VISUAL row in normal
            // editing — wrap-aware, like the arrow keys — instead of the engine's logical by-line
            // move (which skips a wrapped line's continuation rows).
            if self.app.try_visual_vmotion(&xpl) {
                self.request_redraw();
                return true;
            }
            self.app.run_captured(&xpl);
        }
        consumed
    }

    /// Reconfigure the wgpu surface to `(w, h)` physical pixels if it isn't already — cheap no-op when
    /// unchanged. The safety net for mixed-DPI monitor changes: keeps the surface in step with the
    /// window every frame, not only on the `Resized` events (which can be skipped/reordered around a
    /// `ScaleFactorChanged`), so egui's per-frame ppp and the surface never drift apart.
    fn ensure_surface(&mut self, w: u32, h: u32) {
        if self.surface_px == Some((w, h)) {
            return;
        }
        if let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
            if let Some(p) = self.painter.as_mut() {
                p.on_window_resized(ViewportId::ROOT, nw, nh);
                self.surface_px = Some((w, h));
            }
        }
    }

    /// Render one frame: run egui over `GuiApp::ui` and paint via wgpu.
    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // Size the surface to the window BEFORE painting — a mixed-DPI monitor change updates egui's
        // ppp (read fresh below) but can leave the surface stale, which renders the whole window tiny
        // until restart.
        //
        // Guard against a STALE inner_size, too: winit-on-Windows can keep reporting the maximized
        // client size for a while after a maximize→restore, while the window is actually restored.
        // `inner` LARGER than `outer` is physically impossible (outer = client + borders, so
        // outer >= inner always) — that's the tell. Left uncorrected, both the wgpu surface (below)
        // and egui's screen_rect (which `take_egui_input` also derives from inner_size) inflate to the
        // stale size, and the oversized frame is squished into the smaller window → the whole UI (chrome
        // and canvas alike) renders tiny and stays that way until the next resize. Fall back to
        // outer_size (a close, valid client estimate) and nudge winit to re-emit a correct WM_SIZE.
        let inner = window.inner_size();
        let outer = window.outer_size();
        let stale = inner.width > outer.width || inner.height > outer.height;
        let phys = if stale { outer } else { inner };
        if stale {
            let _ = window.request_inner_size(outer);
        }
        self.ensure_surface(phys.width, phys.height);
        let mut raw_input = match self.egui_state.as_mut() {
            Some(s) => s.take_egui_input(&window),
            None => return,
        };
        if stale {
            // Rescale egui's screen_rect (which it built from the stale inner_size) to the corrected
            // physical size, so egui lays out to match the surface THIS frame — no tiny flash while we
            // wait for winit to resync. The ratio cancels out whatever ppp/zoom egui used.
            if let Some(r) = raw_input.screen_rect.as_mut() {
                let sx = outer.width as f32 / inner.width.max(1) as f32;
                let sy = outer.height as f32 / inner.height.max(1) as f32;
                let sz = r.size();
                *r = egui::Rect::from_min_size(r.min, egui::vec2(sz.x * sx, sz.y * sy));
            }
        }
        let ctx = self.egui_ctx.clone();
        let app = &mut self.app;
        // eframe gave the app a bare, full-window `Ui` (no margin/background) via `run_ui`.
        let full_output = ctx.run_ui(raw_input, |ui| app.ui(ui));

        if let Some(state) = self.egui_state.as_mut() {
            state.handle_platform_output(&window, full_output.platform_output);
        }
        let ppp = ctx.pixels_per_point();
        let prims = ctx.tessellate(full_output.shapes, ppp);
        let clear = egui::Rgba::from(ctx.global_style().visuals.panel_fill).to_array();
        if let Some(painter) = self.painter.as_mut() {
            painter.paint_and_update_textures(
                ViewportId::ROOT,
                ppp,
                clear,
                &prims,
                &full_output.textures_delta,
                vec![],
            );
        }

        // Honor egui's repaint request (animations, caret blink, IME).
        let delay = full_output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        if delay.is_zero() {
            window.request_redraw();
            self.repaint_at = None;
        } else if delay < std::time::Duration::MAX {
            self.repaint_at = Some(Instant::now() + delay);
        } else {
            self.repaint_at = None;
        }
    }

    /// Create or destroy the separate `TXT` window to match whether a script is parked on a
    /// `TXT` dialog. Called each loop iteration (it needs the `ActiveEventLoop` to make a window).
    fn sync_txt(&mut self, event_loop: &ActiveEventLoop) {
        match (self.txt.is_some(), self.app.txt_request()) {
            (false, Some((title, _))) => {
                let title = title.unwrap_or_else(|| "Text".to_string());
                let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                // Restore geometry: disk (per WINGEO, clamped onto a real monitor) → this session →
                // default size centred over the main window.
                let restored = self
                    .app
                    .remember_window_geometry()
                    .then(|| load_geometry("txt-window.txt"))
                    .flatten()
                    .map(|(lw, lh, x, y)| clamp_geometry(event_loop, lw, lh, x, y));
                // Create with a LOGICAL size so Windows' per-monitor DPI scaling isn't applied a
                // SECOND time: a PHYSICAL size requested at creation gets re-scaled when winit handles
                // the DPI change as the window lands on its monitor, so on a HiDPI display the window
                // came out scale× too big (640·s × 480·s → "huge"/erratic). A remembered physical size
                // is converted back to logical for the same reason.
                let logical: LogicalSize<f64> = restored
                    .map(|(lw, lh, _, _)| LogicalSize::new(lw, lh))
                    .or_else(|| self.txt_size.map(|p| p.to_logical(scale)))
                    .unwrap_or(LogicalSize::new(640.0, 480.0));
                let size_phys: PhysicalSize<u32> = logical.to_physical(scale); // for centering math
                let pos = restored
                    .map(|(_, _, x, y)| PhysicalPosition::new(x, y))
                    .or(self.txt_pos)
                    .or_else(|| {
                        let main = self.window.as_ref()?;
                        let mp = main.outer_position().ok()?;
                        let ms = main.outer_size();
                        Some(PhysicalPosition::new(
                            mp.x + (ms.width as i32 - size_phys.width as i32) / 2,
                            mp.y + (ms.height as i32 - size_phys.height as i32) / 2,
                        ))
                    });
                // Create WITHOUT a position: on Windows, creating a window WITH a position that lands
                // on a different-DPI monitor makes winit mis-scale the logical inner size (the window
                // came out huge at 150%). Create at the default placement (correct size for its
                // monitor), then move it.
                // `mut` is used only by the `#[cfg(windows)]` owner-window block below; on other
                // platforms it's untouched, so silence the unused-mut lint there.
                #[cfg_attr(not(windows), allow(unused_mut))]
                let mut attrs = Window::default_attributes()
                    .with_title(title)
                    .with_inner_size(logical)
                    .with_window_icon(window_icon());
                // Own the modal to the main window so it travels with XyScript in the z-order:
                // an owned window has no separate taskbar / Alt-Tab entry and always sits above its
                // owner, so returning to the app (Alt-Tab back to XyScript) brings the still-open
                // modal forward instead of stranding it behind the editor. (Windows-only concept.)
                #[cfg(windows)]
                {
                    use winit::platform::windows::WindowAttributesExtWindows;
                    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    if let Some(owner) = self.window.as_ref().and_then(|w| {
                        match w.window_handle().ok()?.as_raw() {
                            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                            _ => None,
                        }
                    }) {
                        attrs = attrs.with_owner_window(owner);
                    }
                }
                let window = Arc::new(event_loop.create_window(attrs).expect("create TXT window"));
                if let Some(p) = pos {
                    window.set_outer_position(p);
                }
                let ctx = egui::Context::default();
                ctx.set_theme(egui::ThemePreference::Light);
                let state = egui_winit::State::new(
                    ctx.clone(),
                    ViewportId::ROOT,
                    event_loop,
                    Some(window.scale_factor() as f32),
                    None,
                    None,
                );
                let mut painter = pollster::block_on(Painter::new(
                    ctx.clone(),
                    wgpu_config(),
                    false,
                    RendererOptions::default(),
                ));
                pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
                    .expect("set TXT wgpu window");
                window.request_redraw();
                self.txt = Some(TxtWindow {
                    window,
                    ctx,
                    state,
                    painter,
                    sel: 0,
                    page_rows: 1,
                    scroll_to_sel: true,
                    repaint_at: None,
                    applied_family: None,
                });
            }
            (true, None) => self.teardown_txt(), // script resumed on its own → close the window
            _ => {}
        }
    }

    /// Tear the TXT window down (script already resumed): persist its geometry (per `WINGEO`), then
    /// drop it. Unlike `close_txt`, does NOT dismiss the script (it resumed by itself).
    fn teardown_txt(&mut self) {
        if let Some(t) = self.txt.as_ref() {
            self.txt_pos = t.window.outer_position().ok();
            self.txt_size = Some(t.window.inner_size());
            if self.app.remember_window_geometry() {
                save_geometry(&t.window, "txt-window.txt");
            }
        }
        self.txt = None;
    }

    /// Persist the TXT window geometry now if `WINGEO` is on (called on live move/resize).
    fn save_txt_geometry_if_wingeo(&self) {
        if self.app.remember_window_geometry() {
            if let Some(t) = self.txt.as_ref() {
                save_geometry(&t.window, "txt-window.txt");
            }
        }
    }

    /// Render one frame of the `TXT` window (scrollable, read-only, monospace).
    fn redraw_txt(&mut self) {
        let Some((_title, text)) = self.app.txt_request() else {
            return;
        };
        // Optional two-column (name, description) grid (currently only `LISTLIB`); when present the
        // body renders as a wrapping grid instead of plain lines, and `sel` indexes records.
        let columns = self.app.txt_columns();
        // Optional per-line label colours (currently only `HLKINDS`): each kind's name drawn in its
        // own colour, so the list previews the theme.
        let line_colors = self.app.txt_line_colors();
        // User-configurable text size (`TXTFONT`, pt), window zoom (`TXTSCALE`), and colours
        // (`TXTFGCOLOR`/`TXTBGCOLOR`).
        let txt_font = self.app.txt_font();
        let txt_scale = self.app.txt_scale();
        let (fr, fg_, fb) = self.app.txt_fg_color();
        let (br, bg_, bb) = self.app.txt_bg_color();
        let txt_fg = egui::Color32::from_rgb(fr, fg_, fb);
        let txt_bg = egui::Color32::from_rgb(br, bg_, bb);
        // `TXTFAMILY`/`TXTSTYLE` — install the chosen font into this window's egui context (only on
        // change). The TXT label renders `.monospace()`, so prepend into the Monospace family;
        // `None` reverts to egui's embedded monospace.
        let want_txt = self
            .app
            .txt_font_family()
            .filter(|s| !s.is_empty())
            .map(|s| (s.to_string(), self.app.txt_font_style()));
        let Some(txt) = self.txt.as_mut() else {
            return;
        };
        if want_txt != txt.applied_family {
            match &want_txt {
                Some((fam, st)) => {
                    crate::apply_egui_family(
                        &txt.ctx,
                        self.app.font_db(),
                        fam,
                        &[egui::FontFamily::Monospace],
                        st.bold(),
                        st.italic(),
                    );
                }
                None => txt.ctx.set_fonts(egui::FontDefinitions::default()),
            }
            txt.applied_family = want_txt;
        }
        txt.ctx.set_zoom_factor(txt_scale);
        // Background fills the window (the clear colour reads `panel_fill`); text colour set on the label.
        txt.ctx.global_style_mut(|s| s.visuals.panel_fill = txt_bg);
        let raw_input = txt.state.take_egui_input(&txt.window);
        let sel = txt.sel;
        let want_scroll = std::mem::take(&mut txt.scroll_to_sel);
        let mut new_sel = sel;
        let mut page_rows = txt.page_rows;
        // `run_ui` hands us a bare full-window `Ui` (same path as the main window). Render the text
        // as one selectable row per line (the command-history model): the highlighted row scrolls
        // itself into view, so `↑/↓` give smooth, line-aligned scrolling instead of pixel jumps.
        let full_output = txt.ctx.run_ui(raw_input, |ui| {
            let avail = ui.available_height();
            egui::ScrollArea::vertical()
                .auto_shrink(false) // fill the window so the scrollbar spans it
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    // Wrap long content to the window width (no horizontal scroll past the right edge).
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    let mut row_h = f32::INFINITY; // smallest row height, for paging
                    // A wrapping, non-interactive monospace label (so row-level click/scroll wins).
                    let cell = |ui: &mut egui::Ui, s: &str| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(s).monospace().size(txt_font).color(txt_fg),
                            )
                            .selectable(false),
                        );
                    };
                    if let Some(rows) = &columns {
                        // Two columns: name on the left, description on the right, each wrapping within
                        // its own column. Size the name column to the WIDEST name plus a two-space
                        // gutter (measured in the actual monospace font, so it tracks font/zoom), so
                        // every description starts at the same x — clear of the longest name.
                        let sel_bg = ui.visuals().selection.bg_fill;
                        let font_id = egui::FontId::monospace(txt_font);
                        // Widest name + a two-space gutter, measured in the actual font.
                        let mut widest = 0.0_f32;
                        for (n, _) in rows.iter() {
                            let w = ui.ctx().fonts_mut(|f| {
                                f.layout_no_wrap(n.clone(), font_id.clone(), txt_fg).size().x
                            });
                            widest = widest.max(w);
                        }
                        let gutter = ui.ctx().fonts_mut(|f| {
                            f.layout_no_wrap("  ".to_string(), font_id.clone(), txt_fg).size().x
                        });
                        // Clamp so a pathologically long name can't swallow the whole window.
                        let name_w = (widest + gutter).min(ui.available_width() * 0.6);
                        for (i, (name, desc)) in rows.iter().enumerate() {
                            let fill =
                                if i == sel { sel_bg } else { egui::Color32::TRANSPARENT };
                            let out = egui::Frame::new()
                                .fill(fill)
                                .inner_margin(egui::Margin::symmetric(2, 1))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    // No inter-cell spacing: the gutter is baked into `name_w`.
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.horizontal_top(|ui| {
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(name_w, 0.0),
                                            egui::Layout::top_down(egui::Align::LEFT),
                                            |ui| {
                                                ui.set_min_width(name_w); // reserve the full column
                                                cell(ui, name);
                                            },
                                        );
                                        let rest = ui.available_width();
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(rest, 0.0),
                                            egui::Layout::top_down(egui::Align::LEFT),
                                            |ui| cell(ui, if desc.is_empty() { " " } else { desc }),
                                        );
                                    });
                                });
                            let resp = out.response.interact(egui::Sense::click());
                            let h = resp.rect.height();
                            if h > 0.0 {
                                row_h = row_h.min(h);
                            }
                            if i == sel && want_scroll {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                new_sel = i;
                            }
                        }
                    } else {
                        for (i, line) in text.lines().enumerate() {
                            // Render empty lines as a space so every row keeps a uniform height.
                            let shown = if line.is_empty() { " " } else { line };
                            // A per-line label colour (HLKINDS): draw the leading label (up to the
                            // first space) in that colour and the rest — the R G B triplet — in the
                            // normal text colour, so the list previews the theme while staying legible.
                            let kind_col =
                                line_colors.as_ref().and_then(|c| c.get(i).copied().flatten());
                            let label: egui::WidgetText = match kind_col {
                                Some((r, g, b)) => {
                                    use egui::text::{LayoutJob, TextFormat};
                                    let font = egui::FontId::monospace(txt_font);
                                    let split =
                                        shown.find(char::is_whitespace).unwrap_or(shown.len());
                                    let mut job = LayoutJob::default();
                                    job.append(
                                        &shown[..split],
                                        0.0,
                                        TextFormat::simple(font.clone(), egui::Color32::from_rgb(r, g, b)),
                                    );
                                    job.append(&shown[split..], 0.0, TextFormat::simple(font, txt_fg));
                                    job.into()
                                }
                                None => egui::RichText::new(shown)
                                    .monospace()
                                    .size(txt_font)
                                    .color(txt_fg)
                                    .into(),
                            };
                            let row = ui.selectable_label(i == sel, label);
                            let h = row.rect.height();
                            if h > 0.0 {
                                row_h = row_h.min(h); // a wrapped line is taller; page by single-line height
                            }
                            if i == sel && want_scroll {
                                row.scroll_to_me(None); // minimal scroll to keep the highlight visible
                            }
                            if row.clicked() {
                                new_sel = i;
                            }
                        }
                    }
                    if row_h.is_finite() && row_h > 0.0 {
                        page_rows = ((avail / row_h) as usize).saturating_sub(1).max(1);
                    }
                });
        });
        txt.sel = new_sel;
        txt.page_rows = page_rows;
        txt.state.handle_platform_output(&txt.window, full_output.platform_output);
        let ppp = txt.ctx.pixels_per_point();
        let prims = txt.ctx.tessellate(full_output.shapes, ppp);
        let clear = egui::Rgba::from(txt.ctx.global_style().visuals.panel_fill).to_array();
        txt.painter.paint_and_update_textures(
            ViewportId::ROOT,
            ppp,
            clear,
            &prims,
            &full_output.textures_delta,
            vec![],
        );
        let delay = full_output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        if delay.is_zero() {
            txt.window.request_redraw();
            txt.repaint_at = None;
        } else if delay < std::time::Duration::MAX {
            txt.repaint_at = Some(Instant::now() + delay);
        } else {
            txt.repaint_at = None;
        }
    }

    /// Move the `TXT` window's highlighted line by `delta` (keyboard), clamped to `[0, lines-1]`,
    /// and redraw (the render scrolls the new highlight into view).
    fn txt_move_sel(&mut self, delta: isize, lines: usize) {
        if let Some(t) = self.txt.as_mut() {
            let max = lines.saturating_sub(1) as isize;
            t.sel = (t.sel as isize + delta).clamp(0, max) as usize;
            t.scroll_to_sel = true;
            t.window.request_redraw();
        }
    }

    /// Set the `TXT` window's highlighted line directly (Home/End) and redraw.
    fn txt_set_sel(&mut self, line: usize) {
        if let Some(t) = self.txt.as_mut() {
            t.sel = line;
            t.scroll_to_sel = true;
            t.window.request_redraw();
        }
    }

    /// Dismiss the `TXT` window (× / Esc): persist its geometry (per `WINGEO`), resume the script,
    /// and close the window.
    fn close_txt(&mut self) {
        if let Some(t) = self.txt.as_ref() {
            self.txt_pos = t.window.outer_position().ok();
            self.txt_size = Some(t.window.inner_size());
            if self.app.remember_window_geometry() {
                save_geometry(&t.window, "txt-window.txt");
            }
        }
        self.app.dismiss_txt();
        self.txt = None;
        self.request_redraw(); // the main window reflects the resumed script
    }

    /// Handle a window event addressed to the `TXT` window. Esc / × dismiss; PageUp/Down,
    /// arrows, Home/End scroll; everything else is forwarded to its egui state.
    fn txt_window_event(&mut self, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                self.close_txt();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw_txt();
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.txt_size = Some(*size); // remember for this session (skip minimized 0×0)
                    if let Some(t) = self.txt.as_mut() {
                        t.painter.on_window_resized(ViewportId::ROOT, w, h);
                    }
                    self.save_txt_geometry_if_wingeo();
                }
            }
            // Remember the user's window position for this session (the first event reflects
            // where we placed it, which is harmless to record).
            WindowEvent::Moved(pos) => {
                self.txt_pos = Some(*pos);
                self.save_txt_geometry_if_wingeo();
            }
            WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. }
                if key_event.state == ElementState::Pressed =>
            {
                // Row count of the (stable-while-open) TXT body, for clamping the highlight — grid
                // records when columnar (LISTLIB), else text lines.
                let lines = self
                    .app
                    .txt_columns()
                    .map(|c| c.len().max(1))
                    .or_else(|| self.app.txt_request().map(|(_, t)| t.lines().count().max(1)))
                    .unwrap_or(1);
                let page = self.txt.as_ref().map(|t| t.page_rows as isize).unwrap_or(1);
                let handled = match key_event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        self.close_txt();
                        true
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.txt_move_sel(page, lines);
                        true
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.txt_move_sel(-page, lines);
                        true
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.txt_move_sel(1, lines);
                        true
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.txt_move_sel(-1, lines);
                        true
                    }
                    Key::Named(NamedKey::Home) => {
                        self.txt_set_sel(0);
                        true
                    }
                    Key::Named(NamedKey::End) => {
                        self.txt_set_sel(lines.saturating_sub(1));
                        true
                    }
                    _ => false,
                };
                if handled {
                    return;
                }
            }
            _ => {}
        }
        // Forward anything else (mouse, wheel, focus) to the TXT window's egui state.
        if let Some(t) = self.txt.as_mut() {
            let _ = t.state.on_window_event(&t.window, &event);
            t.window.request_redraw();
        }
    }

    /// Create the `DIR/W` window, or — if it already exists — focus it and refresh its listing, to
    /// match a freshly-issued `DIR/W`. Called each loop iteration (needs the `ActiveEventLoop`).
    fn sync_dir(&mut self, event_loop: &ActiveEventLoop) {
        // The ENGINE can drop the listing on its own — `DIRWCLOSE on` dismisses it as soon as a file
        // is opened. Tear the OS window down to match, or it would linger with nothing to draw
        // (`redraw_dir` early-returns without a view). Geometry is saved by `close_dir` as usual.
        if self.dir.is_some() && !self.app.dir_window_active() {
            self.close_dir();
        }
        if !self.app.take_dir_window_open() {
            return;
        }
        if let Some(d) = self.dir.as_ref() {
            // Reuse the single window: bring it forward + refresh to the new spec.
            self.app.dir_window_refresh_if_changed();
            d.window.focus_window();
            d.window.request_redraw();
            return;
        }
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        // Restore geometry: disk (per WINGEO, clamped onto a real monitor) → this session → centred.
        let restored = self
            .app
            .remember_window_geometry()
            .then(|| load_geometry("dir-window.txt"))
            .flatten()
            .map(|(lw, lh, x, y)| clamp_geometry(event_loop, lw, lh, x, y));
        // LOGICAL size for creation — see the TXT window: a physical size double-scales on HiDPI.
        let logical: LogicalSize<f64> = restored
            .map(|(lw, lh, _, _)| LogicalSize::new(lw, lh))
            .or_else(|| self.dir_size.map(|p| p.to_logical(scale)))
            .unwrap_or(LogicalSize::new(520.0, 560.0));
        let size_phys: PhysicalSize<u32> = logical.to_physical(scale); // for centering math
        let pos = restored
            .map(|(_, _, x, y)| PhysicalPosition::new(x, y))
            .or(self.dir_pos)
            .or_else(|| {
                let main = self.window.as_ref()?;
                let mp = main.outer_position().ok()?;
                let ms = main.outer_size();
                Some(PhysicalPosition::new(
                    mp.x + (ms.width as i32 - size_phys.width as i32) / 2,
                    mp.y + (ms.height as i32 - size_phys.height as i32) / 2,
                ))
            });
        // Create WITHOUT a position, then move it — see the TXT window: creating with a position on a
        // different-DPI monitor mis-scales the logical inner size on Windows.
        let attrs = Window::default_attributes()
            .with_title("Directory")
            .with_inner_size(logical)
            .with_window_icon(window_icon());
        let window = Arc::new(event_loop.create_window(attrs).expect("create DIR window"));
        if let Some(p) = pos {
            window.set_outer_position(p);
        }
        let ctx = egui::Context::default();
        ctx.set_theme(egui::ThemePreference::Light);
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut painter = pollster::block_on(Painter::new(
            ctx.clone(),
            wgpu_config(),
            false,
            RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .expect("set DIR wgpu window");
        window.request_redraw();
        self.dir = Some(DirWindow {
            window,
            ctx,
            state,
            painter,
            page_rows: 1,
            scroll_to_sel: true,
            applied_family: None,
            title: String::new(),
            repaint_at: None,
        });
    }

    /// Render one frame of the `DIR/W` window: a scrollable list of entries with the selected row
    /// highlighted, a footer showing the directory + type-to-find buffer. Inherits the main
    /// editor's colours and font size. Double-click (or Enter) activates an entry.
    fn redraw_dir(&mut self) {
        let Some(view) = self.app.dir_window_view() else {
            return;
        };
        // DIR/W appearance (DW… overrides, else inherited from the editor).
        let font = self.app.dw_font_pt();
        let scale = self.app.dw_scale();
        let (fr, fgc, fb) = self.app.dw_fg_color();
        let (br, bgc, bb) = self.app.dw_bg_color();
        let (ur, ug, ub) = self.app.editor_ui_fg_color();
        let fg = egui::Color32::from_rgb(fr, fgc, fb);
        let bg = egui::Color32::from_rgb(br, bgc, bb);
        let footer_col = egui::Color32::from_rgb(ur, ug, ub);
        // Matched type-to-find substring is highlighted within each filename row with the main
        // text-selection colours (`SELFGCOLOR` on `SELBGCOLOR`) so it stands out distinctly.
        let (sfr, sfg, sfb) = self.app.editor_sel_fg_color();
        let (sbr, sbg, sbb) = self.app.editor_sel_bg_color();
        let match_fg = egui::Color32::from_rgb(sfr, sfg, sfb);
        let match_bg = egui::Color32::from_rgb(sbr, sbg, sbb);
        let footer = if view.find.is_empty() {
            view.base.display().to_string()
        } else {
            format!("{}    find: {}", view.base.display(), view.find)
        };
        // `DWFAMILY`+`DWSTYLE` — install the chosen face into the Monospace family (the rows render
        // `.monospace()`); `None` reverts to egui's embedded monospace. Only on change.
        let want_family = self
            .app
            .dw_family()
            .filter(|s| !s.is_empty())
            .map(|s| (s, self.app.dw_style()));
        let sel = view.selected;
        let want_scroll = self.dir.as_ref().map(|d| d.scroll_to_sel).unwrap_or(false);
        let title = view.path.clone();
        let Some(d) = self.dir.as_mut() else {
            return;
        };
        if d.title != title {
            d.window.set_title(if title.is_empty() { "Directory" } else { &title });
            d.title = title;
        }
        if want_family != d.applied_family {
            match &want_family {
                Some((fam, st)) => {
                    crate::apply_egui_family(
                        &d.ctx,
                        self.app.font_db(),
                        fam,
                        &[egui::FontFamily::Monospace],
                        st.bold(),
                        st.italic(),
                    );
                }
                None => d.ctx.set_fonts(egui::FontDefinitions::default()),
            }
            d.applied_family = want_family;
        }
        d.scroll_to_sel = false;
        d.ctx.set_zoom_factor(scale);
        d.ctx.global_style_mut(|s| s.visuals.panel_fill = bg);
        let raw_input = d.state.take_egui_input(&d.window);
        let mut clicked: Option<usize> = None;
        let mut activated: Option<usize> = None;
        let mut right_clicked: Option<usize> = None;
        let mut page_rows = d.page_rows;
        let full_output = d.ctx.run_ui(raw_input, |ui| {
            egui::Panel::bottom("dir_footer").show_inside(ui, |ui| {
                ui.label(egui::RichText::new(footer.as_str()).size(font * 0.85).color(footer_col));
            });
            // Type-to-find: the matched prefix (a case-insensitive prefix of the entry name, which
            // begins each row) is highlighted `SELFGCOLOR`-on-`SELBGCOLOR` within the filename.
            let needle = view.find.to_lowercase();
            let needle_chars = needle.chars().count();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let avail = ui.available_height();
                let mut row_h = 0.0f32;
                for (i, line) in view.lines.iter().enumerate() {
                    let shown = if line.is_empty() { " " } else { line.as_str() };
                    // Byte end of the first `needle_chars` characters (a valid char boundary).
                    let split = shown
                        .char_indices()
                        .nth(needle_chars)
                        .map(|(b, _)| b)
                        .unwrap_or(shown.len());
                    let hit = needle_chars > 0
                        && shown[..split].chars().count() == needle_chars
                        && shown[..split].to_lowercase() == needle;
                    let label: egui::WidgetText = if hit {
                        use egui::text::{LayoutJob, TextFormat};
                        let font_id = egui::FontId::monospace(font);
                        let mut job = LayoutJob::default();
                        job.append(
                            &shown[..split],
                            0.0,
                            TextFormat { background: match_bg, ..TextFormat::simple(font_id.clone(), match_fg) },
                        );
                        job.append(&shown[split..], 0.0, TextFormat::simple(font_id, fg));
                        job.into()
                    } else {
                        egui::RichText::new(shown).monospace().size(font).color(fg).into()
                    };
                    let row = ui.selectable_label(i == sel, label);
                    row_h = row_h.max(row.rect.height());
                    if i == sel && want_scroll {
                        row.scroll_to_me(None);
                    }
                    if row.double_clicked() {
                        activated = Some(i);
                    } else if row.clicked() {
                        clicked = Some(i);
                    } else if row.secondary_clicked() {
                        right_clicked = Some(i);
                    }
                }
                if row_h > 0.0 {
                    page_rows = ((avail / row_h) as usize).saturating_sub(1).max(1);
                }
            });
        });
        d.page_rows = page_rows;
        d.state.handle_platform_output(&d.window, full_output.platform_output);
        let ppp = d.ctx.pixels_per_point();
        let prims = d.ctx.tessellate(full_output.shapes, ppp);
        let clear = egui::Rgba::from(d.ctx.global_style().visuals.panel_fill).to_array();
        d.painter.paint_and_update_textures(
            ViewportId::ROOT,
            ppp,
            clear,
            &prims,
            &full_output.textures_delta,
            vec![],
        );
        let delay = full_output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        d.repaint_at = if delay.is_zero() {
            d.window.request_redraw();
            None
        } else if delay < std::time::Duration::MAX {
            Some(Instant::now() + delay)
        } else {
            None
        };
        // Apply deferred mouse actions (outside the egui borrow).
        if let Some(i) = activated {
            self.app.dir_window_select_row(i);
            if self.app.dir_window_activate() {
                // Opened a file in the parent → bring the main window forward to show the new tab.
                if let Some(w) = self.window.as_ref() {
                    w.focus_window();
                }
                self.request_redraw();
            }
            if let Some(d) = self.dir.as_mut() {
                d.scroll_to_sel = true;
                d.window.request_redraw();
            }
        } else if let Some(i) = clicked {
            self.app.dir_window_select_row(i);
            if let Some(d) = self.dir.as_ref() {
                d.window.request_redraw();
            }
        } else if let Some(i) = right_clicked {
            // Right-click copies the clicked entry's full path to the clipboard. Selecting the
            // row updates the highlight; the main-window redraw flushes `$CB` out to the OS
            // clipboard (the dir window doesn't sync the clipboard itself).
            self.app.dir_window_select_row(i);
            self.app.dir_window_copy_path();
            self.request_redraw();
            if let Some(d) = self.dir.as_ref() {
                d.window.request_redraw();
            }
        }
    }

    /// Move the `DIR/W` selection by `delta` rows and redraw (the render scrolls it into view).
    fn dir_move_sel(&mut self, delta: isize) {
        self.app.dir_window_move(delta);
        if let Some(d) = self.dir.as_mut() {
            d.scroll_to_sel = true;
            d.window.request_redraw();
        }
    }

    /// Close the `DIR/W` window (× / Esc).
    fn close_dir(&mut self) {
        if let Some(d) = self.dir.as_ref() {
            self.dir_pos = d.window.outer_position().ok();
            self.dir_size = Some(d.window.inner_size());
            if self.app.remember_window_geometry() {
                save_geometry(&d.window, "dir-window.txt");
            }
        }
        self.app.close_dir_window();
        self.dir = None;
    }

    /// Persist the DIR window geometry now if `WINGEO` is on (called on live move/resize).
    fn save_dir_geometry_if_wingeo(&self) {
        if self.app.remember_window_geometry() {
            if let Some(d) = self.dir.as_ref() {
                save_geometry(&d.window, "dir-window.txt");
            }
        }
    }

    /// Create the Find/Replace window when `FIND` opens, close it when the dialog closes, or raise +
    /// focus it when re-invoked while open. Called each loop iteration (needs the `ActiveEventLoop`).
    fn sync_find(&mut self, event_loop: &ActiveEventLoop) {
        if !self.app.find_dialog_open() {
            if self.find.is_some() {
                self.close_find();
            }
            return;
        }
        if let Some(f) = self.find.as_ref() {
            if self.app.take_find_refocus() {
                f.window.focus_window();
                f.window.request_redraw();
            }
            return;
        }
        self.app.take_find_refocus(); // consume any stale request on a fresh open
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        // Restore geometry: disk (per WINGEO, clamped onto a real monitor) → this session → centred.
        let restored = self
            .app
            .remember_window_geometry()
            .then(|| load_geometry("find-window.txt"))
            .flatten()
            .map(|(lw, lh, x, y)| clamp_geometry(event_loop, lw, lh, x, y));
        let logical: LogicalSize<f64> = restored
            .map(|(lw, lh, _, _)| LogicalSize::new(lw, lh))
            .or_else(|| self.find_size.map(|p| p.to_logical(scale)))
            .unwrap_or(LogicalSize::new(620.0, 265.0));
        let size_phys: PhysicalSize<u32> = logical.to_physical(scale);
        let pos = restored
            .map(|(_, _, x, y)| PhysicalPosition::new(x, y))
            .or(self.find_pos)
            .or_else(|| {
                let main = self.window.as_ref()?;
                let mp = main.outer_position().ok()?;
                let ms = main.outer_size();
                Some(PhysicalPosition::new(
                    mp.x + (ms.width as i32 - size_phys.width as i32) / 2,
                    mp.y + (ms.height as i32 - size_phys.height as i32) / 2,
                ))
            });
        let attrs = Window::default_attributes()
            .with_title("Find / Replace")
            .with_inner_size(logical)
            .with_window_icon(window_icon());
        let window = Arc::new(event_loop.create_window(attrs).expect("create FIND window"));
        if let Some(p) = pos {
            window.set_outer_position(p);
        }
        let ctx = egui::Context::default();
        ctx.set_theme(egui::ThemePreference::Light);
        ctx.set_zoom_factor(1.45); // a bit bigger than the default — the compact form is otherwise tiny
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut painter = pollster::block_on(Painter::new(
            ctx.clone(),
            wgpu_config(),
            false,
            RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .expect("set FIND wgpu window");
        window.focus_window();
        window.request_redraw();
        self.find = Some(FindWindow { window, ctx, state, painter, repaint_at: None });
    }

    /// Render one FIND-window frame (the Find/Replace form), then apply its outcome: close the window,
    /// and/or redraw the MAIN window (a search moved the caret / changed the live highlights).
    fn redraw_find(&mut self) {
        // Take the window OUT so the render closure can borrow `self.app` (the two are disjoint).
        let Some(mut f) = self.find.take() else {
            return;
        };
        let raw_input = f.state.take_egui_input(&f.window);
        let mut outcome = crate::FindOutcome::default();
        let full_output = f.ctx.run_ui(raw_input, |ui| {
            outcome = self.app.find_window_ui(ui);
        });
        f.state.handle_platform_output(&f.window, full_output.platform_output);
        let ppp = f.ctx.pixels_per_point();
        let prims = f.ctx.tessellate(full_output.shapes, ppp);
        let clear = egui::Rgba::from(f.ctx.global_style().visuals.panel_fill).to_array();
        f.painter.paint_and_update_textures(
            ViewportId::ROOT,
            ppp,
            clear,
            &prims,
            &full_output.textures_delta,
            vec![],
        );
        let delay = full_output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        f.repaint_at = if delay.is_zero() {
            f.window.request_redraw();
            None
        } else if delay < std::time::Duration::MAX {
            Some(Instant::now() + delay)
        } else {
            None
        };
        self.find = Some(f); // put it back before close_find (which reads it) / the next frame
        if outcome.close {
            self.close_find();
        }
        if outcome.main_redraw {
            self.request_redraw();
        }
    }

    /// Close the FIND window: persist its geometry (per `WINGEO`), clear the app's FIND state, drop it.
    fn close_find(&mut self) {
        if let Some(f) = self.find.as_ref() {
            self.find_pos = f.window.outer_position().ok();
            self.find_size = Some(f.window.inner_size());
            if self.app.remember_window_geometry() {
                save_geometry(&f.window, "find-window.txt");
            }
        }
        self.app.close_find_state();
        self.find = None;
    }

    /// Persist the FIND window geometry now if `WINGEO` is on (called on live move/resize).
    fn save_find_geometry_if_wingeo(&self) {
        if self.app.remember_window_geometry() {
            if let Some(f) = self.find.as_ref() {
                save_geometry(&f.window, "find-window.txt");
            }
        }
    }

    /// Persist the MAIN window's geometry now if `WINGEO` is on — called on live move/resize (like the
    /// TXT/DIR/Find windows), so the disk copy is always current no matter HOW the app exits (X, QUIT,
    /// or a crash). The exit-time save (`exiting`) then just backstops it.
    fn save_main_geometry_if_wingeo(&self) {
        if self.app.remember_window_geometry() {
            if let Some(w) = self.window.as_ref() {
                save_geometry(w, "window.txt");
            }
        }
    }

    // ---- Scratchpad windows (SCRATCH / SCRATCHCLOSE / $SCRATCHTEXT) --------------------------------

    /// Reconcile scratchpad windows with the app's pending requests each loop: open one window per
    /// queued `SCRATCH`, and honor a `SCRATCHCLOSE`. Needs the `ActiveEventLoop` (window creation).
    fn sync_scratch(&mut self, event_loop: &ActiveEventLoop) {
        for init in self.app.take_scratch_open_queue() {
            self.open_scratch_window(event_loop, init);
        }
        if let Some(insert) = self.app.take_scratch_close_request() {
            // Close the most-recently-focused scratchpad, else the last opened.
            let target = self
                .scratch_focus
                .filter(|id| self.scratch.iter().any(|s| s.window.id() == *id))
                .or_else(|| self.scratch.last().map(|s| s.window.id()));
            if let Some(id) = target {
                self.close_scratch_window(id, insert);
            }
        }
    }

    /// Open one new floating scratchpad window, prefilled with `init` (empty, or the clipboard for
    /// `SCRATCH/V`). Session-only, cascaded from the main window's corner so several don't stack
    /// exactly; no geometry persistence (they're ephemeral parking spots).
    fn open_scratch_window(&mut self, event_loop: &ActiveEventLoop, init: String) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.next_scratch_num = self.next_scratch_num.saturating_add(1);
        let n = self.next_scratch_num;
        // Restore geometry: disk (per `WINGEO`, clamped onto a real monitor) → this session → a default
        // cascaded from the main window's corner. Mirrors the FIND window.
        let restored = self
            .app
            .remember_window_geometry()
            .then(|| load_geometry("scratch-window.txt"))
            .flatten()
            .map(|(lw, lh, x, y)| clamp_geometry(event_loop, lw, lh, x, y));
        let logical: LogicalSize<f64> = restored
            .map(|(lw, lh, _, _)| LogicalSize::new(lw, lh))
            .or_else(|| self.scratch_size.map(|p| p.to_logical(scale)))
            .unwrap_or(LogicalSize::new(500.0, 600.0));
        // Base position: restored → this session's last → cascade from the main window's top-left.
        let base = restored
            .map(|(_, _, x, y)| PhysicalPosition::new(x, y))
            .or(self.scratch_pos)
            .or_else(|| {
                let mp = self.window.as_ref()?.outer_position().ok()?;
                let off = (40.0 * scale) as i32;
                Some(PhysicalPosition::new(mp.x + off, mp.y + off))
            });
        // Cascade CONCURRENT windows (~24pt, wrapping at 8) so a second doesn't stack exactly on the
        // first; a lone scratchpad opens right where you left the last one.
        let step = ((self.scratch.len() as i32) % 8) * (24.0 * scale) as i32;
        let pos = base.map(|p| PhysicalPosition::new(p.x + step, p.y + step));
        let attrs = Window::default_attributes()
            .with_title(format!("Scratch {n}"))
            .with_inner_size(logical)
            .with_window_icon(window_icon());
        let window = Arc::new(event_loop.create_window(attrs).expect("create SCRATCH window"));
        if let Some(p) = pos {
            window.set_outer_position(p);
        }
        let ctx = egui::Context::default();
        ctx.set_theme(egui::ThemePreference::Light);
        let state = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut painter = pollster::block_on(Painter::new(
            ctx.clone(),
            wgpu_config(),
            false,
            RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .expect("set SCRATCH wgpu window");
        window.focus_window();
        window.request_redraw();
        self.scratch.push(ScratchWindow {
            window,
            ctx,
            state,
            painter,
            text: init,
            focus_pending: true,
            pending_text: String::new(),
            applied_family: None,
            repaint_at: None,
        });
    }

    /// Render one scratchpad frame: a full-window multiline `TextEdit` bound to its owned text.
    fn redraw_scratch(&mut self, id: WindowId) {
        let Some(idx) = self.scratch.iter().position(|s| s.window.id() == id) else {
            return;
        };
        // Install the editor font (`EDFAMILY`/`EDSTYLE`) on change — mapped into egui's Monospace slot,
        // which the TextEdit renders with; `None` falls back to egui's default.
        let want = self
            .app
            .scratch_font_family()
            .filter(|s| !s.is_empty())
            .map(|f| (f, self.app.scratch_font_style()));
        let size = self.app.scratch_font_size().max(6.0);
        // Optional `SCRATCHFGCOLOR`/`SCRATCHBGCOLOR` (`None` = egui's light-theme default).
        let fg = self.app.scratch_fg_color().map(|(r, g, b)| egui::Color32::from_rgb(r, g, b));
        let bg = self
            .app
            .scratch_bg_color()
            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
            .unwrap_or_else(|| egui::Visuals::light().panel_fill);
        // Take the window OUT so the render closure can own its `text` without borrowing `self`.
        let mut sw = self.scratch.remove(idx);
        // Background: set it every frame (reset to the theme default when the knob is cleared). Both
        // `panel_fill` (the window clear colour, for the margin) AND `extreme_bg_color` (the multiline
        // TextEdit's own fill) must change, or the text box paints its default white over the clear.
        sw.ctx.global_style_mut(|s| {
            s.visuals.panel_fill = bg;
            s.visuals.extreme_bg_color = bg;
        });
        if want != sw.applied_family {
            match &want {
                Some((fam, st)) => {
                    crate::apply_egui_family(
                        &sw.ctx,
                        self.app.font_db(),
                        fam,
                        &[egui::FontFamily::Monospace],
                        st.bold(),
                        st.italic(),
                    );
                }
                None => sw.ctx.set_fonts(egui::FontDefinitions::default()),
            }
            sw.applied_family = want;
        }
        let mut raw_input = sw.state.take_egui_input(&sw.window);
        // Inject any keyboard-file typing-layer text as a synthetic text event, so the TextEdit
        // receives it at its caret exactly like ordinary typing.
        if !sw.pending_text.is_empty() {
            raw_input
                .events
                .push(egui::Event::Text(std::mem::take(&mut sw.pending_text)));
        }
        let focus_now = std::mem::take(&mut sw.focus_pending);
        let ctx = sw.ctx.clone();
        let text = &mut sw.text;
        let full_output = ctx.run_ui(raw_input, |ui| {
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut edit = egui::TextEdit::multiline(text)
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::new(size, egui::FontFamily::Monospace));
                    if let Some(c) = fg {
                        edit = edit.text_color(c);
                    }
                    let resp = ui.add_sized(ui.available_size(), edit);
                    if focus_now {
                        resp.request_focus();
                    }
                });
        });
        sw.state
            .handle_platform_output(&sw.window, full_output.platform_output);
        let ppp = ctx.pixels_per_point();
        let prims = ctx.tessellate(full_output.shapes, ppp);
        let clear = egui::Rgba::from(ctx.global_style().visuals.panel_fill).to_array();
        sw.painter.paint_and_update_textures(
            ViewportId::ROOT,
            ppp,
            clear,
            &prims,
            &full_output.textures_delta,
            vec![],
        );
        let delay = full_output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(std::time::Duration::MAX);
        sw.repaint_at = if delay.is_zero() {
            sw.window.request_redraw();
            None
        } else if delay < std::time::Duration::MAX {
            Some(Instant::now() + delay)
        } else {
            None
        };
        // Put it back where it was (nothing else mutated the Vec during the render).
        self.scratch.insert(idx, sw);
    }

    /// Persist a scratchpad's geometry now: remember it for this session and, if `WINGEO` is on, to
    /// disk (`scratch-window.txt`) — called on live move/resize and on close, like the FIND window.
    fn save_scratch_geometry(&mut self, id: WindowId) {
        if let Some(sw) = self.scratch.iter().find(|s| s.window.id() == id) {
            self.scratch_pos = sw.window.outer_position().ok();
            self.scratch_size = Some(sw.window.inner_size());
            if self.app.remember_window_geometry() {
                save_geometry(&sw.window, "scratch-window.txt");
            }
        }
    }

    /// Close one scratchpad: stash its text into `$SCRATCHTEXT` (and, when `insert`, drop it into the
    /// active tab at the caret — `SCRATCHCLOSE/V`), then drop the window (and its surface).
    fn close_scratch_window(&mut self, id: WindowId, insert: bool) {
        if let Some(idx) = self.scratch.iter().position(|s| s.window.id() == id) {
            // Persist geometry (session + disk under WINGEO) before the window goes away.
            self.save_scratch_geometry(id);
            let sw = self.scratch.remove(idx);
            self.app.set_scratch_text(sw.text.clone());
            if insert {
                self.app.insert_scratch_at_caret(&sw.text);
                self.request_redraw(); // the main document changed
            }
            if self.scratch_focus == Some(id) {
                self.scratch_focus = None;
            }
            self.focused_windows.remove(&id);
        }
    }

    fn scratch_window_event(&mut self, id: WindowId, event: WindowEvent) {
        // Keyboard-file typing-layer awareness: run keys through the loaded keymap. A CAPTURED typing
        // cell (`insert "…"` — a custom layout, a text binder) types its .kbd text into the box;
        // command bindings do NOT run here (a plain text box has nothing for them to act on), so they
        // fall through to egui's default editing, and ordinary typing still uses the OS layout / IME.
        // This mirrors the main editor's typing decision (see `intercept_key`), so the scratchpad
        // types the same way under your keyboard file.
        if let WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. } = &event {
            if let Some(code) = key_event.physical_key.to_scancode().and_then(|sc| {
                let native = (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 };
                self.key.as_ref().and_then(|k| k.table.to_canonical(native))
            }) {
                match key_event.state {
                    ElementState::Pressed => {
                        // Resolve (also updates modifier state); a captured typing cell yields text.
                        let text = self.key.as_mut().and_then(|k| {
                            match k.resolver.resolve(KbdKey::down(code)) {
                                Disposition::Dispatch(xpl)
                                    if is_typing_cell(&xpl)
                                        && should_capture(code, &k.resolver, k.passthrough_mask) =>
                                {
                                    typing_cell_text(&xpl)
                                }
                                _ => None,
                            }
                        });
                        if let Some(t) = text {
                            if let Some(sw) = self.scratch.iter_mut().find(|s| s.window.id() == id) {
                                sw.pending_text.push_str(&t);
                                sw.window.request_redraw();
                            }
                            return; // consumed — don't let egui also type the OS-layout character
                        }
                        // Not a captured typing cell → fall through; egui handles the key.
                    }
                    ElementState::Released => {
                        // Balance the modifier state, then let egui see the release too.
                        if let Some(k) = self.key.as_mut() {
                            let _ = k.resolver.resolve(KbdKey::up(code));
                        }
                    }
                }
            }
        }
        match &event {
            WindowEvent::CloseRequested => {
                // The title-bar X discards (stash into `$SCRATCHTEXT`) — no insert.
                self.close_scratch_window(id, false);
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw_scratch(id);
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    if let Some(sw) = self.scratch.iter_mut().find(|s| s.window.id() == id) {
                        sw.painter.on_window_resized(ViewportId::ROOT, w, h);
                    }
                    self.save_scratch_geometry(id); // keep the WINGEO copy current
                }
                return;
            }
            WindowEvent::Moved(_) => {
                self.save_scratch_geometry(id);
                return;
            }
            WindowEvent::Focused(true) => {
                self.scratch_focus = Some(id);
                // fall through to egui
            }
            WindowEvent::Focused(false) => {
                // Drop momentary held modifiers so none stick in the shared resolver (mirrors the
                // FIND / main windows).
                if let Some(k) = self.key.as_mut() {
                    k.resolver.release_held();
                    k.consumed.clear();
                }
                // fall through to egui
            }
            _ => {}
        }
        // Forward the rest to egui: ordinary OS-layout typing, mouse caret/selection, IME, clipboard,
        // and any key that wasn't a captured .kbd typing cell.
        if let Some(sw) = self.scratch.iter_mut().find(|s| s.window.id() == id) {
            let resp = sw.state.on_window_event(&sw.window, &event);
            if resp.repaint {
                sw.window.request_redraw();
            }
        }
    }

    fn find_window_event(&mut self, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                self.close_find();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw_find();
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.find_size = Some(*size);
                    if let Some(f) = self.find.as_mut() {
                        f.painter.on_window_resized(ViewportId::ROOT, w, h);
                    }
                    self.save_find_geometry_if_wingeo();
                }
            }
            WindowEvent::Moved(pos) => {
                self.find_pos = Some(*pos);
                self.save_find_geometry_if_wingeo();
            }
            // Losing focus (Alt+Tab): drop momentary held modifiers so none stick in the shared resolver.
            WindowEvent::Focused(false) => {
                if let Some(k) = self.key.as_mut() {
                    k.resolver.release_held();
                    k.consumed.clear();
                }
            }
            // Keep the shared modifier state fresh while the FIND window holds focus — otherwise
            // `self.mods` (updated only from the MAIN window) is stale here, so `Shift+F3` below read
            // Shift as up and did Find NEXT instead of Find Prev.
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
            }
            // F3 / Shift+F3 repeat the last search from the FIND window too, driving the main document.
            WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. }
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::F3) =>
            {
                self.app.find_again(self.mods.shift_key());
                self.request_redraw();
                if let Some(f) = self.find.as_ref() {
                    f.window.request_redraw();
                }
                return;
            }
            _ => {}
        }
        // Everything else (typing, mouse, Enter, Esc, Tab) goes to egui so the fields + buttons work.
        if let Some(f) = self.find.as_mut() {
            let resp = f.state.on_window_event(&f.window, &event);
            if resp.repaint {
                f.window.request_redraw();
            }
        }
    }

    /// Resolve a `DIR/W` key through the shared keymap resolver — feeding BOTH edges so modifier
    /// state stays honest — and return the XPL it dispatches on the DOWN edge (if any). Lets a key
    /// BOUND to the ENTER verb activate the selection here, like the main window's `intercept_key`.
    fn dir_window_keymap_dispatch(&mut self, ev: &winit::event::KeyEvent) -> Option<String> {
        let key = self.key.as_mut()?;
        let sc = ev.physical_key.to_scancode()?;
        let native = (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 };
        let code = key.table.to_canonical(native)?;
        match ev.state {
            ElementState::Pressed => {
                if let Disposition::Dispatch(xpl) = key.resolver.resolve(KbdKey::down(code)) {
                    key.consumed.insert(code);
                    return Some(xpl);
                }
                None
            }
            ElementState::Released => {
                let _ = key.resolver.resolve(KbdKey::up(code));
                key.consumed.remove(&code);
                None
            }
        }
    }

    /// Handle a window event addressed to the `DIR/W` window.
    fn dir_window_event(&mut self, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                self.close_dir();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw_dir();
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    self.dir_size = Some(*size);
                    if let Some(d) = self.dir.as_mut() {
                        d.painter.on_window_resized(ViewportId::ROOT, w, h);
                    }
                    self.save_dir_geometry_if_wingeo();
                }
            }
            WindowEvent::Moved(pos) => {
                self.dir_pos = Some(*pos);
                self.save_dir_geometry_if_wingeo();
            }
            // Returning focus to the window auto-refreshes the listing (if the dir changed).
            WindowEvent::Focused(true) => {
                self.app.dir_window_refresh_if_changed();
                if let Some(d) = self.dir.as_ref() {
                    d.window.request_redraw();
                }
            }
            // Losing focus (e.g. Alt+Tab): drop momentary held modifiers so none stick in the
            // shared resolver (mirrors the main window's reset).
            WindowEvent::Focused(false) => {
                if let Some(k) = self.key.as_mut() {
                    k.resolver.release_held();
                    k.consumed.clear();
                }
            }
            WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. }
                if key_event.state == ElementState::Pressed =>
            {
                // Keymap first: a key BOUND to the ENTER verb (not just the physical Enter)
                // activates the selection here too, matching the main window.
                if self
                    .dir_window_keymap_dispatch(key_event)
                    .as_deref()
                    .is_some_and(|c| c.trim().eq_ignore_ascii_case("ENTER"))
                {
                    if self.app.dir_window_activate() {
                        if let Some(w) = self.window.as_ref() {
                            w.focus_window();
                        }
                        self.request_redraw();
                    }
                    if let Some(d) = self.dir.as_mut() {
                        d.scroll_to_sel = true;
                        d.window.request_redraw();
                    }
                    return;
                }
                let page = self.dir.as_ref().map(|d| d.page_rows as isize).unwrap_or(1);
                let find_active =
                    self.app.dir_window_view().map(|v| !v.find.is_empty()).unwrap_or(false);
                let handled = match &key_event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if find_active {
                            self.app.dir_window_find_clear();
                        } else {
                            self.close_dir();
                            return;
                        }
                        true
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.dir_move_sel(1);
                        true
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.dir_move_sel(-1);
                        true
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.dir_move_sel(page);
                        true
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.dir_move_sel(-page);
                        true
                    }
                    Key::Named(NamedKey::Home) => {
                        self.app.dir_window_to_edge(false);
                        true
                    }
                    Key::Named(NamedKey::End) => {
                        self.app.dir_window_to_edge(true);
                        true
                    }
                    Key::Named(NamedKey::Enter) => {
                        if self.app.dir_window_activate() {
                            if let Some(w) = self.window.as_ref() {
                                w.focus_window();
                            }
                            self.request_redraw();
                        }
                        true
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.app.dir_window_find_backspace();
                        true
                    }
                    Key::Named(NamedKey::F3) => {
                        self.app.dir_window_find_again();
                        true
                    }
                    Key::Character(s) => {
                        // Type-to-find: feed printable characters to the express-key finder.
                        for ch in s.chars() {
                            self.app.dir_window_find_push(ch);
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    if let Some(d) = self.dir.as_mut() {
                        d.scroll_to_sel = true;
                        d.window.request_redraw();
                    }
                    return;
                }
            }
            // Up / other key edges: feed the resolver so modifier state stays honest.
            WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. } => {
                let _ = self.dir_window_keymap_dispatch(key_event);
            }
            _ => {}
        }
        // Forward anything else (mouse, wheel, focus-lost) to the window's egui state.
        if let Some(d) = self.dir.as_mut() {
            let _ = d.state.on_window_event(&d.window, &event);
            d.window.request_redraw();
        }
    }
}

/// The per-user session directory (`%APPDATA%\xyscript\session` on Windows, `~/.config/xyscript/
/// session` elsewhere) — where `SESSION` stores its manifest and shadow backups. Reuses the same base
/// as `geometry_path`. `None` if no per-user dir is resolvable.
pub(crate) fn session_dir() -> Option<PathBuf> {
    geometry_path("session")
}

/// The wgpu configuration used for every window (main + TXT/DIR/FIND). On Windows we default the
/// backend to **DX12**: wgpu's automatic selection picks a Vulkan/GL adapter on some machines whose
/// driver then fails at surface creation — the window opens and instantly vanishes (a 2026-07 field
/// report on a Windows 11 box; DX12 worked, Vulkan didn't). DX12 is the reliable Windows backend. An
/// explicit `WGPU_BACKEND` still wins — the default setup already reads it — so we only force DX12
/// when the user hasn't chosen a backend. Off Windows this is exactly `WgpuConfiguration::default()`.
fn wgpu_config() -> WgpuConfiguration {
    #[allow(unused_mut)]
    let mut cfg = WgpuConfiguration::default();
    #[cfg(windows)]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        if let egui_wgpu::WgpuSetup::CreateNew(new) = &mut cfg.wgpu_setup {
            new.instance_descriptor.backends = egui_wgpu::wgpu::Backends::DX12;
        }
    }
    cfg
}

/// Fatal graphics-init failure (no usable GPU adapter/surface): show a native dialog explaining the
/// fix, then exit — instead of a bare panic that vanishes the window with no message. `os_message::show`
/// spawns a DETACHED box (a separate process), so exiting immediately still leaves it on screen.
fn graphics_init_failed(detail: &str) -> ! {
    // The useful remedy differs sharply by platform — and generic "try dx12" advice is actively wrong
    // off Windows (dx12 is a Windows-only backend), so tailor the suggestions to the actual OS.
    #[cfg(windows)]
    let advice = "\u{2022} Update your graphics driver (Intel / AMD / NVIDIA), then restart.\n\
         \u{2022} Or pick a different renderer: set the environment variable WGPU_BACKEND to dx12 \
         (or vulkan, or gl) before starting XyScript.";
    #[cfg(target_os = "linux")]
    let advice = "\u{2022} Install the Vulkan driver for your GPU, then restart. On Debian/Ubuntu:\n    \
         sudo apt install mesa-vulkan-drivers\n  \
         (Fedora: mesa-vulkan-drivers  \u{2022}  Arch: vulkan-intel / vulkan-radeon.)\n\
         \u{2022} If you run Wayland, try the OpenGL renderer under X11:\n    \
         WINIT_UNIX_BACKEND=x11 WGPU_BACKEND=gl xyscript\n\
         \u{2022} Old GPU with no Vulkan (pre-2014 Intel, etc.)? Use SOFTWARE rendering:\n    \
         LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=gl xyscript\n  \
         (or install Mesa lavapipe for software Vulkan  \u{2022}  Arch: vulkan-swrast.)\n\
         \u{2022} Otherwise update Mesa / your GPU driver and restart.";
    #[cfg(target_os = "macos")]
    let advice = "\u{2022} Update macOS, then restart.\n\
         \u{2022} Or pick a different renderer: set the environment variable WGPU_BACKEND to metal \
         (or gl) before starting XyScript.";
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    let advice = "\u{2022} Update your graphics driver, then restart.\n\
         \u{2022} Or set the environment variable WGPU_BACKEND to vulkan (or gl) before starting XyScript.";

    xyscript_app::os_message::show(
        Some("XyScript — graphics error"),
        &format!(
            "XyScript couldn't start the graphics system on this PC.\n\n\
             This is almost always a display-driver problem. Things to try:\n\
             {advice}\n\n\
             Technical detail: {detail}"
        ),
    );
    std::process::exit(1);
}

/// Per-user path for a window-geometry file (`window.txt`, `find-window.txt`, `dir-window.txt`,
/// `txt-window.txt`): `%APPDATA%\xyscript\<name>` on Windows, `$XDG_CONFIG_HOME`/`~/.config/xyscript/
/// <name>` elsewhere. `None` if no per-user dir is resolvable.
fn geometry_path(name: &str) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    let dir = base.join("xyscript");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(name))
}

/// The application window icon, decoded from the bundled 256×256 PNG into winit's RGBA `Icon`.
///
/// Cross-platform reach: this sets the icon shown in the title bar / taskbar on **Windows** and on
/// **Linux under X11** (via `_NET_WM_ICON`). It is a **no-op on macOS** (which has no per-window
/// icon — the Dock icon comes from the `.app` bundle's `.icns`) and is **ignored under Wayland**
/// (which matches a `.desktop` file by app-id instead). The Windows *executable* icon is embedded
/// separately at build time (see `build.rs`). Returns `None` if the PNG fails to decode, which winit
/// treats as "no icon".
fn window_icon() -> Option<winit::window::Icon> {
    let bytes = include_bytes!("../assets/icon-256.png");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (w, h) = (img.width(), img.height());
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Load a remembered window geometry `(lw, lh, x, y)` from `<name>` — **logical** size (DPI-independent
/// points) + **physical** top-left position (absolute desktop px). Returns `None` on first run, a
/// missing/unparseable file, an old physical-px format (`v1`, no longer used), or degenerate/off-screen
/// values. Storing the size logically is what makes a window restored on a different-DPI monitor keep
/// the same APPARENT size instead of blowing up (winit re-applies the target monitor's scale factor).
fn load_geometry(name: &str) -> Option<(f64, f64, i32, i32)> {
    let s = std::fs::read_to_string(geometry_path(name)?).ok()?;
    let mut it = s.split_whitespace();
    if it.next()? != "v2" {
        return None; // older physical-px file or foreign format — fall back to the default size
    }
    let lw: f64 = it.next()?.parse().ok()?;
    let lh: f64 = it.next()?.parse().ok()?;
    let x: i32 = it.next()?.parse().ok()?;
    let y: i32 = it.next()?.parse().ok()?;
    if !(lw.is_finite() && lh.is_finite())
        || lw < 100.0
        || lh < 80.0
        || x < -20000
        || y < -20000
        || x > 20000
        || y > 20000
    {
        return None;
    }
    Some((lw, lh, x, y))
}

/// Persist a window's current size + position to `<name>`. Size is stored in LOGICAL points (physical ÷
/// this monitor's scale factor) so it restores at the same apparent size on any-DPI display; position
/// stays physical (absolute desktop coords, so it reopens on the same monitor). Skips a minimized
/// window (Windows reports a far off-screen position then), so the last good geometry is kept.
fn save_geometry(window: &Window, name: &str) {
    let Some(path) = geometry_path(name) else {
        return;
    };
    let size = window.inner_size();
    let Ok(pos) = window.outer_position() else {
        return;
    };
    if size.width < 100 || size.height < 80 || pos.x < -20000 || pos.y < -20000 {
        return; // minimized / degenerate — keep the previous saved geometry
    }
    let logical: winit::dpi::LogicalSize<f64> = size.to_logical(window.scale_factor());
    let _ = std::fs::write(
        path,
        format!("v2 {:.1} {:.1} {} {}\n", logical.width, logical.height, pos.x, pos.y),
    );
}

/// Clamp a restored geometry so the window fits on an actual display. A size larger than the
/// surface's max texture extent makes `Surface::configure` panic (wgpu validation), and a window
/// bigger than its monitor is never useful — so cap each dimension to the target monitor's size and
/// keep the top-left corner on that monitor. `GPU_MAX_EXTENT` is a conservative GPU safety net for
/// the rare monitor that exceeds it. Works in LOGICAL points for size (`lw`,`lh`): the caps are the
/// target monitor's logical size and `GPU_MAX_EXTENT ÷ scale` (the surface limit is on physical px).
/// Returns the adjusted `(lw, lh, x, y)`.
fn clamp_geometry(
    event_loop: &ActiveEventLoop,
    lw: f64,
    lh: f64,
    x: i32,
    y: i32,
) -> (f64, f64, i32, i32) {
    const GPU_MAX_EXTENT: f64 = 8192.0;
    // Pick the monitor containing the top-left corner, else the primary, else any.
    let monitor = event_loop
        .available_monitors()
        .find(|m| {
            let p = m.position();
            let s = m.size();
            x >= p.x
                && y >= p.y
                && x < p.x + s.width as i32
                && y < p.y + s.height as i32
        })
        .or_else(|| event_loop.primary_monitor())
        .or_else(|| event_loop.available_monitors().next());
    let (mon_phys_w, mon_phys_h, scale, mp) = match &monitor {
        Some(m) => (m.size().width, m.size().height, m.scale_factor(), m.position()),
        None => (GPU_MAX_EXTENT as u32, GPU_MAX_EXTENT as u32, 1.0, PhysicalPosition::new(0, 0)),
    };
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let max_logical_w = mon_phys_w as f64 / scale;
    let max_logical_h = mon_phys_h as f64 / scale;
    let gpu_logical = GPU_MAX_EXTENT / scale;
    let lw = lw.min(max_logical_w).min(gpu_logical).max(100.0);
    let lh = lh.min(max_logical_h).min(gpu_logical).max(100.0);
    // Keep the window's top-left within the monitor (using its resulting physical extent).
    let win_phys_w = (lw * scale) as i32;
    let win_phys_h = (lh * scale) as i32;
    let x = x.clamp(mp.x, mp.x + (mon_phys_w as i32 - win_phys_w).max(0));
    let y = y.clamp(mp.y, mp.y + (mon_phys_h as i32 - win_phys_h).max(0));
    (lw, lh, x, y)
}

/// Windows-only: force a freshly-created window to the foreground. A `DUPVIEW/W` second instance is
/// spawned by a process that is itself the foreground app, so Windows' foreground lock can leave the
/// new window unfocused — and since the child then goes straight to `Focused(true)` (never a
/// `Focused(false)` first), the keyboard-state reset that arms on focus change wouldn't fire, leaving
/// the keyboard "frozen" until you clicked the window by hand. Bringing it to the foreground here makes
/// the child receive `Focused(true)` immediately, so that reset runs and the keyboard works at once.
#[cfg(windows)]
mod winfocus {
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, attach: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn BringWindowToTop(hwnd: isize) -> i32;
        fn SetFocus(hwnd: isize) -> isize;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    /// Force `hwnd` to the foreground even under Windows' foreground lock, by briefly attaching to the
    /// current foreground thread's input queue (the classic bypass). Timing-independent — needs no
    /// prior `AllowSetForegroundWindow` grant from the spawner, so a slow-booting child still wins.
    pub fn force_foreground(hwnd: isize) {
        unsafe {
            let fg = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg, std::ptr::null_mut());
            let cur = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != cur && AttachThreadInput(cur, fg_thread, 1) != 0;
            const SW_SHOW: i32 = 5;
            ShowWindow(hwnd, SW_SHOW);
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
            if attached {
                AttachThreadInput(cur, fg_thread, 0);
            }
        }
    }
}

impl ApplicationHandler for Handler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // already initialized (e.g. resume after suspend)
        }
        let mut attrs = Window::default_attributes().with_title("XyScript").with_window_icon(window_icon());
        // Restore the remembered size + position (when `WINGEO on`); fall back to a sensible default
        // on first run, when remembering is off, or when no valid geometry is saved.
        let restored = if self.app.remember_window_geometry() {
            load_geometry("window.txt")
        } else {
            None
        };
        let mut restored_pos: Option<PhysicalPosition<i32>> = None;
        let mut restored_size: Option<LogicalSize<f64>> = None;
        if let Some((lw, lh, x, y)) = restored {
            let (lw, lh, x, y) = clamp_geometry(event_loop, lw, lh, x, y);
            // Size is LOGICAL: winit converts it with the target monitor's scale factor, so the
            // window keeps its apparent size across different-DPI displays. Position stays physical.
            attrs = attrs
                .with_inner_size(LogicalSize::new(lw, lh))
                .with_position(PhysicalPosition::new(x, y));
            restored_pos = Some(PhysicalPosition::new(x, y));
            restored_size = Some(LogicalSize::new(lw, lh));
        } else {
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
        }
        // Honour a `TITLEBAR off` (or Writer's View) restored from config at startup — create the
        // window already borderless, so we avoid a decorated→borderless flash AND the size bug it
        // causes: toggling decorations OFF after creation makes Windows keep the OUTER size and grow
        // the CLIENT by the title-bar height, so the restored window came back ~a title bar too tall.
        // The effective state MUST match the per-frame sync below (`show_titlebar && !writer_view`).
        self.decorations = self.app.show_titlebar() && !self.app.writer_view();
        attrs = attrs.with_decorations(self.decorations);
        // Linux: set a stable X11 WM_CLASS and Wayland app-id of "xyscript". This is what a
        // compositor matches against `xyscript.desktop` (StartupWMClass=xyscript) to give the
        // window its taskbar/dock icon — essential on Wayland, which ignores `with_window_icon`.
        // UFCS avoids the ambiguity of two extension traits both offering `with_name`.
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = WindowAttributesExtX11::with_name(attrs, "xyscript", "xyscript");
            attrs = WindowAttributesExtWayland::with_name(attrs, "xyscript", "xyscript");
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        // Multi-monitor: the `with_position` creation hint is unreliable across monitors on Windows
        // (the window can land on the primary display instead of the saved one). Re-assert the saved
        // physical position AFTER creation — the same thing the TXT/DIR/Find windows do — so it lands
        // back on the monitor it was closed on.
        if let Some(p) = restored_pos {
            let _ = window.set_outer_position(p);
        }
        // Re-assert the saved CLIENT size after creation, as a cheap guard against the `with_inner_size`
        // creation hint being computed against a transitional frame on Windows. (The main title-bar
        // growth bug is fixed at the decoration toggle itself — see the `set_decorations` sync — but
        // pinning inner here keeps the restore exact regardless of decoration/DPI accounting.)
        if let Some(sz) = restored_size {
            let _ = window.request_inner_size(sz);
        }
        // Claim keyboard focus. For a `DUPVIEW/W` second instance (spawned by another XyScript that is
        // the foreground app) Windows can otherwise leave the new window unfocused. `focus_window()`
        // alone is unreliable (the parent's foreground grant expires while a slow startup runs), so a
        // spawned child (identified by the `DUPVIEW/W` args) also forces the foreground via
        // AttachThreadInput — see `winfocus`. A normal launch relies on the OS, as before.
        window.focus_window();
        #[cfg(windows)]
        if std::env::args().any(|a| a == "--goto" || a == "--cwd") {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Some(RawWindowHandle::Win32(h)) = window.window_handle().ok().map(|h| h.as_raw()) {
                winfocus::force_foreground(h.hwnd.get());
            }
        }

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let mut painter = pollster::block_on(Painter::new(
            self.egui_ctx.clone(),
            wgpu_config(),
            false,
            RendererOptions::default(),
        ));
        // No usable GPU adapter/surface → tell the user how to fix it and exit cleanly, rather than
        // panicking (which just vanishes the window a second after it appears).
        if let Err(e) = pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
        {
            graphics_init_failed(&e.to_string());
        }

        self.window = Some(window.clone());
        self.egui_state = Some(egui_state);
        self.painter = Some(painter);
        window.request_redraw();
    }

    /// The event loop is exiting (window close or `QUIT`). Remember the window geometry so the next
    /// run restores it. Called once, while the window still exists, for either exit path.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Final session flush on graceful exit — captures edits since the last periodic snapshot.
        // Every quit route reaches here; independent of the window-geometry (`WINGEO`) setting below.
        self.app.snapshot_session();
        if !self.app.remember_window_geometry() {
            return; // user opted out — leave any previous window.txt untouched
        }
        if let Some(window) = self.window.as_ref() {
            save_geometry(window, "window.txt");
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.window.is_none() {
            return;
        }
        // Track focus across ALL our windows (main / TXT / DIR-W / Find) so `DIMONBLUR` dims the main
        // window only when the WHOLE app is unfocused — not when focus merely moved to one of our own
        // child windows. Runs for every window before the per-window dispatch below (which still gets
        // the event). Redraw the main window so its dim state updates; the RedrawRequested is processed
        // after this and any paired Focused event, so a main→child switch never flashes the dim.
        if let WindowEvent::Focused(f) = &event {
            if *f {
                self.focused_windows.insert(id);
            } else {
                self.focused_windows.remove(&id);
            }
            // Prune ids for windows that no longer exist: a child window closed WHILE focused may not
            // emit `Focused(false)`, and a stale id would otherwise keep the app "focused" forever.
            let valid: HashSet<WindowId> = [
                self.window.as_ref().map(|w| w.id()),
                self.txt.as_ref().map(|t| t.window.id()),
                self.dir.as_ref().map(|d| d.window.id()),
                self.find.as_ref().map(|f| f.window.id()),
            ]
            .into_iter()
            .flatten()
            .chain(self.scratch.iter().map(|s| s.window.id()))
            .collect();
            self.focused_windows.retain(|id| valid.contains(id));
            self.app.set_app_focus(!self.focused_windows.is_empty());
            self.request_redraw();
        }
        // Events for the separate TXT window go to its own handler.
        if self.txt.as_ref().is_some_and(|t| t.window.id() == id) {
            self.txt_window_event(event);
            return;
        }
        // Events for the separate DIR/W window go to its own handler.
        if self.dir.as_ref().is_some_and(|d| d.window.id() == id) {
            self.dir_window_event(event);
            return;
        }
        // Events for the separate Find/Replace window go to its own handler.
        if self.find.as_ref().is_some_and(|f| f.window.id() == id) {
            self.find_window_event(event);
            return;
        }
        // Events for a floating scratchpad window go to its own handler.
        if self.scratch.iter().any(|s| s.window.id() == id) {
            self.scratch_window_event(id, event);
            return;
        }
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                if self.app.should_exit() {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::Resized(size) => {
                // Reconcile immediately (redraw also does, as the safety net for DPI changes whose
                // resize is skipped/reordered — see `ensure_surface`).
                self.ensure_surface(size.width, size.height);
                // Persist the new size live (per `WINGEO`), so the disk copy is current for ANY exit.
                self.save_main_geometry_if_wingeo();
                // fall through so egui updates its screen rect too
            }
            // Persist the new position live (per `WINGEO`) — the main window, like the TXT/DIR/Find
            // windows, now saves on every move, so QUIT and X both restore the last spot.
            WindowEvent::Moved(_) => {
                self.save_main_geometry_if_wingeo();
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                // fall through to egui
            }
            WindowEvent::KeyboardInput { event: key_event, is_synthetic: false, .. } => {
                // Ctrl+Break: abort a runaway/hung script in an orderly way. Checked FIRST — ahead
                // of the keymap, READKEY capture, and egui focus — so it works even while a script
                // is parked or actively looping. The physical Pause/Break key + Ctrl is robust
                // across layouts (Ctrl+Pause = VK_CANCEL on Windows, still physical-key Pause).
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::Pause)
                    && self.mods.control_key()
                {
                    if self.app.is_script_running() {
                        self.app.request_script_abort();
                        self.request_redraw();
                    }
                    return; // consumed: don't let it reach the keymap/egui
                }
                // Ctrl+Z / Ctrl+D in a RUNNING Console = end-of-input: close the child's stdin (EOF) so
                // a filter like `sort` finishes. Gated on the Console being active AND a command running
                // with interactive stdin, so Ctrl+Z stays Undo (etc.) everywhere else and even in the
                // Console when nothing is running.
                if key_event.state == ElementState::Pressed
                    && self.mods.control_key()
                    && matches!(
                        key_event.physical_key,
                        PhysicalKey::Code(KeyCode::KeyZ) | PhysicalKey::Code(KeyCode::KeyD)
                    )
                    && self.app.editor.console_is_active()
                    && self.app.editor.console_job_running()
                {
                    self.app.editor.console_stdin_eof();
                    self.request_redraw();
                    return; // consumed
                }
                // Ctrl+C in a RUNNING Console = interrupt (kill) the command, like a terminal SIGINT.
                // Gated the same way, so Ctrl+C stays Copy everywhere else (and in the idle Console).
                if key_event.state == ElementState::Pressed
                    && self.mods.control_key()
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::KeyC)
                    && self.app.editor.console_is_active()
                    && self.app.editor.console_command_running()
                {
                    self.app.editor.console_interrupt();
                    self.request_redraw();
                    return; // consumed
                }
                // Shift+Up / Shift+Down in the Console recall the session command history into the
                // prompt. Consumed only when the Console handles it (a prompt is showing, the caret is
                // on the input line, and there is somewhere to go); otherwise it falls through to the
                // normal Shift+arrow (extend the selection).
                if key_event.state == ElementState::Pressed
                    && self.mods.shift_key()
                    && !self.mods.control_key()
                    && !self.mods.alt_key()
                    && matches!(
                        key_event.physical_key,
                        PhysicalKey::Code(KeyCode::ArrowUp) | PhysicalKey::Code(KeyCode::ArrowDown)
                    )
                    && self.app.editor.console_is_active()
                {
                    let older = key_event.physical_key == PhysicalKey::Code(KeyCode::ArrowUp);
                    if self.app.editor.console_history_recall(older) {
                        self.request_redraw();
                        return; // consumed
                    }
                }
                // In a directory listing, the express-find control keys (F3 = next match, Backspace
                // = shorten the type-to-find query) must run AHEAD of the keymap — otherwise a bound
                // command key eats them (e.g. `Backspace` = `BD`). `dir_express_control` consumes
                // them only inside a listing (and not during a delete-confirm).
                if key_event.state == ElementState::Pressed {
                    let dir_ctrl = match key_event.physical_key {
                        PhysicalKey::Code(KeyCode::F3) => Some(true),
                        PhysicalKey::Code(KeyCode::Backspace) => Some(false),
                        _ => None,
                    };
                    if let Some(find_again) = dir_ctrl {
                        if self.app.dir_express_control(find_again) {
                            self.request_redraw();
                            return; // consumed by the listing
                        }
                    }
                }
                // F3 / Shift+F3 = Find Next / Find Prev — but only as a FALLBACK. A keymap binding for
                // F3 (e.g. the shipped `F3=XD`) must win when the document owns the keyboard, so this
                // fires only when F3 is UNBOUND at the current modifiers, or when a text edit (the
                // command line / a picker field) owns the keyboard and the keymap path is skipped
                // below. Otherwise F3 falls through to the keymap and runs its binding. Not in a dir
                // listing (F3 there is express-find, handled above / by the listing).
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::F3)
                    && !self.app.active_is_dir_listing()
                {
                    // Is F3 bound in the keymap at the live modifier state? (physical → native → canonical
                    // code, then a pure keymap lookup — no resolver side effects.)
                    let f3_bound = self.key.as_ref().is_some_and(|k| {
                        key_event
                            .physical_key
                            .to_scancode()
                            .map(|sc| (sc & 0xFF) | if (sc >> 8) == 0xE0 { WIN_E0 } else { 0 })
                            .and_then(|native| k.table.to_canonical(native))
                            .is_some_and(|code| {
                                k.resolver.keymap.resolve(k.resolver.state.current_bitmask(), code).is_some()
                            })
                    });
                    if self.egui_ctx.text_edit_focused() || !f3_bound {
                        self.app.find_again(self.mods.shift_key());
                        self.request_redraw();
                        return; // consumed
                    }
                    // bound + document focused → fall through to the keymap, which runs the binding.
                }
                // When an egui text box has keyboard focus (e.g. the Find dialog field or the
                // font-picker size field), let egui handle editing keys (arrows/Backspace/Ctrl+A/…)
                // — the keymap must not intercept them. The main editor canvas isn't an egui
                // TextEdit, so this is false there and the keymap captures as usual.
                if self.egui_ctx.text_edit_focused() {
                    // The resolver is dormant while a text box owns the keyboard — so a modifier
                    // RELEASE now goes to egui, not the resolver, and a modifier held when the box
                    // gained focus would otherwise stay "down" forever (every later key then routes
                    // through its layer — looks like the keymap changed / "F5 quits"). Drop the
                    // resolver's momentary held state so the canvas starts clean once focus returns.
                    // (Latched/Caps is preserved by `release_held`.)
                    if let Some(k) = self.key.as_mut() {
                        k.resolver.release_held();
                        k.consumed.clear();
                    }
                    // fall through to egui
                } else if self.intercept_key(key_event) {
                    self.request_redraw();
                    return; // consumed: egui must not also process it
                }
                // not consumed: fall through and forward to egui (typing/IME/etc.)
            }
            WindowEvent::Focused(false) => {
                // Lost keyboard focus (e.g. Alt+Tab away): the key-UP edges for anything held now
                // go to the newly-focused window, never to us, so a held modifier (Alt/Ctrl/Shift/
                // HOTKEY) would stay stuck "down" here and misroute every later key — assignments
                // appear "lost" and keys fall through to the OS. Drop momentary held state so we
                // return to a clean slate. (Latched/Caps is kept.)
                if let Some(k) = self.key.as_mut() {
                    k.resolver.release_held();
                    k.consumed.clear();
                }
                // fall through to egui
            }
            WindowEvent::Focused(true) => {
                // Gaining focus — including a freshly-spawned DUPVIEW/W window's FIRST focus, which
                // never saw a Focused(false) to clean up. Reset momentary held state so a modifier left
                // looking "stuck" from before we had focus can't misroute every key (the observed
                // keyboard-frozen-until-you-click-away symptom on a `DUPVIEW/W` window). Latched/Caps is
                // preserved by `release_held`. Symmetric with the Focused(false) cleanup above.
                if let Some(k) = self.key.as_mut() {
                    k.resolver.release_held();
                    k.consumed.clear();
                }
                // fall through to egui
            }
            _ => {}
        }

        let window = match &self.window {
            Some(w) => w.clone(),
            None => return,
        };
        if let Some(state) = self.egui_state.as_mut() {
            let resp = state.on_window_event(&window, &event);
            if resp.repaint {
                window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Native file dialogs. On Windows the dialog runs on its OWN thread, started non-blocking here:
        // the main loop keeps pumping while it's open (so the owner-modal cross-thread handshake can't
        // deadlock — a blocking join did), and the picked path is delivered back over a channel and
        // applied on a later `about_to_wait`. wgpu's DXGI factory hooks the main thread's message queue,
        // which stops a same-thread modal dialog from re-enumerating on a filter change; its own thread
        // has a clean queue. Elsewhere, dialogs MUST run on the main thread, so they run inline (as
        // before — outside the egui frame so they can't re-enter `run_ui` and deadlock egui's Context).
        if let Some(window) = self.window.clone() {
            // Deliver a finished dialog first.
            if let Some(job) = self.dialog_job.as_ref() {
                match job.rx.try_recv() {
                    Ok(picked) => {
                        let job = self.dialog_job.take().unwrap();
                        self.app.apply_dialog_result(job.req, picked);
                        window.request_redraw();
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => self.dialog_job = None,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {} // still open
                }
            }
            // Start a new one only when none is in flight (owner-modal already blocks the UI that would
            // request another, but guard anyway).
            if self.dialog_job.is_none() {
                if let Some(req) = self.app.take_pending_dialog() {
                    let default_typst = self.app.active_is_typst();
                    #[cfg(windows)]
                    {
                        let hwnd = main_hwnd(window.as_ref());
                        let (tx, rx) = std::sync::mpsc::channel();
                        let req_thread = req.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(run_file_dialog_threaded(&req_thread, hwnd, default_typst));
                        });
                        self.dialog_job = Some(DialogJob { rx, req });
                    }
                    #[cfg(not(windows))]
                    {
                        let picked = run_file_dialog(&req, window.as_ref(), default_typst);
                        self.app.apply_dialog_result(req, picked);
                        window.request_redraw();
                    }
                }
            }
        }
        // A `LOADKBD` (via Ctrl+L or the cmline) swaps the keymap on the editor; mirror it
        // onto the capture resolver the GUI actually intercepts with.
        self.sync_capture_keymap();
        // `TITLEBAR on|off` toggles the OS decorations live; apply on change. Writer's View (`WV`)
        // additionally drops the title bar while it's on — so the effective state is titlebar AND not
        // Writer's View, and turning WV off restores the user's TITLEBAR preference.
        if let Some(window) = self.window.as_ref() {
            let want = self.app.show_titlebar() && !self.app.writer_view();
            if want != self.decorations {
                // Preserve the CLIENT (inner) size across the toggle. On Windows, `set_decorations`
                // keeps the OUTER size and lets the client absorb (off) or give up (on) the title-bar
                // height — so removing the title bar silently GREW the window by ~a title bar every
                // time it was toggled off, and that inflated size got saved by `WINGEO`, compounding
                // each launch. Re-asserting the pre-toggle inner size keeps the document area constant
                // (what the user actually sized) and stops the growth.
                let inner = window.inner_size();
                window.set_decorations(want);
                let _ = window.request_inner_size(inner);
                self.decorations = want;
            }
            // The title-bar drag strip was grabbed → start an OS-driven window move.
            if self.app.take_window_drag() {
                let _ = window.drag_window();
            }
        }
        // Open/close the TXT window to match the script's parked state (needs the event loop).
        self.sync_txt(event_loop);
        // Open/focus the DIR/W window to match a freshly-issued `DIR/W` (needs the event loop).
        self.sync_dir(event_loop);
        // Open/close the Find/Replace window to match whether the dialog is open (needs the event loop).
        self.sync_find(event_loop);
        // Open/close floating scratchpad windows to match `SCRATCH`/`SCRATCHCLOSE` (needs the event loop).
        self.sync_scratch(event_loop);
        // The earliest scheduled repaint across the main + TXT + DIR + FIND + scratchpad windows drives
        // the wait.
        let next = [
            self.repaint_at,
            self.txt.as_ref().and_then(|t| t.repaint_at),
            self.dir.as_ref().and_then(|d| d.repaint_at),
            self.find.as_ref().and_then(|f| f.repaint_at),
        ]
        .into_iter()
        .flatten()
        .chain(self.scratch.iter().filter_map(|s| s.repaint_at))
        .min();
        match next {
            Some(t) if Instant::now() >= t => {
                self.repaint_at = None;
                self.request_redraw();
                if let Some(tw) = self.txt.as_mut() {
                    tw.repaint_at = None;
                    tw.window.request_redraw();
                }
                if let Some(dw) = self.dir.as_mut() {
                    dw.repaint_at = None;
                    dw.window.request_redraw();
                }
                if let Some(fw) = self.find.as_mut() {
                    fw.repaint_at = None;
                    fw.window.request_redraw();
                }
                for sw in self.scratch.iter_mut() {
                    if sw.repaint_at.is_some_and(|t| Instant::now() >= t) {
                        sw.repaint_at = None;
                        sw.window.request_redraw();
                    }
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        // While a native file dialog is open on its own thread, keep waking (~30 ms) so we poll its
        // result channel promptly — otherwise `Wait` would sleep until an unrelated event.
        if self.dialog_job.is_some() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(30),
            ));
        }
    }
}

/// Canonical codes the egui layer ALWAYS keeps owning even when the keymap binds them, so their
/// special handling survives: Enter (28 — cmline submit / newline). Left/Right/Home/End are
/// deliberately NOT here (capturing them runs the bound motion directly via the correct canonical
/// code). Up/Down AND PageUp/PageDown are a SPECIAL case handled inline: egui owns them only when
/// UNMODIFIED (plain wrap-aware visual/page motion); ANY modifier (incl. Shift) reclaims them for
/// the keymap, so a binding like `Shift+PgUp = HI` fires.
const ALWAYS_EGUI: &[u8] = &[28];
/// Up / Down arrow + PageUp / PageDown canonical codes (the realigned XyWrite chart's gray cluster).
const ARROW_UP: u8 = 100;
const ARROW_DOWN: u8 = 102;
const ARROW_LEFT: u8 = 101;
const ARROW_RIGHT: u8 = 103;
const PAGE_UP: u8 = 93;
const PAGE_DOWN: u8 = 97;
/// The numpad nav cluster with NumLock OFF. The kbd identity-maps these non-extended set-1
/// scancodes, so they arrive as 72/80/73/81 — DISTINCT from the gray cluster above. We let them
/// reach egui on the same terms as the gray arrows so numpad nav scrolls pickers / moves the caret
/// identically (and, NumLock ON, the same physical keys still type digits via egui's Text events).
const NUMPAD_UP: u8 = 72;
const NUMPAD_DOWN: u8 = 80;
const NUMPAD_LEFT: u8 = 75;
const NUMPAD_RIGHT: u8 = 77;
const NUMPAD_PAGE_UP: u8 = 73;
const NUMPAD_PAGE_DOWN: u8 = 81;
/// Backspace scancode (delete-back, `BD`). Re-injected as egui Backspace for modal filter editing.
const BACKSPACE: u8 = 14;
/// The physical forward-Delete keys — all bind the delete-forward verb (`RC`): numpad Del (83), the
/// canonical Delete (90, per `key_to_code`), and the gray Delete (95). Re-injected as egui Delete so
/// Delete deletes a highlighted modal row regardless of which physical key. See `edit_code_egui_key`.
const DELETE_CODES: &[u8] = &[83, 90, 95];

/// True if `xpl` is a desugared self-insert typing cell — the D4 sugar turns a bare/quoted
/// printable cell into `insert "..."`. These reach OS Text/IME (so keyboard layout and IME
/// composition work) instead of being captured; anything else is a command the KBD owns. (To
/// self-insert a multi-char snippet on an unmodified key, use a text binder `@X`, which is a
/// command and so IS captured.)
fn is_typing_cell(xpl: &str) -> bool {
    xpl.trim_start().starts_with("insert \"")
}

/// The literal text a typing cell (`insert "…"`) inserts, with `\"` and `\\` unescaped. `None` if it
/// isn't a well-formed typing cell. Used by the scratchpad to type the keyboard file's layer into its
/// egui box instead of running the `insert` verb against the main document.
fn typing_cell_text(xpl: &str) -> Option<String> {
    let s = xpl.trim_start().strip_prefix("insert \"")?;
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c); // \" → ", \\ → \, best-effort for any other \x
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out); // closing quote
        } else {
            out.push(c);
        }
    }
    None // unterminated
}

/// The command text BEFORE a trailing `ENTER` verb in a resolved binding — so a chain like
/// `AR;ENTER` can run its prefix (`AR`) and then submit the command line. Returns `Some("")` for a
/// bare `ENTER`, `Some(prefix)` when the LAST `;`-separated token is `ENTER` (case-insensitive), or
/// `None` otherwise. Splitting on the last `;` suffices for the keyboard bindings this targets.
fn enter_submit_prefix(xpl: &str) -> Option<&str> {
    let t = xpl.trim();
    if t.eq_ignore_ascii_case("ENTER") {
        return Some("");
    }
    let idx = t.rfind(';')?;
    t[idx + 1..].trim().eq_ignore_ascii_case("ENTER").then(|| t[..idx].trim_end())
}

/// Whether `xpl`'s first word is a vertical/page motion verb the GUI handles VISUALLY (wrap-aware
/// line move for arrows via [`try_visual_vmotion`], page move for PageUp/Down in the egui input
/// path). An arrow/numpad-nav key bound to one of these is left to egui so it moves/scrolls
/// wrap-aware. A nav key bound to anything else is claimed by the keymap so the binding runs —
/// including the LOGICAL movers `UP`/`DOWN` (bind an arrow to them to get plain by-line motion),
/// which are deliberately NOT here so they stay logical wherever they're delivered.
fn is_gui_motion(xpl: &str) -> bool {
    matches!(
        xpl.split_whitespace().next().map(|w| w.to_ascii_uppercase()).as_deref(),
        Some("LU") | Some("LD") | Some("PU") | Some("PD")
    )
}

/// Codes `menu_key` acts on: the gray + numpad arrows, Enter (gray + numpad) and Esc. In the menu
/// key path, a key that maps to none of these is offered instead as an accelerator letter.
fn is_menu_nav_code(code: u8) -> bool {
    matches!(
        code,
        ARROW_UP | NUMPAD_UP | ARROW_DOWN | NUMPAD_DOWN
            | ARROW_LEFT | NUMPAD_LEFT | ARROW_RIGHT | NUMPAD_RIGHT
            | 28 | NUMPAD_ENTER | 1 // Enter · numpad Enter · Esc
    )
}

/// Map a resolved verb to the arrow SCANCODE that `menu_key` understands, so a KBD binding to a
/// navigation verb drives the keyboard menu exactly like the physical arrows do. `None` = not a
/// navigation verb. Covers the caret-motion verbs' left/right/up/down aliases plus the page verbs
/// (which `menu_key` currently swallows — harmless). See `nav_verb_egui_key` for the modal side.
fn nav_verb_code(xpl: &str) -> Option<u8> {
    match xpl.split_whitespace().next().map(|w| w.to_ascii_uppercase()).as_deref() {
        Some("UP") | Some("LU") => Some(ARROW_UP),
        Some("DOWN") | Some("LD") => Some(ARROW_DOWN),
        Some("LEFT") | Some("PC") | Some("CL") => Some(ARROW_LEFT),
        Some("RIGHT") | Some("NC") | Some("CR") => Some(ARROW_RIGHT),
        Some("PU") => Some(PAGE_UP),
        Some("PD") => Some(PAGE_DOWN),
        _ => None,
    }
}

/// Map a resolved verb to the egui `Key` a modal's handler understands, so a KBD binding to a
/// navigation verb drives a PICK/history list or an INPUT field like the physical arrows. `None` =
/// not a navigation verb (the bound command is then inert while the modal is up). Same verb set as
/// `nav_verb_code`, kept in lockstep with it.
fn nav_verb_egui_key(xpl: &str) -> Option<egui::Key> {
    match xpl.split_whitespace().next().map(|w| w.to_ascii_uppercase()).as_deref() {
        Some("UP") | Some("LU") => Some(egui::Key::ArrowUp),
        Some("DOWN") | Some("LD") => Some(egui::Key::ArrowDown),
        Some("LEFT") | Some("PC") | Some("CL") => Some(egui::Key::ArrowLeft),
        Some("RIGHT") | Some("NC") | Some("CR") => Some(egui::Key::ArrowRight),
        Some("PU") => Some(egui::Key::PageUp),
        Some("PD") => Some(egui::Key::PageDown),
        // Jump-to-end verbs move a PICK/history list to its top/bottom: top/bottom-of-file
        // (`TF`/`BF`, `TOP`/`BOTTOM`), screen top/bottom (`HM`/`BS`), and line home/end (`HOME`/`END`).
        Some("TF") | Some("TOP") | Some("HM") | Some("HOME") => Some(egui::Key::Home),
        Some("BF") | Some("BOTTOM") | Some("BS") | Some("END") => Some(egui::Key::End),
        _ => None,
    }
}

/// Map a physical EDIT scancode — Backspace / Delete — to the egui `Key` a modal's handler wants, so
/// those keys edit a type-to-find filter (Backspace) and delete a highlighted row (Delete) by their
/// physical identity while a modal is up, even though they're bound to editing verbs (`Delete`→`RC`,
/// etc.). Checked AHEAD of the nav mappings so a Backspace bound to a jump verb still edits the filter
/// instead of jumping the list. `None` = not an edit key.
fn edit_code_egui_key(code: u8) -> Option<egui::Key> {
    if code == BACKSPACE {
        Some(egui::Key::Backspace)
    } else if DELETE_CODES.contains(&code) {
        Some(egui::Key::Delete)
    } else {
        None
    }
}

/// Map a physical nav SCANCODE (gray or numpad arrows / page keys) to the egui `Key` a modal's
/// handler understands. Used as a FALLBACK so a nav key rebound to a command whose first word
/// isn't a nav verb — e.g. `80=IF NOT $TX;GT;ELSE;LD;EI` on Numpad-Down, where the buried `LD`
/// can't be spotted statically — still navigates a modal by the key's physical identity (its
/// document command stays inert behind the modal). This mirrors the menu path's `unwrap_or(code)`
/// fallback into `menu_key`. `None` = the physical key isn't a nav key.
fn nav_code_egui_key(code: u8) -> Option<egui::Key> {
    match code {
        ARROW_UP | NUMPAD_UP => Some(egui::Key::ArrowUp),
        ARROW_DOWN | NUMPAD_DOWN => Some(egui::Key::ArrowDown),
        ARROW_LEFT | NUMPAD_LEFT => Some(egui::Key::ArrowLeft),
        ARROW_RIGHT | NUMPAD_RIGHT => Some(egui::Key::ArrowRight),
        PAGE_UP | NUMPAD_PAGE_UP => Some(egui::Key::PageUp),
        PAGE_DOWN | NUMPAD_PAGE_DOWN => Some(egui::Key::PageDown),
        _ => None,
    }
}

/// Capture rule: a function key / Tab / Numpad-Enter (always — they carry no text), or any
/// key pressed while a command modifier (anything outside Shift/Caps) is held.
fn should_capture(code: u8, resolver: &Resolver, passthrough_mask: u32) -> bool {
    if FUNCTION_KEYS.contains(&code) || code == TAB || code == NUMPAD_ENTER {
        return true;
    }
    let mask = resolver.state.current_bitmask();
    (mask & !passthrough_mask) != 0
}

#[cfg(test)]
mod tests {
    use super::{
        describe_key, edit_code_egui_key, enter_submit_prefix, is_gui_motion, is_modifier_key,
        is_typing_cell, nav_verb_egui_key,
    };

    #[test]
    fn enter_submit_prefix_detects_a_trailing_enter() {
        // Bare ENTER → submit with no prefix.
        assert_eq!(enter_submit_prefix("ENTER"), Some(""));
        assert_eq!(enter_submit_prefix("  enter  "), Some(""));
        // A chain ending in ENTER → run the prefix, then submit.
        assert_eq!(enter_submit_prefix("AR;ENTER"), Some("AR"));
        assert_eq!(enter_submit_prefix("AR ; enter"), Some("AR"));
        assert_eq!(enter_submit_prefix("FOO;BAR;ENTER"), Some("FOO;BAR"));
        // No trailing ENTER → not a submit.
        assert_eq!(enter_submit_prefix("GT;TH"), None);
        assert_eq!(enter_submit_prefix("ENTER;AR"), None); // ENTER not last
        assert_eq!(enter_submit_prefix("insert \"x\""), None);
    }

    #[test]
    fn describe_key_uses_keymap_modifier_names_and_bits() {
        use winit::keyboard::{Key, NamedKey, SmolStr};
        use xyscript_kbd::{ModBehavior, ModifierDef};
        // Names AND bits come from the keymap's ModifierDefs — incl. a user-defined `HOTKEY`.
        let mk = |name: &str, bit: u32| ModifierDef {
            name: name.into(),
            bit,
            trigger_keys: vec![],
            behavior: ModBehavior::Hold,
            dual_role: false,
        };
        let mods = [mk("CTRL", 256), mk("ALT", 512), mk("SHIFT", 1024), mk("HOTKEY", 4096)];
        assert_eq!(describe_key(&Key::Character(SmolStr::new("y")), 256, &mods), "CTRL+Y");
        assert_eq!(describe_key(&Key::Named(NamedKey::F3), 1024, &mods), "SHIFT+F3");
        // Ordered by bit; a user-defined modifier is honoured by its name.
        assert_eq!(
            describe_key(&Key::Character(SmolStr::new("a")), 256 | 4096, &mods),
            "CTRL+HOTKEY+A"
        );
        // No modifiers → just the key label.
        assert_eq!(describe_key(&Key::Named(NamedKey::Enter), 0, &mods), "Enter");
    }

    #[test]
    fn readkey_ignores_bare_modifier_keys() {
        use winit::keyboard::{Key, NamedKey, SmolStr};
        // Modifiers (incl. the ones absent from MODIFIER_CODES: Super/AltGraph/locks) are skipped so a
        // bare press doesn't count as the keystroke.
        for m in [
            NamedKey::Shift, NamedKey::Control, NamedKey::Alt, NamedKey::AltGraph, NamedKey::Super,
            NamedKey::Meta, NamedKey::CapsLock, NamedKey::NumLock,
        ] {
            assert!(is_modifier_key(&Key::Named(m)), "{m:?} should be a modifier");
        }
        // A real key is NOT a modifier — READKEY resolves on it.
        assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier_key(&Key::Character(SmolStr::new("a"))));
    }

    #[test]
    fn physical_backspace_and_delete_reach_a_modal_by_identity() {
        use egui::Key;
        // A bound Backspace/Delete (they carry editing verbs) still edits the type-to-find filter /
        // deletes a highlighted row while a modal is up, by physical scancode — not swallowed.
        assert_eq!(edit_code_egui_key(14), Some(Key::Backspace)); // BD
        // All three forward-Delete keys (numpad / canonical / gray), each bound `RC`.
        for del in [83, 90, 95] {
            assert_eq!(edit_code_egui_key(del), Some(Key::Delete), "scancode {del}");
        }
        // Anything else isn't an edit key (falls through to the nav mappings / stays inert).
        assert_eq!(edit_code_egui_key(100), None); // an arrow
        assert_eq!(edit_code_egui_key(63), None); // F5
    }

    #[test]
    fn nav_verbs_drive_a_modal_list_including_page_and_jump() {
        use egui::Key;
        // Arrows + page keys move a PICK / HISTORY list; TF/BF (and their aliases) jump to the ends.
        assert_eq!(nav_verb_egui_key("UP"), Some(Key::ArrowUp));
        assert_eq!(nav_verb_egui_key("down sel"), Some(Key::ArrowDown));
        assert_eq!(nav_verb_egui_key("PU"), Some(Key::PageUp));
        assert_eq!(nav_verb_egui_key("PD"), Some(Key::PageDown));
        assert_eq!(nav_verb_egui_key("TF"), Some(Key::Home));
        assert_eq!(nav_verb_egui_key("BF"), Some(Key::End));
        assert_eq!(nav_verb_egui_key("TOP"), Some(Key::Home));
        assert_eq!(nav_verb_egui_key("BOTTOM"), Some(Key::End));
        // A non-navigation command doesn't drive the list (stays inert behind the modal).
        assert_eq!(nav_verb_egui_key("PR hi"), None);
        assert_eq!(nav_verb_egui_key("GT"), None);
    }

    #[test]
    fn only_gui_refined_motions_are_left_to_egui_on_nav_keys() {
        // The VISUAL movers + page motion fall through to egui's wrap-aware motion on an arrow/nav key.
        for m in ["LU", "LD", "PU", "PD", "lu sel", "LD SEL"] {
            assert!(is_gui_motion(m), "{m:?} should be a GUI-refined motion");
        }
        // Anything else bound to a nav key belongs to the keymap and must run — including the LOGICAL
        // movers UP/DOWN (bind an arrow to them for by-line motion; they must NOT be swallowed as
        // visual) and non-motion verbs (the numpad/arrow binding bug: `80=GT` was being swallowed).
        for c in ["UP", "DOWN", "UP SEL", "down sel", "GT", "TH", "GH", "PR hi", "SE /x/", "HM", "BS", "TOP"] {
            assert!(!is_gui_motion(c), "{c:?} must be claimed by the keymap, not egui");
        }
    }

    #[test]
    fn typing_cells_are_desugared_inserts_commands_are_not() {
        // The D4 desugarer (kbd_sugar) rewrites a literal-text cell to lowercase `insert "..."`;
        // those must route to OS Text/IME. These are its exact output forms for q / "hi" / space /
        // backslash / "?" — keep in sync if the desugarer's emitted shape ever changes.
        for xpl in ["insert \"q\"", "insert \"hi\"", "insert \" \"", "insert \"\\\\\"", "insert \"?\""] {
            assert!(is_typing_cell(xpl), "{xpl:?} should be a typing cell");
        }
        // Commands (incl. an explicit upper-case INSERT, binders) are captured, not typed.
        for cmd in ["XC", "TH", "BD", "LB", "call foo", "@A", "&B", "INSERT \"x\""] {
            assert!(!is_typing_cell(cmd), "{cmd:?} should be a command, not typing");
        }
    }
}
