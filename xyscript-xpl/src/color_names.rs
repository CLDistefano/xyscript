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

/// Every colour, de-duplicated (a name that appears more than once keeps the FIRST spelling but the
/// value [`lookup`] would return — the last one parsed) and sorted case-insensitively by name. The
/// display names are `'static` (borrowed from the embedded source). Built once.
fn entries() -> &'static Vec<(&'static str, (u8, u8, u8))> {
    static E: OnceLock<Vec<(&'static str, (u8, u8, u8))>> = OnceLock::new();
    E.get_or_init(|| {
        // Ordered de-dup: first spelling wins for display, last value wins (matches `table()`/`lookup`).
        let mut order: Vec<&'static str> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut vals: Vec<(u8, u8, u8)> = Vec::new();
        for line in SOURCE.lines() {
            if let Some((name, rgb)) = parse_line(line) {
                let key = name.to_ascii_lowercase();
                match seen.get(&key) {
                    Some(&i) => vals[i] = rgb,
                    None => {
                        seen.insert(key, order.len());
                        order.push(name);
                        vals.push(rgb);
                    }
                }
            }
        }
        let mut v: Vec<(&'static str, (u8, u8, u8))> =
            order.into_iter().zip(vals).collect();
        v.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
        v
    })
}

/// A rendered colour-name listing: the text plus the span layers that colour it — used by the
/// `COLORNAMES` verb. `fg` tints each name in its own colour; `bg` paints a solid swatch block after the
/// values so every colour is visible regardless of the editor theme (a pale name that would vanish as
/// text still shows as a block). Offsets are byte offsets into `text` (all-ASCII, so also rope offsets).
pub struct Listing {
    pub text: String,
    pub fg: Vec<(usize, usize, (u8, u8, u8))>,
    pub bg: Vec<(usize, usize, (u8, u8, u8))>,
}

/// Build the `COLORNAMES` listing, optionally filtered to names containing `filter` (case-insensitive).
pub fn listing(filter: Option<&str>) -> Listing {
    const SWATCH: &str = "      "; // 6 blank cells, painted with the colour as background
    let needle = filter.map(|f| f.trim().to_ascii_lowercase()).filter(|f| !f.is_empty());
    let rows: Vec<&(&str, (u8, u8, u8))> = entries()
        .iter()
        .filter(|(name, _)| needle.as_ref().map_or(true, |n| name.to_ascii_lowercase().contains(n)))
        .collect();
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);

    let mut text = match &needle {
        Some(n) => format!("{} colour name(s) matching {n:?}:\n\n", rows.len()),
        None => format!("{} colour names (X11 / rgb.txt):\n\n", rows.len()),
    };
    let mut fg = Vec::with_capacity(rows.len());
    let mut bg = Vec::with_capacity(rows.len());
    for (name, (r, g, b)) in rows {
        let line_start = text.len();
        fg.push((line_start, line_start + name.len(), (*r, *g, *b)));
        text.push_str(name);
        // Pad to align the values, then the values, then the swatch.
        text.push_str(&" ".repeat(width - name.len()));
        text.push_str(&format!("  {r:>3} {g:>3} {b:>3}  "));
        let sw_start = text.len();
        bg.push((sw_start, sw_start + SWATCH.len(), (*r, *g, *b)));
        text.push_str(SWATCH);
        text.push('\n');
    }
    Listing { text, fg, bg }
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

    #[test]
    fn entries_are_sorted_and_deduped() {
        let e = entries();
        assert!(e.len() > 500);
        // Sorted case-insensitively, and each lowercased name appears once.
        for w in e.windows(2) {
            assert!(w[0].0.to_ascii_lowercase() <= w[1].0.to_ascii_lowercase(), "sorted");
        }
        let mut keys: Vec<String> = e.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "no duplicate names");
    }

    #[test]
    fn listing_text_and_spans_line_up() {
        let l = listing(None);
        assert!(
            l.text.starts_with(&format!("{} colour names", entries().len())),
            "header counts the names: {:?}",
            &l.text[..24]
        );
        // There's one fg span (the name) and one bg span (the swatch) per row.
        assert_eq!(l.fg.len(), l.bg.len());
        assert_eq!(l.fg.len(), entries().len());
        // Every fg span's covered text is exactly that colour's name (offsets align with the text).
        for (s, e, rgb) in &l.fg {
            let name = &l.text[*s..*e];
            assert_eq!(lookup(name), Some(*rgb), "fg span {name:?} maps to its own colour");
        }
        // A filter narrows the rows and updates the header.
        let blue = listing(Some("blue"));
        assert!(blue.text.contains("matching \"blue\""));
        assert!(blue.fg.len() < l.fg.len() && !blue.fg.is_empty());
    }
}
