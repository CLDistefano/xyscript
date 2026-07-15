//! Scrolling viewport math (`beyond-stage2-plan.md` Stage 3, step 6).
//!
//! Pure, headless-testable geometry: which window of a (possibly huge) document
//! is on screen, kept following the cursor, and where the cursor sits within
//! that window. The terminal plumbing (size queries, clears, cursor placement)
//! lives in `main.rs`; this module has no display dependency — only [`Buffer`].

use crate::highlight::{self, Token};
use crate::style::{Span, StyledLine};
use crate::text::Buffer;

/// The top-left of the visible window, in document coordinates (0-based line and
/// column). Persists across redraws so scrolling is stable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    pub top: usize,
    pub left: usize,
}

impl Viewport {
    pub fn new() -> Viewport {
        Viewport::default()
    }

    /// Scroll the minimum amount so the cursor at `(line, col)` is visible in a
    /// `height` × `width` window. Vertical and horizontal are independent.
    pub fn follow(&mut self, line: usize, col: usize, height: usize, width: usize) {
        if height > 0 {
            if line < self.top {
                self.top = line;
            } else if line >= self.top + height {
                self.top = line - height + 1;
            }
        }
        if width > 0 {
            if col < self.left {
                self.left = col;
            } else if col >= self.left + width {
                self.left = col - width + 1;
            }
        }
    }

    /// The `height` visible rows, each horizontally clipped to `width` columns.
    /// Rows past the end of the document are empty strings.
    pub fn rows(&self, buffer: &Buffer, height: usize, width: usize) -> Vec<String> {
        let total = buffer.line_count();
        (0..height)
            .map(|i| {
                let line = self.top + i;
                if line >= total {
                    String::new()
                } else {
                    buffer
                        .line_text(line)
                        .chars()
                        .skip(self.left)
                        .take(width)
                        .collect()
                }
            })
            .collect()
    }

    /// The visible rows as **styled spans** — the render path the live display
    /// uses. With empty `tokens` (the default — highlighting off), each non-empty
    /// line is one plain span (a blank line is an empty vec). With `tokens`
    /// (highlighting on, byte offsets into the document's UTF-8 source), each line
    /// is split into styled spans at token boundaries. Decode-aware: text comes
    /// from [`Buffer::line_text`].
    pub fn styled_rows(
        &self,
        buffer: &Buffer,
        tokens: &[Token],
        height: usize,
        width: usize,
    ) -> Vec<StyledLine> {
        let total = buffer.line_count();
        (0..height)
            .map(|i| {
                let line = self.top + i;
                if line >= total {
                    return Vec::new();
                }
                if tokens.is_empty() {
                    // Plain fast path — identical to the pre-highlighting behavior.
                    let s: String =
                        buffer.line_text(line).chars().skip(self.left).take(width).collect();
                    return if s.is_empty() { Vec::new() } else { vec![Span::plain(s)] };
                }
                let (start, _end) = buffer.line_byte_range(line);
                let full = highlight::styled_line(&buffer.line_text(line), start, tokens);
                highlight::clip_styled(&full, self.left, width)
            })
            .collect()
    }

    /// The cursor's on-screen `(row, col)` relative to the window, given its
    /// document `(line, col)`. Call [`Viewport::follow`] first so it's in range.
    pub fn cursor_screen(&self, line: usize, col: usize) -> (usize, usize) {
        (line.saturating_sub(self.top), col.saturating_sub(self.left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(lines: usize) -> Buffer {
        let body: String = (0..lines).map(|i| format!("line{i}\n")).collect();
        Buffer::from_str(&body)
    }

    #[test]
    fn follow_scrolls_down_to_keep_cursor_visible() {
        let mut vp = Viewport::new();
        vp.follow(0, 0, 10, 80);
        assert_eq!(vp.top, 0);
        // Cursor on line 25 with a 10-row window -> top becomes 16 (lines 16..25).
        vp.follow(25, 0, 10, 80);
        assert_eq!(vp.top, 16);
        assert_eq!(vp.cursor_screen(25, 0), (9, 0)); // last visible row
    }

    #[test]
    fn follow_scrolls_up() {
        let mut vp = Viewport { top: 20, left: 0 };
        vp.follow(5, 0, 10, 80);
        assert_eq!(vp.top, 5);
    }

    #[test]
    fn follow_scrolls_horizontally() {
        let mut vp = Viewport::new();
        vp.follow(0, 100, 10, 80); // col 100 in an 80-wide window
        assert_eq!(vp.left, 21); // shows cols 21..100
        assert_eq!(vp.cursor_screen(0, 100), (0, 79));
        vp.follow(0, 3, 10, 80); // jump left
        assert_eq!(vp.left, 3);
    }

    #[test]
    fn rows_render_the_visible_window_clipped() {
        let b = doc(50);
        let vp = Viewport { top: 10, left: 0 };
        let rows = vp.rows(&b, 3, 80);
        assert_eq!(rows, vec!["line10", "line11", "line12"]);
    }

    #[test]
    fn rows_past_end_are_blank() {
        let b = Buffer::from_str("only\n"); // lines: "only", "" -> 2 lines
        let vp = Viewport::new();
        let rows = vp.rows(&b, 4, 80);
        assert_eq!(rows, vec!["only", "", "", ""]);
    }

    #[test]
    fn rows_clip_horizontally() {
        let b = Buffer::from_str("abcdefghij");
        let vp = Viewport { top: 0, left: 2 };
        let rows = vp.rows(&b, 1, 4);
        assert_eq!(rows, vec!["cdef"]); // cols 2..6
    }

    #[test]
    fn rows_reflect_the_current_encoding_view() {
        use crate::encoding::{cp437, latin1};
        // Same bytes, two views: the viewport renders whatever the live encoding
        // decodes — so a `set_encoding` switch is visible on the next redraw.
        let mut b = Buffer::from_bytes(vec![0xAE, 0xAF], cp437());
        let vp = Viewport::new();
        assert_eq!(vp.rows(&b, 1, 80), vec!["«»"]); // CP437
        b.set_encoding(latin1());
        assert_eq!(vp.rows(&b, 1, 80), vec!["®¯"]); // Latin-1, same bytes
    }

    #[test]
    fn styled_rows_apply_highlight_tokens() {
        use crate::highlight::Highlighter;
        let src = "# Hi\nplain\n";
        let b = Buffer::from_str(src);
        let toks = Highlighter::Markdown.highlight(src);
        let vp = Viewport::new();
        let rows = vp.styled_rows(&b, &toks, 2, 80);
        assert!(rows[0].iter().any(|s| s.style.bold), "heading line should be bold");
        assert!(rows[1].iter().all(|s| s.style.is_plain()), "plain line stays plain");
    }

    #[test]
    fn styled_rows_wrap_each_line_in_one_plain_span() {
        use crate::style::{Span, Style};
        let b = doc(3);
        let vp = Viewport::new();
        let rows = vp.styled_rows(&b, &[], 3, 80);
        assert_eq!(rows[0], vec![Span::plain("line0")]);
        assert!(rows[0][0].style.is_plain());
        assert_eq!(rows[0][0].style, Style::default());
        // A row past the end is a blank line (no spans).
        let past = vp.styled_rows(&b, &[], 5, 80);
        assert_eq!(past[4], Vec::<Span>::new());
    }
}
