//! Display styling for rendered lines (`byte-text-engine-plan.md` step 5).
//!
//! The render pipeline carries text as **styled spans**, not bare strings, so the
//! features on the horizon — syntax highlighting, Xy-MoDe colors (hex RGB),
//! GitHub-Flavored-Markdown rendering — can attach styles later without reshaping
//! the viewport→display path (the guidance recorded in the feature notes). For
//! now the viewport emits a single plain span per line; nothing computes styles
//! yet, but the *types* leave room for it.

/// A display color. Minimal now (the 8 basic terminal colors + "default"); a
/// `Rgb(u8, u8, u8)` arm can be added for Xy-MoDe hex colors without disturbing
/// callers that only use the named ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The terminal's default foreground/background.
    #[default]
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

/// How a [`Span`] is drawn: colors plus emphasis flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    /// The unstyled default (terminal colors, no emphasis).
    pub fn plain() -> Style {
        Style::default()
    }

    /// Whether this is the plain default — lets the renderer skip color codes.
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A run of text sharing one [`Style`] — the unit a highlighter emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    /// A span with the default (plain) style.
    pub fn plain(text: impl Into<String>) -> Span {
        Span { text: text.into(), style: Style::default() }
    }

    /// A span with an explicit style.
    pub fn styled(text: impl Into<String>, style: Style) -> Span {
        Span { text: text.into(), style }
    }
}

/// One on-screen line as a sequence of styled spans. An empty vec is a blank
/// line. (A future highlighter splits a line into several spans; today it's one.)
pub type StyledLine = Vec<Span>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_style_is_default_and_detected() {
        assert!(Style::plain().is_plain());
        let s = Style { bold: true, ..Style::default() };
        assert!(!s.is_plain());
    }

    #[test]
    fn span_constructors() {
        assert_eq!(Span::plain("hi"), Span { text: "hi".into(), style: Style::default() });
        let s = Style { fg: Color::Red, ..Style::default() };
        assert_eq!(Span::styled("x", s).style.fg, Color::Red);
    }
}
