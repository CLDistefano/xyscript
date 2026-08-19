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

/// Text-like extensions the Open dialog shows by default. Broad on purpose — XyScript is a UTF-8
/// text editor, not tied to one format — and an "All files" entry is added alongside, so anything
/// else stays reachable. (rfd renders each as `*.<ext>`; a filter's first ext is the Save default,
/// which doesn't matter for Open.)
const TEXT_EXTENSIONS: &[&str] = &[
    // Documents / markup
    "txt", "text", "md", "markdown", "mdown", "mkd", "rst", "org", "adoc", "asciidoc", "tex", "typ",
    "log", "nfo", "me", "readme",
    // XyScript's own files
    "xpl", "kbd", "lib", "cfg",
    // Data / config
    "json", "jsonc", "csv", "tsv", "xml", "yaml", "yml", "toml", "ini", "conf", "config",
    "properties", "env", "editorconfig", "diff", "patch",
    // Web / style
    "html", "htm", "xhtml", "css", "scss", "sass", "less", "svg",
    // Code / scripts
    "js", "mjs", "cjs", "ts", "tsx", "jsx", "rs", "py", "rb", "pl", "php", "lua", "sh", "bash",
    "zsh", "ps1", "bat", "cmd", "c", "h", "cpp", "hpp", "cc", "cs", "java", "kt", "go", "swift",
    "sql", "r", "jl", "vim", "awk", "make", "mk", "cmake",
];

/// Run a native file dialog for `req`, parented to `window` so the OS makes it *owner-modal*. Blocks
/// until the user chooses or cancels; returns the picked path (or `None`). Called from
/// `about_to_wait` — never from inside the egui frame — so the modal loop can't re-enter `run_ui`
/// and deadlock egui's Context.
fn run_file_dialog(
    req: &crate::PendingDialog,
    window: &winit::window::Window,
    default_typst: bool,
) -> Option<std::path::PathBuf> {
    use crate::PendingDialog;
    let base = rfd::FileDialog::new().set_parent(window);
    match req {
        PendingDialog::Open { hex } => {
            // A `/HEX` open is usually a binary, so lead with the All-files filter there.
            if *hex {
                base.add_filter("All files", &["*"])
                    .add_filter("Text files", TEXT_EXTENSIONS)
                    .pick_file()
            } else {
                base.add_filter("Text files", TEXT_EXTENSIONS)
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
        // Keep the surface sized to the window BEFORE painting — a mixed-DPI monitor change updates
        // egui's ppp (read fresh below) but can leave the surface stale, which renders the whole
        // window tiny until restart.
        let phys = window.inner_size();
        self.ensure_surface(phys.width, phys.height);
        let raw_input = match self.egui_state.as_mut() {
            Some(s) => s.take_egui_input(&window),
            None => return,
        };
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
        if let Some((lw, lh, x, y)) = restored {
            let (lw, lh, x, y) = clamp_geometry(event_loop, lw, lh, x, y);
            // Size is LOGICAL: winit converts it with the target monitor's scale factor, so the
            // window keeps its apparent size across different-DPI displays. Position stays physical.
            attrs = attrs
                .with_inner_size(LogicalSize::new(lw, lh))
                .with_position(PhysicalPosition::new(x, y));
        } else {
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
        }
        // Honour a `TITLEBAR off` restored from config at startup (avoids a decorated→borderless flash).
        self.decorations = self.app.show_titlebar();
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
                // fall through so egui updates its screen rect too
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
                // F3 / Shift+F3 = Find Next / Find Prev (universal). Handled here — ahead of the
                // egui-focus check below — so it works even while the Find dialog's field (or the
                // command bar) has keyboard focus, where the keymap path is skipped. Not in a dir
                // listing (F3 there is express-find, handled above / by the listing).
                if key_event.state == ElementState::Pressed
                    && key_event.physical_key == PhysicalKey::Code(KeyCode::F3)
                    && !self.app.active_is_dir_listing()
                {
                    self.app.find_again(self.mods.shift_key());
                    self.request_redraw();
                    return; // consumed
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
        // A menu / key action requested a native file dialog. Run it HERE — outside the egui frame
        // and outside any window-event dispatch — parented to the main window so the OS makes it
        // owner-modal: while it is open, clicks on the main window are refused (it stays put) and
        // clicking another application still switches to it. Running it inline mid-frame re-entered
        // `run_ui` and deadlocked egui's Context (the freeze).
        if let Some(window) = self.window.clone() {
            if let Some(req) = self.app.take_pending_dialog() {
                let default_typst = self.app.active_is_typst();
                let picked = run_file_dialog(&req, window.as_ref(), default_typst);
                self.app.apply_dialog_result(req, picked);
                window.request_redraw();
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
                window.set_decorations(want);
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
        // The earliest scheduled repaint across the main + TXT + DIR + FIND windows drives the wait.
        let next = [
            self.repaint_at,
            self.txt.as_ref().and_then(|t| t.repaint_at),
            self.dir.as_ref().and_then(|d| d.repaint_at),
            self.find.as_ref().and_then(|f| f.repaint_at),
        ]
        .into_iter()
        .flatten()
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
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
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
/// path). An arrow/numpad-nav key bound to one of these is left to egui so it scrolls wrap-aware;
/// a nav key bound to anything else is claimed by the keymap so the binding actually runs.
fn is_gui_motion(xpl: &str) -> bool {
    matches!(
        xpl.split_whitespace().next().map(|w| w.to_ascii_uppercase()).as_deref(),
        Some("LU") | Some("LD") | Some("UP") | Some("DOWN") | Some("PU") | Some("PD")
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
        // These fall through to egui's wrap-aware motion when bound to an arrow/nav key.
        for m in ["UP", "DOWN", "LU", "LD", "PU", "PD", "UP SEL", "down sel"] {
            assert!(is_gui_motion(m), "{m:?} should be a GUI-refined motion");
        }
        // Anything else bound to a nav key belongs to the keymap and must run (the numpad/arrow
        // binding bug: `80=GT` was being swallowed by the visual-motion interception).
        for c in ["GT", "TH", "GH", "PR hi", "SE /x/", "HM", "BS", "TOP"] {
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
