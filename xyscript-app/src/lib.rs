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

pub mod ansi;
pub mod boot;
pub mod bundle;
pub mod byte_rope;
pub mod clock;
pub mod compile;
pub mod counter;
pub mod diff;
pub mod dir;
pub mod document;
pub mod editor;
pub mod encoding;
pub mod gfm;
pub mod grep;
pub mod help;
pub mod hex;
pub mod manual;
pub mod highlight;
pub mod html;
pub mod pdf;
pub mod host;
pub mod kbd_dump;
pub mod kbd_sugar;
pub mod lint;
pub mod os_message;
pub mod outline;
pub mod panes;
pub mod sort;
pub mod style;
pub mod text;
pub mod update;
mod typst_esc;
pub mod viewport;

pub use byte_rope::ByteRope;
pub use document::Document;
pub use document::{Bookmark, MarkNav};
pub use panes::{Divider, DocId, PaneId, PaneRect, PaneTree, Rect as PaneRectF, Side, Split};
pub use editor::Editor;
pub use encoding::{cp437, latin1, utf8, Charset, Encoding};
pub use gfm::to_typst;
pub use highlight::{Highlighter, Token, TokenKind};
pub use pdf::{
    gfm_to_image, gfm_to_pdf, gfm_to_svg, html_to_image, html_to_pdf, html_to_svg, text_to_image,
    text_to_pdf, text_to_svg, typst_to_image, typst_to_pdf, typst_to_svg, PreviewFonts, PreviewTint,
};
pub use style::{Color, Span, Style, StyledLine};
pub use host::AppHost;
pub use kbd_sugar::desugar_keymap;
pub use text::{Buffer, BalMatch};
pub use xyscript_xpl::{
    describe_xlink_payload, BUILTIN_VERBS, CONTROL_KEYWORDS, FontMode, UpdateAction, UpdateMode,
    XlinkConfirmChoice,
};
pub use viewport::Viewport;
