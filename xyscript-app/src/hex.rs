//! Hex + text ("hex dump") view model — the PURE layout behind `HEX on`.
//!
//! With hex view active the document's raw bytes are shown as a fixed grid: an 8-digit offset
//! gutter, [`BYTES_PER_ROW`] bytes per row as hex pairs (grouped 8 + 8), and a text column showing
//! each byte through the document's encoding (non-printable → `.`). This module owns the pure part —
//! formatting a row and the cell↔offset geometry — so the GUI can raster it and overlay the caret /
//! selection using the same column math. Nothing here touches or mutates bytes.

use crate::encoding::{Charset, Encoding};

/// Bytes shown per row — the universal hexdump width.
pub const BYTES_PER_ROW: usize = 16;

/// Column (0-based char cell) of the FIRST hex digit of byte `i` (`0..BYTES_PER_ROW`) within a row:
/// 8-digit offset + two spaces, then `"XX "` per byte, with one extra space before the second group
/// of 8. So byte 0 is at col 10, byte 8 at col 35, etc.
pub fn hex_digit_col(i: usize) -> usize {
    10 + i * 3 + usize::from(i >= BYTES_PER_ROW / 2)
}

/// Column (0-based char cell) of byte `i`'s glyph in the text column (which follows the hex area and
/// a one-space separator).
pub fn text_col(i: usize) -> usize {
    // hex area = 10 (gutter) + 16*3 (pairs) + 1 (group gap) = 59, then a 1-space separator → 60.
    60 + i
}

/// Total width, in char cells, of a full row (`text_col(last) + 1`) — the canvas is sized to this.
pub const ROW_WIDTH: usize = 60 + BYTES_PER_ROW;

/// The text-column glyph for a byte, decoded through `enc`. Control / non-printable bytes — and any
/// byte the encoding can't represent as one printable char (e.g. a lone 0x80+ under UTF-8) — render
/// as `.`. Single-byte code pages (CP437, Latin-1, …) show their high-byte glyphs.
pub fn text_glyph(b: u8, enc: &Charset) -> char {
    let s = enc.decode(&[b]);
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if !c.is_control() && c != '\u{FFFD}' => c,
        _ => '.',
    }
}

/// The full display line for one row of up to [`BYTES_PER_ROW`] bytes starting at `base_offset`:
/// `00000000  48 65 6c 6c 6f 20 77 6f  72 6c 64 21 0a 00 ff fe  Hello world!....`
/// A short final row pads the hex area with spaces so the text column stays aligned; its text column
/// holds only the bytes present.
pub fn format_row(bytes: &[u8], base_offset: usize, enc: &Charset) -> String {
    let mut s = String::with_capacity(ROW_WIDTH);
    s.push_str(&format!("{base_offset:08x}  "));
    for i in 0..BYTES_PER_ROW {
        if i == BYTES_PER_ROW / 2 {
            s.push(' '); // gap between the two groups of 8
        }
        match bytes.get(i) {
            Some(b) => s.push_str(&format!("{b:02x} ")),
            None => s.push_str("   "), // pad a short final row
        }
    }
    s.push(' '); // separator before the text column
    for &b in bytes.iter().take(BYTES_PER_ROW) {
        s.push(text_glyph(b, enc));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding;

    #[test]
    fn format_row_lays_out_offset_hex_and_text_aligned_to_the_geometry() {
        let enc = encoding::cp437();
        let bytes: Vec<u8> = b"Hello world!\n\x00\xff\xfe".to_vec(); // 16 bytes
        let row = format_row(&bytes, 0, &enc);
        // The offset gutter.
        assert!(row.starts_with("00000000  "));
        // Each byte's first hex digit sits exactly at hex_digit_col(i).
        let chars: Vec<char> = row.chars().collect();
        for (i, &b) in bytes.iter().enumerate() {
            let hi = std::char::from_digit((b >> 4) as u32, 16).unwrap();
            assert_eq!(chars[hex_digit_col(i)], hi, "byte {i} hi-nibble at wrong col");
        }
        // The group gap: byte 8 starts at col 35 (10 + 24 + 1).
        assert_eq!(hex_digit_col(8), 35);
        // Text column: printable ASCII shown, control/00/high → '.'; 0xff/0xfe are CP437 glyphs.
        assert_eq!(chars[text_col(0)], 'H');
        assert_eq!(chars[text_col(12)], '.'); // '\n' (control)
        assert_eq!(chars[text_col(13)], '.'); // 0x00 (control)
        // Row width matches ROW_WIDTH (full row).
        assert_eq!(row.chars().count(), ROW_WIDTH);
    }

    #[test]
    fn short_final_row_pads_hex_but_only_shows_present_text() {
        let enc = encoding::cp437();
        let row = format_row(b"ab", 0x10, &enc);
        assert!(row.starts_with("00000010  61 62 "));
        // Only two text glyphs, at their columns.
        let chars: Vec<char> = row.chars().collect();
        assert_eq!(chars[text_col(0)], 'a');
        assert_eq!(chars[text_col(1)], 'b');
        // Nothing padded into the text column beyond the present bytes.
        assert_eq!(row.chars().count(), text_col(1) + 1);
    }

    #[test]
    fn utf8_shows_ascii_and_dots_high_bytes() {
        let enc = encoding::utf8();
        assert_eq!(text_glyph(b'A', &enc), 'A');
        assert_eq!(text_glyph(0x09, &enc), '.'); // tab (control)
        assert_eq!(text_glyph(0xC3, &enc), '.'); // lone UTF-8 lead byte → invalid alone
    }
}
