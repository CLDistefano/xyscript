//! XyScript binary — the thin platform shell (Stage 2/3).
//!
//! The editing logic lives in the library ([`xyscript_app::Editor`]) and is
//! fully headless-tested. This binary is the minimal, platform-specific glue: it
//! installs the keyboard layer's global non-exclusive hook, normalizes each
//! native event to a canonical [`KeyEvent`], feeds it to the editor, and renders
//! a **scrolling viewport** of the active document to the terminal via
//! `crossterm` (which handles size, cursor placement, and conhost/VT
//! compatibility). `KF` keys are released to the OS; **Ctrl+Q quits**.
//!
//! Provisional by design — `crossterm` is a generic terminal backend, not
//! XyScript's identity; the display can be swapped for a real GUI later without
//! touching the editor core or the (pure, tested) viewport math.
//!
//! Usage: `cargo run -p xyscript-app [-- <startup.xpl>]`.

use std::io::Write;

use crossterm::cursor::{MoveTo, Show};
use crossterm::style::{
    Attribute, Color as CtColor, Print, ResetColor, SetAttribute, SetBackgroundColor,
    SetForegroundColor,
};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use xyscript_app::{Color, Editor, Span, Viewport};
use xyscript_kbd::event::{KeyEvent, KeyPhase};
use xyscript_kbd::normalize::{host_table, ScancodeMap};
use xyscript_kbd::pipeline::Capture;
use xyscript_kbd::platform::{run_capture, RawEvent};
use xyscript_kbd::{load, Disposition, LoadOutcome};

/// The demo typing layout, compiled in so it works regardless of directory.
const INTERNAL_KBD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../xyscript-kbd/kbd/xyscript-internal.kbd"
));

use xyscript_app::boot::resolve_startup_script;

const VIRTUAL_BASE: Option<u8> = Some(120);

fn main() {
    // Headless batch mode (`--run <script>`): run the script with no UI and exit. This is what a
    // `RUN/BG`-spawned child invokes (`$ED --run <path>`). Checked before any terminal setup.
    let cli_args: Vec<String> = std::env::args().skip(1).collect();
    // `--message <text>`: show a native OS box and exit (what `MESSAGE` re-execs on Windows).
    if let Some(code) = xyscript_app::os_message::maybe_render(&cli_args) {
        std::process::exit(code);
    }
    if let Some(code) = xyscript_app::boot::run_batch(&cli_args) {
        std::process::exit(code);
    }

    let mut editor = match load(INTERNAL_KBD, VIRTUAL_BASE) {
        LoadOutcome::Installed(km) => Editor::new(km),
        LoadOutcome::Rejected(e) => {
            eprintln!("xyscript: bundled layout failed to parse ({e:?}); using default.");
            Editor::with_default_keymap()
        }
    };

    // Personal dictionary (`LEARN`): load `xyscript-dict.txt` beside the executable, if present.
    editor.load_learned_words();

    // Startup script: `-s <path>` if given, else `startup.xpl` beside the executable.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match resolve_startup_script(&args) {
        Ok(Some((label, src))) => editor.run_startup(&src, &label),
        Ok(None) => {}
        Err(e) => eprintln!("xyscript: {e}"),
    }

    // Enter the alternate screen so the shell is restored on quit.
    let _ = execute!(std::io::stdout(), EnterAlternateScreen);

    let mut vp = Viewport::new();
    redraw(&mut editor, &mut vp);

    let table = host_table(VIRTUAL_BASE);

    let mut handler = move |raw: RawEvent| -> Capture {
        let Some(code) = table.to_canonical(raw.native) else {
            return Capture::PassThrough; // unclaimed key -> the OS keeps it
        };
        let ev = match raw.phase {
            KeyPhase::Down => KeyEvent::down(code),
            KeyPhase::Up => KeyEvent::up(code),
            KeyPhase::Inject => KeyEvent::inject(code, 0),
        };
        let disposition = editor.feed_key(ev);
        // The TUI has no tab/prompt model, so any QUIT (verify or not) just exits.
        if editor.take_quit().is_some() {
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, Show);
            std::process::exit(0);
        }
        editor.poll_config_track(); // AUTOCFG: mirror settings if a command changed them
        redraw(&mut editor, &mut vp);
        match disposition {
            Disposition::Flip => Capture::PassThrough, // KF -> OS
            _ => Capture::Consume,
        }
    };

    if let Err(e) = run_capture(&mut handler) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, Show);
        eprintln!("xyscript: keyboard capture unavailable: {e}");
        eprintln!("(the editor core still works headlessly; see the library tests)");
    }
}

/// Redraw a scrolling viewport of the active document, with a status line (or the
/// command line) on the bottom row, and the real terminal cursor placed in it.
fn redraw(editor: &mut Editor, vp: &mut Viewport) {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let width = cols.max(1) as usize;
    let height = rows.max(1) as usize;
    let text_height = height.saturating_sub(1).max(1); // bottom row = status/cmline
    let status_row = (height - 1) as u16;

    let cur_line = editor.document().line().saturating_sub(1); // 0-based
    let cur_col = editor.document().column();
    vp.follow(cur_line, cur_col, text_height, width);
    // Feed the viewport hint so the viewport motions (PD/PU/HM/BS) know the page
    // size and screen edges in the terminal too.
    editor.set_viewport(vp.top, text_height);
    let doc = editor.document();
    let lines = vp.styled_rows(doc, editor.highlights(), text_height, width);

    let mut out = std::io::stdout();
    let _ = queue!(out, Clear(ClearType::All));
    for (i, spans) in lines.iter().enumerate() {
        if !spans.is_empty() {
            let _ = queue!(out, MoveTo(0, i as u16));
            for span in spans {
                queue_span(&mut out, span);
            }
        }
    }

    if editor.cmline_open() {
        let cl = editor.command_line();
        let prompt = format!(":{}", cl.text());
        let _ = queue!(out, MoveTo(0, status_row), Print(prompt));
        let _ = queue!(out, MoveTo((1 + cl.cursor()) as u16, status_row), Show);
    } else {
        let _ = queue!(out, MoveTo(0, status_row), Print(status_line(editor, width)));
        let (sr, sc) = vp.cursor_screen(cur_line, cur_col);
        let _ = queue!(out, MoveTo(sc as u16, sr as u16), Show);
    }
    let _ = out.flush();
}

/// Queue one styled span. A plain span prints verbatim; a styled one sets the
/// colors/attributes, prints, then resets — so the styled-span pipeline is fully
/// wired even though nothing emits non-plain spans yet (step 5 forward-compat).
fn queue_span(out: &mut impl Write, span: &Span) {
    if span.style.is_plain() {
        let _ = queue!(out, Print(&span.text));
        return;
    }
    let st = &span.style;
    let _ = queue!(out, SetForegroundColor(to_ct(st.fg)), SetBackgroundColor(to_ct(st.bg)));
    if st.bold {
        let _ = queue!(out, SetAttribute(Attribute::Bold));
    }
    if st.italic {
        let _ = queue!(out, SetAttribute(Attribute::Italic));
    }
    if st.underline {
        let _ = queue!(out, SetAttribute(Attribute::Underlined));
    }
    let _ = queue!(out, Print(&span.text), SetAttribute(Attribute::Reset), ResetColor);
}

/// Map a XyScript display [`Color`] to a crossterm color.
fn to_ct(c: Color) -> CtColor {
    match c {
        Color::Default => CtColor::Reset,
        Color::Black => CtColor::Black,
        Color::Red => CtColor::Red,
        Color::Green => CtColor::Green,
        Color::Yellow => CtColor::Yellow,
        Color::Blue => CtColor::Blue,
        Color::Magenta => CtColor::Magenta,
        Color::Cyan => CtColor::Cyan,
        Color::White => CtColor::White,
    }
}

/// Build the bottom status line, clipped to `width`.
fn status_line(editor: &Editor, width: usize) -> String {
    let doc = editor.document();
    let name = editor.active_file_name().unwrap_or_else(|| "(untitled)".to_string());
    let modified = if editor.modified() { "[+]" } else { "" };
    let hl = editor.highlighter_name().map(|n| format!("  hl:{n}")).unwrap_or_default();
    let info = format!(
        "{name}{modified}  {}:{}  {}{hl}  buf {}/{}  [Ctrl+Q quit]",
        doc.line(),
        doc.column(),
        doc.encoding_name(),
        editor.active_index() + 1,
        editor.buffer_count(),
    );
    let msg = if let Some(e) = editor.last_error() {
        format!("  --  err: {e}") // fatal errors always show (QUIET only hides non-fatal/info)
    } else if let Some(s) = editor.command_feedback() {
        format!("  --  {s}") // PR always; op-statuses/non-fatal errors honor QUIET
    } else {
        String::new()
    };
    let line = format!("{info}{msg}");
    line.chars().take(width).collect()
}
