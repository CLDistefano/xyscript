//! XyScript app — the Stage 2 integration layer (`development-plan.md` Stage 2).
//!
//! Wires the three foundations into an editor:
//! - the **keyboard layer** (`xyscript-kbd`) resolves physical keys to XPL,
//! - the **interpreter** (`xyscript-xpl`) executes that XPL,
//! - a minimal **text engine** ([`text`]) is what the interpreter's editor verbs
//!   mutate, via the [`Host`](xyscript_xpl::Host) implementation in [`host`].
//!
//! [`editor::Editor`] owns all three and runs the closed loop: a key event →
//! resolve → dispatch XPL → mutate the buffer → render. Everything here is
//! headless-testable; the live OS-hook + display is the thin shell in `main.rs`.

pub mod boot;
pub mod byte_rope;
pub mod clock;
pub mod counter;
pub mod dir;
pub mod document;
pub mod editor;
pub mod encoding;
pub mod gfm;
pub mod help;
pub mod highlight;
pub mod html;
pub mod pdf;
pub mod host;
pub mod kbd_dump;
pub mod kbd_sugar;
pub mod lint;
pub mod os_message;
pub mod panes;
pub mod sort;
pub mod style;
pub mod text;
mod typst_esc;
pub mod viewport;

pub use byte_rope::ByteRope;
pub use document::Document;
pub use panes::{Divider, DocId, PaneId, PaneRect, PaneTree, Rect as PaneRectF, Side, Split};
pub use editor::Editor;
pub use encoding::{cp437, latin1, utf8, Charset, Encoding};
pub use gfm::to_typst;
pub use highlight::{Highlighter, Token, TokenKind};
pub use pdf::{
    gfm_to_image, gfm_to_pdf, html_to_image, html_to_pdf, typst_to_image, typst_to_pdf, PreviewFonts,
};
pub use style::{Color, Span, Style, StyledLine};
pub use host::AppHost;
pub use kbd_sugar::desugar_keymap;
pub use text::{Buffer, BalMatch};
pub use xyscript_xpl::{BUILTIN_VERBS, CONTROL_KEYWORDS};
pub use viewport::Viewport;
