//! Named colours for the colour verbs (`MDA red`, `MDALL blue …`, `BGCOLOR salmon`, …).
//!
//! The table is the classic X11 / `rgb.txt` name set (≈650 names, `grey0`–`grey100`, `SlateGray1`–`4`,
//! and the familiar English names), embedded from [`color_names.lst`] — a plain XPL `set NAME = "R G B"`
//! script (the same format a user can `RUN` to get `$NAME` variables). It is parsed ONCE, lazily, into a
//! case-insensitive map, so a name is a zero-setup alternative to a numeric triple anywhere a colour is
//! taken. To refresh the list, replace `color_names.lst` (keep the `set NAME = "R G B"` shape).

use std::collections::HashMap;
use std::sync::OnceLock;

/// The embedded colour list (an XPL `set` script; `#` comments and blank lines are ignored).
const SOURCE: &str = include_str!("color_names.lst");

/// Parse the embedded list once into a lowercase-name → RGB map.
fn table() -> &'static HashMap<String, (u8, u8, u8)> {
    static MAP: OnceLock<HashMap<String, (u8, u8, u8)>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for line in SOURCE.lines() {
            if let Some((name, rgb)) = parse_line(line) {
                map.insert(name.to_ascii_lowercase(), rgb);
            }
        }
        map
    })
}

/// Parse one `set NAME = "R G B"` line → `(name, (r,g,b))`. Returns `None` for a comment, a blank line,
/// or anything that doesn't fit the shape (so a stray line can't panic the load).
fn parse_line(line: &str) -> Option<(&str, (u8, u8, u8))> {
    let line = line.trim();
    let rest = line.strip_prefix("set ")?; // `NAME = "R G B"`
    let (name, value) = rest.split_once('=')?;
    let name = name.trim();
    let triple = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut it = triple.split_whitespace();
    let r = it.next()?.parse().ok()?;
    let g = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    if it.next().is_some() || name.is_empty() {
        return None;
    }
    Some((name, (r, g, b)))
}

/// Look up a colour by name (case-insensitive). `None` if it isn't a known name.
pub fn lookup(name: &str) -> Option<(u8, u8, u8)> {
    table().get(&name.trim().to_ascii_lowercase()).copied()
}

/// Whether `word` is a known colour name — lets a caller (e.g. `MDALL`'s style/pattern split) tell a
/// one-word colour name from the start of a pattern without consuming it.
pub fn is_name(word: &str) -> bool {
    table().contains_key(&word.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_embedded_list() {
        // A healthy chunk of names loaded (the file has ~650).
        assert!(table().len() > 500, "only {} names parsed", table().len());
    }

    #[test]
    fn looks_up_names_case_insensitively() {
        assert_eq!(lookup("black"), Some((0, 0, 0)));
        assert_eq!(lookup("Black"), Some((0, 0, 0)));
        assert_eq!(lookup("WHITE"), Some((255, 255, 255)));
        assert_eq!(lookup("  red  "), Some((255, 0, 0)));
        assert!(lookup("notacolour").is_none());
        assert!(is_name("SlateGray") && !is_name("frobnicate"));
    }

    #[test]
    fn parses_names_with_punctuation() {
        // Two entries carry an apostrophe / hyphen — they must still load.
        assert!(lookup("baker'schocolate").is_some());
        assert!(lookup("semi-sweetchocolate").is_some());
    }
}
