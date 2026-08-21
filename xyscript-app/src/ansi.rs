//! A minimal ANSI SGR (Select Graphic Rendition) parser for Console output.
//!
//! `AnsiState::feed` takes a chunk of a command's output and returns it split into runs of
//! `(text, Style)` with the escape sequences STRIPPED — the caller inserts each run's text and, for a
//! non-default style, records the matching MDA colour/attribute span over its range. Only SGR (`ESC[…m`)
//! is honoured; other CSI sequences (cursor motion, screen erase) are stripped and ignored — this is a
//! command console, not a terminal emulator. State persists across `feed` calls (a colour run, or even a
//! single escape, can straddle a chunk boundary), so an incomplete trailing escape is buffered.

use xyscript_xpl::{ATTR_BOLD, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE};

/// The active graphic rendition: foreground / background colour (`None` = the theme default) and the
/// attribute bitset ([`ATTR_BOLD`] etc.). The MDA layers this maps onto are fg / bg / attrs.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub attrs: u8,
}

impl Style {
    pub fn is_default(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && self.attrs == 0
    }
}

/// Streaming SGR state: the current style plus any partial escape carried over from the last chunk.
#[derive(Default)]
pub struct AnsiState {
    cur: Style,
    pending: String,
}

impl AnsiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset to the default style and drop any partial escape — call between commands so one command's
    /// leftover colour never bleeds into the next command's output (or our own `(exit N)` line).
    pub fn reset(&mut self) {
        self.cur = Style::default();
        self.pending.clear();
    }

    /// Parse `chunk`, returning `(text, style)` runs with escapes removed. Escapes are ASCII, so a
    /// byte scan is safe for UTF-8 text (a multibyte char's bytes are never escape bytes); each run's
    /// bytes are decoded lossily at the boundary.
    pub fn feed(&mut self, chunk: &str) -> Vec<(String, Style)> {
        let mut input = std::mem::take(&mut self.pending);
        input.push_str(chunk);
        let b = input.as_bytes();
        let n = b.len();
        let mut runs: Vec<(Vec<u8>, Style)> = Vec::new();
        let mut text: Vec<u8> = Vec::new();
        let mut run_style = self.cur;
        let mut i = 0;
        while i < n {
            if b[i] == 0x1b {
                // Need `ESC [ … final`. If the sequence is cut off at the chunk end, buffer it whole.
                if i + 1 >= n {
                    self.pending = String::from_utf8_lossy(&b[i..]).into_owned();
                    break;
                }
                if b[i + 1] == b'[' {
                    // CSI: parameter/intermediate bytes (0x20–0x3f) up to a final byte (0x40–0x7e).
                    let mut j = i + 2;
                    while j < n && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    if j >= n {
                        self.pending = String::from_utf8_lossy(&b[i..]).into_owned();
                        break;
                    }
                    if b[j] == b'm' {
                        // SGR — a style change: close the current run, then apply the parameters.
                        if !text.is_empty() {
                            runs.push((std::mem::take(&mut text), run_style));
                        }
                        apply_sgr(&mut self.cur, &b[i + 2..j]);
                        run_style = self.cur;
                    }
                    // Any other final byte (cursor/erase/etc.) → strip and ignore.
                    i = j + 1;
                    continue;
                }
                // A lone ESC or ESC-not-`[` (rare) → strip the ESC and keep scanning.
                i += 1;
                continue;
            }
            text.push(b[i]);
            i += 1;
        }
        if !text.is_empty() {
            runs.push((text, run_style));
        }
        runs.into_iter()
            .map(|(t, s)| (String::from_utf8_lossy(&t).into_owned(), s))
            .collect()
    }
}

/// Apply one SGR sequence's `;`-separated numeric parameters to `style`.
fn apply_sgr(style: &mut Style, params: &[u8]) {
    // An empty parameter list (`ESC[m`) means reset, same as `0`.
    let text = std::str::from_utf8(params).unwrap_or("");
    let codes: Vec<i32> = if text.is_empty() {
        vec![0]
    } else {
        text.split(';').map(|p| p.parse::<i32>().unwrap_or(0)).collect()
    };
    let mut it = codes.into_iter().peekable();
    while let Some(c) = it.next() {
        match c {
            0 => *style = Style::default(),
            1 => style.attrs |= ATTR_BOLD,
            3 => style.attrs |= ATTR_ITALIC,
            4 => style.attrs |= ATTR_UNDERLINE,
            7 => style.attrs |= ATTR_REVERSE,
            22 => style.attrs &= !ATTR_BOLD,
            23 => style.attrs &= !ATTR_ITALIC,
            24 => style.attrs &= !ATTR_UNDERLINE,
            27 => style.attrs &= !ATTR_REVERSE,
            30..=37 => style.fg = Some(BASIC[(c - 30) as usize]),
            90..=97 => style.fg = Some(BASIC[(c - 90 + 8) as usize]),
            39 => style.fg = None,
            40..=47 => style.bg = Some(BASIC[(c - 40) as usize]),
            100..=107 => style.bg = Some(BASIC[(c - 100 + 8) as usize]),
            49 => style.bg = None,
            38 => style.fg = take_extended(&mut it),
            48 => style.bg = take_extended(&mut it),
            _ => {} // unsupported (e.g. blink) — ignored
        }
    }
}

/// Consume the tail of a `38`/`48` extended-colour code: `5;<n>` (256-colour) or `2;<r>;<g>;<b>`.
fn take_extended(it: &mut std::iter::Peekable<std::vec::IntoIter<i32>>) -> Option<(u8, u8, u8)> {
    match it.next() {
        Some(5) => it.next().map(|n| xterm256(n.clamp(0, 255) as u8)),
        Some(2) => {
            let r = it.next().unwrap_or(0).clamp(0, 255) as u8;
            let g = it.next().unwrap_or(0).clamp(0, 255) as u8;
            let bl = it.next().unwrap_or(0).clamp(0, 255) as u8;
            Some((r, g, bl))
        }
        _ => None,
    }
}

/// The 16 basic ANSI colours (0–7 normal, 8–15 bright), standard xterm RGB.
const BASIC: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (170, 0, 0),
    (0, 170, 0),
    (170, 85, 0),
    (0, 0, 170),
    (170, 0, 170),
    (0, 170, 170),
    (170, 170, 170),
    (85, 85, 85),
    (255, 85, 85),
    (85, 255, 85),
    (255, 255, 85),
    (85, 85, 255),
    (255, 85, 255),
    (85, 255, 255),
    (255, 255, 255),
];

/// Map an xterm 256-colour index to RGB: 0–15 basic, 16–231 the 6×6×6 cube, 232–255 grayscale.
fn xterm256(n: u8) -> (u8, u8, u8) {
    match n {
        0..=15 => BASIC[n as usize],
        16..=231 => {
            let n = n - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(n / 36 % 6), step(n / 6 % 6), step(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_fg_run_then_reset() {
        let mut a = AnsiState::new();
        let runs = a.feed("\x1b[31mred\x1b[0m plain");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, "red");
        assert_eq!(runs[0].1.fg, Some((170, 0, 0)));
        assert_eq!(runs[1].0, " plain");
        assert!(runs[1].1.is_default());
    }

    #[test]
    fn escape_split_across_chunks_is_buffered() {
        let mut a = AnsiState::new();
        let r1 = a.feed("pre\x1b[3"); // the escape is cut off → buffered, not shown
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].0, "pre");
        let r2 = a.feed("1mred"); // completes to `ESC[31m` → red
        assert_eq!(r2[0].0, "red");
        assert_eq!(r2[0].1.fg, Some((170, 0, 0)));
    }

    #[test]
    fn attrs_truecolor_and_256() {
        let mut a = AnsiState::new();
        let runs = a.feed("\x1b[1;4mB\x1b[0m\x1b[38;2;10;20;30mT\x1b[0m\x1b[38;5;196mX");
        assert_eq!(runs[0].0, "B");
        assert_eq!(runs[0].1.attrs, ATTR_BOLD | ATTR_UNDERLINE);
        let t = runs.iter().find(|r| r.0 == "T").unwrap();
        assert_eq!(t.1.fg, Some((10, 20, 30)));
        let x = runs.iter().find(|r| r.0 == "X").unwrap();
        assert_eq!(x.1.fg, Some(xterm256(196)));
    }

    #[test]
    fn non_sgr_csi_is_stripped() {
        let mut a = AnsiState::new();
        let runs = a.feed("a\x1b[2Kb\x1b[Hc"); // erase-line + cursor-home ignored
        let text: String = runs.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(text, "abc");
    }

    #[test]
    fn utf8_text_survives_the_byte_scan() {
        let mut a = AnsiState::new();
        let runs = a.feed("\x1b[32mcafé — déjà\x1b[0m");
        assert_eq!(runs[0].0, "café — déjà");
        assert_eq!(runs[0].1.fg, Some((0, 170, 0)));
    }
}
