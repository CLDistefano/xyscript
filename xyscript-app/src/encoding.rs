//! Character encodings (`byte-text-engine-plan.md`, step 1).
//!
//! In the byte-oriented document model the file's **bytes are the truth** and an
//! `Encoding` is a switchable **view**: it [`decode`](Encoding::decode)s bytes →
//! display text and [`encode`](Encoding::encode)s typed text → bytes. Switching
//! the encoding re-decodes the same bytes (no byte change); a character the
//! encoding can't represent encodes to `?` (the EditPad behavior).
//!
//! Step 1 implements **single-byte** encodings (1 byte ↔ 1 character) — the DOS
//! code pages, Windows-125x, ISO-8859-*, Mac, KOI8 — which cover all the XyWrite
//! files and most of the breadth. The trait's `decode`/`encode` are slice-based,
//! so they are already N:1-capable; multi-byte encodings (UTF-8/16) plug in later
//! without reshaping the API.
//!
//! Note (validated here): in **CP437**, byte `0xAE` is `«` and `0xAF` is `»` —
//! so XyWrite's embedded function-code delimiters are the guillemets `«…»`
//! (`«FC»`, `«MDBO»`), confirming CP437 is the right view for these files.

use std::collections::HashMap;

/// A character encoding: a reversible (lossy on encode) mapping between bytes and
/// display text.
///
/// The default implementations of the char-boundary methods describe a
/// **single-byte** encoding (1 byte == 1 char); multi-byte encodings (UTF-8)
/// override them. This is the N:1 capability BE2 reserved — adding UTF-8 needs no
/// API reshape, only overrides.
pub trait Encoding {
    /// A short canonical name (e.g. `"cp437"`).
    fn name(&self) -> &str;
    /// Whether one byte always maps to one character (true for the single-byte
    /// code pages). The byte buffer can use byte==char arithmetic when true.
    fn is_single_byte(&self) -> bool {
        true
    }
    /// Decode bytes into display text. Single-byte code pages that map all 256
    /// values never need a replacement on decode.
    fn decode(&self, bytes: &[u8]) -> String;
    /// Encode text into bytes for storage. Characters the encoding can't
    /// represent become `?`.
    fn encode(&self, text: &str) -> Vec<u8>;
    /// Like [`Self::encode`] but *strict*: the first character the encoding cannot represent is
    /// returned as `Err(char)` instead of being substituted with `?`. Used by `WRITEFILE/STRICT` so a
    /// script can refuse a lossy write. The default (for UTF-8, where every character is representable)
    /// never fails.
    fn encode_strict(&self, text: &str) -> Result<Vec<u8>, char> {
        Ok(self.encode(text))
    }

    /// Byte length of the character whose lead byte is `b` (1 for single-byte;
    /// the UTF-8 lead-byte length otherwise). A continuation/invalid lead yields
    /// 1 (lossy-tolerant). Lets the buffer step the cursor by whole characters.
    fn lead_len(&self, _b: u8) -> usize {
        1
    }
    /// Whether `b` is a trailing/continuation byte of a multi-byte character
    /// (never true for single-byte encodings). Lets the buffer scan back to a
    /// character boundary.
    fn is_continuation(&self, _b: u8) -> bool {
        false
    }
    /// Number of characters in `bytes` (== byte count for single-byte). Used for
    /// the cursor's column and goal-column-preserving vertical motion.
    fn char_count(&self, bytes: &[u8]) -> usize {
        bytes.len()
    }
}

/// A single-byte code page: a fixed `byte -> char` table (all 256 defined) plus
/// the reverse `char -> byte` map.
#[derive(Debug, Clone)]
pub struct SingleByteEncoding {
    name: &'static str,
    to_char: [char; 256],
    to_byte: HashMap<char, u8>,
}

impl SingleByteEncoding {
    /// Build from a 256-entry `byte -> char` table.
    fn from_table(name: &'static str, to_char: [char; 256]) -> SingleByteEncoding {
        let mut to_byte = HashMap::with_capacity(256);
        for (b, &c) in to_char.iter().enumerate() {
            // Bijective code pages: first writer wins (no collisions expected).
            to_byte.entry(c).or_insert(b as u8);
        }
        SingleByteEncoding { name, to_char, to_byte }
    }
}

impl Encoding for SingleByteEncoding {
    fn name(&self) -> &str {
        self.name
    }
    fn decode(&self, bytes: &[u8]) -> String {
        bytes.iter().map(|&b| self.to_char[b as usize]).collect()
    }
    fn encode(&self, text: &str) -> Vec<u8> {
        text.chars()
            .map(|c| self.to_byte.get(&c).copied().unwrap_or(b'?'))
            .collect()
    }
    fn encode_strict(&self, text: &str) -> Result<Vec<u8>, char> {
        let mut out = Vec::with_capacity(text.len());
        for c in text.chars() {
            match self.to_byte.get(&c) {
                Some(&b) => out.push(b),
                None => return Err(c), // not representable in this code page
            }
        }
        Ok(out)
    }
}

/// UTF-8 — the multi-byte encoding for new authoring documents (1–4 bytes per
/// char). Every character is representable, so encode never substitutes `?`.
#[derive(Debug, Clone)]
pub struct Utf8Encoding;

impl Encoding for Utf8Encoding {
    fn name(&self) -> &str {
        "utf-8"
    }
    fn is_single_byte(&self) -> bool {
        false
    }
    fn decode(&self, bytes: &[u8]) -> String {
        // Lossy: invalid sequences show U+FFFD but the bytes are preserved in the
        // rope, so a malformed UTF-8 file still round-trips byte-exact on save.
        String::from_utf8_lossy(bytes).into_owned()
    }
    fn encode(&self, text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }
    fn lead_len(&self, b: u8) -> usize {
        match b {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1, // continuation byte or invalid lead -> 1 (lossy)
        }
    }
    fn is_continuation(&self, b: u8) -> bool {
        (b & 0xC0) == 0x80
    }
    fn char_count(&self, bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
    }
}

/// The encoding view a [`Buffer`](crate::text::Buffer) holds: a closed set of
/// concrete encodings (single-byte code pages or UTF-8), so it stays `Clone`/
/// allocation-free with no trait objects. Delegates the [`Encoding`] interface to
/// the active variant.
#[derive(Debug, Clone)]
pub enum Charset {
    // Boxed: the single-byte table is ~1 KB, and a `Charset` is cloned with every
    // `Document`; boxing keeps the enum pointer-sized.
    SingleByte(Box<SingleByteEncoding>),
    Utf8(Utf8Encoding),
}

impl Charset {
    fn inner(&self) -> &dyn Encoding {
        match self {
            Charset::SingleByte(e) => e.as_ref(),
            Charset::Utf8(e) => e,
        }
    }
}

impl Encoding for Charset {
    fn name(&self) -> &str {
        self.inner().name()
    }
    fn is_single_byte(&self) -> bool {
        self.inner().is_single_byte()
    }
    fn decode(&self, bytes: &[u8]) -> String {
        self.inner().decode(bytes)
    }
    fn encode(&self, text: &str) -> Vec<u8> {
        self.inner().encode(text)
    }
    fn encode_strict(&self, text: &str) -> Result<Vec<u8>, char> {
        self.inner().encode_strict(text)
    }
    fn lead_len(&self, b: u8) -> usize {
        self.inner().lead_len(b)
    }
    fn is_continuation(&self, b: u8) -> bool {
        self.inner().is_continuation(b)
    }
    fn char_count(&self, bytes: &[u8]) -> usize {
        self.inner().char_count(bytes)
    }
}

/// CP437 (the original IBM PC / DOS US OEM code page). Low 0x00–0x7F are ASCII;
/// 0x80–0xFF are the accented letters, box-drawing, and symbol glyphs.
pub fn cp437() -> Charset {
    let mut t = ['\u{0}'; 256];
    for (i, slot) in t.iter_mut().enumerate().take(0x80) {
        *slot = char::from_u32(i as u32).unwrap(); // 0x00..0x7F = ASCII
    }
    for (i, &cp) in CP437_HIGH.iter().enumerate() {
        t[0x80 + i] = char::from_u32(cp).unwrap();
    }
    Charset::SingleByte(Box::new(SingleByteEncoding::from_table("cp437", t)))
}

/// ISO-8859-1 (Latin-1): byte value == Unicode code point. A trivial second
/// single-byte encoding (useful for testing and as a fallback).
pub fn latin1() -> Charset {
    let mut t = ['\u{0}'; 256];
    for (i, slot) in t.iter_mut().enumerate() {
        *slot = char::from_u32(i as u32).unwrap();
    }
    Charset::SingleByte(Box::new(SingleByteEncoding::from_table("latin1", t)))
}

/// UTF-8 — the default view for new documents (step 7).
pub fn utf8() -> Charset {
    Charset::Utf8(Utf8Encoding)
}

/// Build a single-byte `Charset` from a high-half (0x80–0xFF) Unicode table; the low half is ASCII.
fn single_byte(name: &'static str, high: &[u32; 128]) -> Charset {
    let mut t = ['\u{0}'; 256];
    for (i, slot) in t.iter_mut().enumerate().take(0x80) {
        *slot = char::from_u32(i as u32).unwrap();
    }
    for (i, &cp) in high.iter().enumerate() {
        t[0x80 + i] = char::from_u32(cp).unwrap();
    }
    Charset::SingleByte(Box::new(SingleByteEncoding::from_table(name, t)))
}

// Additional legacy code pages. Each is single-byte; bytes with no character in the standard mapping
// are kept losslessly as a unique Private-Use code point (`0xE000 + byte`) so editing never corrupts
// an unrecognized byte. (Tables generated from the Unicode Consortium mapping files.)
/// CP850 — DOS Latin-1 (Western European).
pub fn cp850() -> Charset { single_byte("cp850", &CP850_HIGH) }
/// CP852 — DOS Latin-2 (Central European).
pub fn cp852() -> Charset { single_byte("cp852", &CP852_HIGH) }
/// CP857 — DOS Turkish.
pub fn cp857() -> Charset { single_byte("cp857", &CP857_HIGH) }
/// CP866 — DOS Cyrillic (Russian).
pub fn cp866() -> Charset { single_byte("cp866", &CP866_HIGH) }
/// CP775 — DOS Baltic Rim.
pub fn cp775() -> Charset { single_byte("cp775", &CP775_HIGH) }
/// Windows-1252 — Western European Windows (ANSI).
pub fn windows1252() -> Charset { single_byte("windows-1252", &CP1252_HIGH) }
/// ISO-8859-15 (Latin-9) — Latin-1 with the euro sign and a few revised letters.
pub fn latin9() -> Charset { single_byte("latin9", &ISO8859_15_HIGH) }
/// Mac OS Roman — the classic (pre-OSX) Macintosh Western encoding.
pub fn macroman() -> Charset { single_byte("macroman", &MACROMAN_HIGH) }

/// CP866X — a manufactured CP866 variant for XyWrite back-compatibility. Identical to CP866 except:
/// bytes 0xAE/0xAF (174/175) carry the CP437 guillemets `«`/`»` that XyWrite uses to wrap embedded
/// commands (instead of Cyrillic `о`/`п`), and those two letters are relocated to 0xDD/0xDE (221/222),
/// displacing CP866's `▌`/`▐` half-block glyphs. Stays bijective, so it round-trips.
pub fn cp866x() -> Charset {
    let mut high = CP866_HIGH;
    high[0xAE - 0x80] = 0x00AB; // « — XyWrite open-command marker (was Cyrillic о)
    high[0xAF - 0x80] = 0x00BB; // » — XyWrite close-command marker (was Cyrillic п)
    high[0xDD - 0x80] = 0x043E; // о relocated here (was ▌)
    high[0xDE - 0x80] = 0x043F; // п relocated here (was ▐)
    single_byte("cp866x", &high)
}

/// Every selectable encoding in menu order: (canonical name = what [`Encoding::name`] returns and
/// `$EN` reports, human label). Both the "Display as" and "Convert to" menus render from this, and
/// every canonical name resolves through [`by_name`]. Add an encoding here + a `by_name` arm and it
/// appears in both menus automatically.
pub const CATALOG: &[(&str, &str)] = &[
    ("utf-8", "UTF-8"),
    ("cp437", "CP437 (DOS US)"),
    ("cp850", "CP850 (DOS Western European)"),
    ("cp852", "CP852 (DOS Central European)"),
    ("cp857", "CP857 (DOS Turkish)"),
    ("cp866", "CP866 (DOS Cyrillic)"),
    ("cp866x", "CP866X (DOS Cyrillic, XyWrite «»)"),
    ("cp775", "CP775 (DOS Baltic)"),
    ("windows-1252", "Windows-1252 (Western European)"),
    ("latin1", "Latin-1 (ISO-8859-1)"),
    ("latin9", "Latin-9 (ISO-8859-15)"),
    ("macroman", "Mac OS Roman"),
];

/// Look up an encoding by name/alias (for the `ENCODING` verb and the menus). Case-, dash-, and
/// space-insensitive.
pub fn by_name(name: &str) -> Option<Charset> {
    match name.to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
        "cp437" | "437" | "oemus" | "ibm437" | "dos" => Some(cp437()),
        "cp850" | "850" | "ibm850" => Some(cp850()),
        "cp852" | "852" | "ibm852" => Some(cp852()),
        "cp857" | "857" | "ibm857" => Some(cp857()),
        "cp866" | "866" | "ibm866" => Some(cp866()),
        "cp866x" | "866x" | "ibm866x" => Some(cp866x()),
        "cp775" | "775" | "ibm775" => Some(cp775()),
        "windows1252" | "cp1252" | "1252" | "win1252" | "ansi" => Some(windows1252()),
        "latin1" | "iso88591" | "88591" | "l1" => Some(latin1()),
        "latin9" | "iso885915" | "885915" | "l9" => Some(latin9()),
        "macroman" | "macintosh" | "mac" | "macosroman" => Some(macroman()),
        "utf8" | "u8" | "unicode" => Some(utf8()),
        _ => None,
    }
}

/// CP437 mapping for bytes 0x80..=0xFF → Unicode scalar values.
#[rustfmt::skip]
const CP437_HIGH: [u32; 128] = [
    0x00C7,0x00FC,0x00E9,0x00E2,0x00E4,0x00E0,0x00E5,0x00E7, // 80-87 Ç ü é â ä à å ç
    0x00EA,0x00EB,0x00E8,0x00EF,0x00EE,0x00EC,0x00C4,0x00C5, // 88-8F ê ë è ï î ì Ä Å
    0x00C9,0x00E6,0x00C6,0x00F4,0x00F6,0x00F2,0x00FB,0x00F9, // 90-97 É æ Æ ô ö ò û ù
    0x00FF,0x00D6,0x00DC,0x00A2,0x00A3,0x00A5,0x20A7,0x0192, // 98-9F ÿ Ö Ü ¢ £ ¥ ₧ ƒ
    0x00E1,0x00ED,0x00F3,0x00FA,0x00F1,0x00D1,0x00AA,0x00BA, // A0-A7 á í ó ú ñ Ñ ª º
    0x00BF,0x2310,0x00AC,0x00BD,0x00BC,0x00A1,0x00AB,0x00BB, // A8-AF ¿ ⌐ ¬ ½ ¼ ¡ « »
    0x2591,0x2592,0x2593,0x2502,0x2524,0x2561,0x2562,0x2556, // B0-B7 ░ ▒ ▓ │ ┤ ╡ ╢ ╖
    0x2555,0x2563,0x2551,0x2557,0x255D,0x255C,0x255B,0x2510, // B8-BF ╕ ╣ ║ ╗ ╝ ╜ ╛ ┐
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x255E,0x255F, // C0-C7 └ ┴ ┬ ├ ─ ┼ ╞ ╟
    0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x2567, // C8-CF ╚ ╔ ╩ ╦ ╠ ═ ╬ ╧
    0x2568,0x2564,0x2565,0x2559,0x2558,0x2552,0x2553,0x256B, // D0-D7 ╨ ╤ ╥ ╙ ╘ ╒ ╓ ╫
    0x256A,0x2518,0x250C,0x2588,0x2584,0x258C,0x2590,0x2580, // D8-DF ╪ ┘ ┌ █ ▄ ▌ ▐ ▀
    0x03B1,0x00DF,0x0393,0x03C0,0x03A3,0x03C3,0x00B5,0x03C4, // E0-E7 α ß Γ π Σ σ µ τ
    0x03A6,0x0398,0x03A9,0x03B4,0x221E,0x03C6,0x03B5,0x2229, // E8-EF Φ Θ Ω δ ∞ φ ε ∩
    0x2261,0x00B1,0x2265,0x2264,0x2320,0x2321,0x00F7,0x2248, // F0-F7 ≡ ± ≥ ≤ ⌠ ⌡ ÷ ≈
    0x00B0,0x2219,0x00B7,0x221A,0x207F,0x00B2,0x25A0,0x00A0, // F8-FF ° ∙ · √ ⁿ ² ■ NBSP
];

// The high-half (0x80–0xFF) Unicode mappings for the added code pages, generated from the Unicode
// Consortium mapping files. `0xE0xx` entries are bytes the code page leaves undefined, kept as a
// unique Private-Use code point so they round-trip losslessly.

/// CP850 (DOS Western European).
#[rustfmt::skip]
const CP850_HIGH: [u32; 128] = [
    0x00C7,0x00FC,0x00E9,0x00E2,0x00E4,0x00E0,0x00E5,0x00E7, 0x00EA,0x00EB,0x00E8,0x00EF,0x00EE,0x00EC,0x00C4,0x00C5,
    0x00C9,0x00E6,0x00C6,0x00F4,0x00F6,0x00F2,0x00FB,0x00F9, 0x00FF,0x00D6,0x00DC,0x00F8,0x00A3,0x00D8,0x00D7,0x0192,
    0x00E1,0x00ED,0x00F3,0x00FA,0x00F1,0x00D1,0x00AA,0x00BA, 0x00BF,0x00AE,0x00AC,0x00BD,0x00BC,0x00A1,0x00AB,0x00BB,
    0x2591,0x2592,0x2593,0x2502,0x2524,0x00C1,0x00C2,0x00C0, 0x00A9,0x2563,0x2551,0x2557,0x255D,0x00A2,0x00A5,0x2510,
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x00E3,0x00C3, 0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x00A4,
    0x00F0,0x00D0,0x00CA,0x00CB,0x00C8,0x0131,0x00CD,0x00CE, 0x00CF,0x2518,0x250C,0x2588,0x2584,0x00A6,0x00CC,0x2580,
    0x00D3,0x00DF,0x00D4,0x00D2,0x00F5,0x00D5,0x00B5,0x00FE, 0x00DE,0x00DA,0x00DB,0x00D9,0x00FD,0x00DD,0x00AF,0x00B4,
    0x00AD,0x00B1,0x2017,0x00BE,0x00B6,0x00A7,0x00F7,0x00B8, 0x00B0,0x00A8,0x00B7,0x00B9,0x00B3,0x00B2,0x25A0,0x00A0,
];

/// CP852 (DOS Central European / Latin-2).
#[rustfmt::skip]
const CP852_HIGH: [u32; 128] = [
    0x00C7,0x00FC,0x00E9,0x00E2,0x00E4,0x016F,0x0107,0x00E7, 0x0142,0x00EB,0x0150,0x0151,0x00EE,0x0179,0x00C4,0x0106,
    0x00C9,0x0139,0x013A,0x00F4,0x00F6,0x013D,0x013E,0x015A, 0x015B,0x00D6,0x00DC,0x0164,0x0165,0x0141,0x00D7,0x010D,
    0x00E1,0x00ED,0x00F3,0x00FA,0x0104,0x0105,0x017D,0x017E, 0x0118,0x0119,0x00AC,0x017A,0x010C,0x015F,0x00AB,0x00BB,
    0x2591,0x2592,0x2593,0x2502,0x2524,0x00C1,0x00C2,0x011A, 0x015E,0x2563,0x2551,0x2557,0x255D,0x017B,0x017C,0x2510,
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x0102,0x0103, 0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x00A4,
    0x0111,0x0110,0x010E,0x00CB,0x010F,0x0147,0x00CD,0x00CE, 0x011B,0x2518,0x250C,0x2588,0x2584,0x0162,0x016E,0x2580,
    0x00D3,0x00DF,0x00D4,0x0143,0x0144,0x0148,0x0160,0x0161, 0x0154,0x00DA,0x0155,0x0170,0x00FD,0x00DD,0x0163,0x00B4,
    0x00AD,0x02DD,0x02DB,0x02C7,0x02D8,0x00A7,0x00F7,0x00B8, 0x00B0,0x00A8,0x02D9,0x0171,0x0158,0x0159,0x25A0,0x00A0,
];

/// CP857 (DOS Turkish). 0xD5/0xE7/0xF2 are undefined → unique PUA.
#[rustfmt::skip]
const CP857_HIGH: [u32; 128] = [
    0x00C7,0x00FC,0x00E9,0x00E2,0x00E4,0x00E0,0x00E5,0x00E7, 0x00EA,0x00EB,0x00E8,0x00EF,0x00EE,0x0131,0x00C4,0x00C5,
    0x00C9,0x00E6,0x00C6,0x00F4,0x00F6,0x00F2,0x00FB,0x00F9, 0x0130,0x00D6,0x00DC,0x00F8,0x00A3,0x00D8,0x015E,0x015F,
    0x00E1,0x00ED,0x00F3,0x00FA,0x00F1,0x00D1,0x011E,0x011F, 0x00BF,0x00AE,0x00AC,0x00BD,0x00BC,0x00A1,0x00AB,0x00BB,
    0x2591,0x2592,0x2593,0x2502,0x2524,0x00C1,0x00C2,0x00C0, 0x00A9,0x2563,0x2551,0x2557,0x255D,0x00A2,0x00A5,0x2510,
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x00E3,0x00C3, 0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x00A4,
    0x00BA,0x00AA,0x00CA,0x00CB,0x00C8,0xE0D5,0x00CD,0x00CE, 0x00CF,0x2518,0x250C,0x2588,0x2584,0x00A6,0x00CC,0x2580,
    0x00D3,0x00DF,0x00D4,0x00D2,0x00F5,0x00D5,0x00B5,0xE0E7, 0x00D7,0x00DA,0x00DB,0x00D9,0x00EC,0x00FF,0x00AF,0x00B4,
    0x00AD,0x00B1,0xE0F2,0x00BE,0x00B6,0x00A7,0x00F7,0x00B8, 0x00B0,0x00A8,0x00B7,0x00B9,0x00B3,0x00B2,0x25A0,0x00A0,
];

/// CP866 (DOS Cyrillic).
#[rustfmt::skip]
const CP866_HIGH: [u32; 128] = [
    0x0410,0x0411,0x0412,0x0413,0x0414,0x0415,0x0416,0x0417, 0x0418,0x0419,0x041A,0x041B,0x041C,0x041D,0x041E,0x041F,
    0x0420,0x0421,0x0422,0x0423,0x0424,0x0425,0x0426,0x0427, 0x0428,0x0429,0x042A,0x042B,0x042C,0x042D,0x042E,0x042F,
    0x0430,0x0431,0x0432,0x0433,0x0434,0x0435,0x0436,0x0437, 0x0438,0x0439,0x043A,0x043B,0x043C,0x043D,0x043E,0x043F,
    0x2591,0x2592,0x2593,0x2502,0x2524,0x2561,0x2562,0x2556, 0x2555,0x2563,0x2551,0x2557,0x255D,0x255C,0x255B,0x2510,
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x255E,0x255F, 0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x2567,
    0x2568,0x2564,0x2565,0x2559,0x2558,0x2552,0x2553,0x256B, 0x256A,0x2518,0x250C,0x2588,0x2584,0x258C,0x2590,0x2580,
    0x0440,0x0441,0x0442,0x0443,0x0444,0x0445,0x0446,0x0447, 0x0448,0x0449,0x044A,0x044B,0x044C,0x044D,0x044E,0x044F,
    0x0401,0x0451,0x0404,0x0454,0x0407,0x0457,0x040E,0x045E, 0x00B0,0x2219,0x00B7,0x221A,0x2116,0x00A4,0x25A0,0x00A0,
];

/// CP775 (DOS Baltic Rim).
#[rustfmt::skip]
const CP775_HIGH: [u32; 128] = [
    0x0106,0x00FC,0x00E9,0x0101,0x00E4,0x0123,0x00E5,0x0107, 0x0142,0x0113,0x0156,0x0157,0x012B,0x0179,0x00C4,0x00C5,
    0x00C9,0x00E6,0x00C6,0x014D,0x00F6,0x0122,0x00A2,0x015A, 0x015B,0x00D6,0x00DC,0x00F8,0x00A3,0x00D8,0x00D7,0x00A4,
    0x0100,0x012A,0x00F3,0x017B,0x017C,0x017A,0x201D,0x00A6, 0x00A9,0x00AE,0x00AC,0x00BD,0x00BC,0x0141,0x00AB,0x00BB,
    0x2591,0x2592,0x2593,0x2502,0x2524,0x0104,0x010C,0x0118, 0x0116,0x2563,0x2551,0x2557,0x255D,0x012E,0x0160,0x2510,
    0x2514,0x2534,0x252C,0x251C,0x2500,0x253C,0x0172,0x016A, 0x255A,0x2554,0x2569,0x2566,0x2560,0x2550,0x256C,0x017D,
    0x0105,0x010D,0x0119,0x0117,0x012F,0x0161,0x0173,0x016B, 0x017E,0x2518,0x250C,0x2588,0x2584,0x258C,0x2590,0x2580,
    0x00D3,0x00DF,0x014C,0x0143,0x00F5,0x00D5,0x00B5,0x0144, 0x0136,0x0137,0x013B,0x013C,0x0146,0x0112,0x0145,0x2019,
    0x00AD,0x00B1,0x201C,0x00BE,0x00B6,0x00A7,0x00F7,0x201E, 0x00B0,0x2219,0x00B7,0x00B9,0x00B3,0x00B2,0x25A0,0x00A0,
];

/// Windows-1252 (Western European / ANSI). 0x81/0x8D/0x8F/0x90/0x9D are undefined → unique PUA.
#[rustfmt::skip]
const CP1252_HIGH: [u32; 128] = [
    0x20AC,0xE081,0x201A,0x0192,0x201E,0x2026,0x2020,0x2021, 0x02C6,0x2030,0x0160,0x2039,0x0152,0xE08D,0x017D,0xE08F,
    0xE090,0x2018,0x2019,0x201C,0x201D,0x2022,0x2013,0x2014, 0x02DC,0x2122,0x0161,0x203A,0x0153,0xE09D,0x017E,0x0178,
    0x00A0,0x00A1,0x00A2,0x00A3,0x00A4,0x00A5,0x00A6,0x00A7, 0x00A8,0x00A9,0x00AA,0x00AB,0x00AC,0x00AD,0x00AE,0x00AF,
    0x00B0,0x00B1,0x00B2,0x00B3,0x00B4,0x00B5,0x00B6,0x00B7, 0x00B8,0x00B9,0x00BA,0x00BB,0x00BC,0x00BD,0x00BE,0x00BF,
    0x00C0,0x00C1,0x00C2,0x00C3,0x00C4,0x00C5,0x00C6,0x00C7, 0x00C8,0x00C9,0x00CA,0x00CB,0x00CC,0x00CD,0x00CE,0x00CF,
    0x00D0,0x00D1,0x00D2,0x00D3,0x00D4,0x00D5,0x00D6,0x00D7, 0x00D8,0x00D9,0x00DA,0x00DB,0x00DC,0x00DD,0x00DE,0x00DF,
    0x00E0,0x00E1,0x00E2,0x00E3,0x00E4,0x00E5,0x00E6,0x00E7, 0x00E8,0x00E9,0x00EA,0x00EB,0x00EC,0x00ED,0x00EE,0x00EF,
    0x00F0,0x00F1,0x00F2,0x00F3,0x00F4,0x00F5,0x00F6,0x00F7, 0x00F8,0x00F9,0x00FA,0x00FB,0x00FC,0x00FD,0x00FE,0x00FF,
];

/// ISO-8859-15 (Latin-9). 0x80–0x9F are the C1 control range (mapped to U+0080–U+009F).
#[rustfmt::skip]
const ISO8859_15_HIGH: [u32; 128] = [
    0x0080,0x0081,0x0082,0x0083,0x0084,0x0085,0x0086,0x0087, 0x0088,0x0089,0x008A,0x008B,0x008C,0x008D,0x008E,0x008F,
    0x0090,0x0091,0x0092,0x0093,0x0094,0x0095,0x0096,0x0097, 0x0098,0x0099,0x009A,0x009B,0x009C,0x009D,0x009E,0x009F,
    0x00A0,0x00A1,0x00A2,0x00A3,0x20AC,0x00A5,0x0160,0x00A7, 0x0161,0x00A9,0x00AA,0x00AB,0x00AC,0x00AD,0x00AE,0x00AF,
    0x00B0,0x00B1,0x00B2,0x00B3,0x017D,0x00B5,0x00B6,0x00B7, 0x017E,0x00B9,0x00BA,0x00BB,0x0152,0x0153,0x0178,0x00BF,
    0x00C0,0x00C1,0x00C2,0x00C3,0x00C4,0x00C5,0x00C6,0x00C7, 0x00C8,0x00C9,0x00CA,0x00CB,0x00CC,0x00CD,0x00CE,0x00CF,
    0x00D0,0x00D1,0x00D2,0x00D3,0x00D4,0x00D5,0x00D6,0x00D7, 0x00D8,0x00D9,0x00DA,0x00DB,0x00DC,0x00DD,0x00DE,0x00DF,
    0x00E0,0x00E1,0x00E2,0x00E3,0x00E4,0x00E5,0x00E6,0x00E7, 0x00E8,0x00E9,0x00EA,0x00EB,0x00EC,0x00ED,0x00EE,0x00EF,
    0x00F0,0x00F1,0x00F2,0x00F3,0x00F4,0x00F5,0x00F6,0x00F7, 0x00F8,0x00F9,0x00FA,0x00FB,0x00FC,0x00FD,0x00FE,0x00FF,
];

/// Mac OS Roman.
#[rustfmt::skip]
const MACROMAN_HIGH: [u32; 128] = [
    0x00C4,0x00C5,0x00C7,0x00C9,0x00D1,0x00D6,0x00DC,0x00E1, 0x00E0,0x00E2,0x00E4,0x00E3,0x00E5,0x00E7,0x00E9,0x00E8,
    0x00EA,0x00EB,0x00ED,0x00EC,0x00EE,0x00EF,0x00F1,0x00F3, 0x00F2,0x00F4,0x00F6,0x00F5,0x00FA,0x00F9,0x00FB,0x00FC,
    0x2020,0x00B0,0x00A2,0x00A3,0x00A7,0x2022,0x00B6,0x00DF, 0x00AE,0x00A9,0x2122,0x00B4,0x00A8,0x2260,0x00C6,0x00D8,
    0x221E,0x00B1,0x2264,0x2265,0x00A5,0x00B5,0x2202,0x2211, 0x220F,0x03C0,0x222B,0x00AA,0x00BA,0x03A9,0x00E6,0x00F8,
    0x00BF,0x00A1,0x00AC,0x221A,0x0192,0x2248,0x2206,0x00AB, 0x00BB,0x2026,0x00A0,0x00C0,0x00C3,0x00D5,0x0152,0x0153,
    0x2013,0x2014,0x201C,0x201D,0x2018,0x2019,0x00F7,0x25CA, 0x00FF,0x0178,0x2044,0x20AC,0x2039,0x203A,0xFB01,0xFB02,
    0x2021,0x00B7,0x201A,0x201E,0x2030,0x00C2,0x00CA,0x00C1, 0x00CB,0x00C8,0x00CD,0x00CE,0x00CF,0x00CC,0x00D3,0x00D4,
    0xF8FF,0x00D2,0x00DA,0x00DB,0x00D9,0x0131,0x02C6,0x02DC, 0x00AF,0x02D8,0x02D9,0x02DA,0x00B8,0x02DD,0x02DB,0x02C7,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_round_trips_every_byte() {
        let enc = cp437();
        for b in 0u8..=255 {
            let decoded = enc.decode(&[b]);
            assert_eq!(decoded.chars().count(), 1, "byte {b:#x} -> one char");
            let reencoded = enc.encode(&decoded);
            assert_eq!(reencoded, vec![b], "byte {b:#x} did not round-trip");
        }
    }

    #[test]
    fn cp437_xywrite_markers_are_guillemets() {
        // days.26 opens with AE 46 43 AF = «FC» under CP437 (XyWrite function code).
        let enc = cp437();
        assert_eq!(enc.decode(&[0xAE, 0x46, 0x43, 0xAF]), "«FC»");
        // 0xCD is the double horizontal rule used in the file.
        assert_eq!(enc.decode(&[0xCD]), "═");
        // ASCII is identity.
        assert_eq!(enc.decode(b"Hello, world"), "Hello, world");
    }

    #[test]
    fn cp437_encode_unrepresentable_becomes_question_mark() {
        let enc = cp437();
        assert_eq!(enc.encode("€"), vec![b'?']); // U+20AC not in CP437
        assert_eq!(enc.encode("日本"), vec![b'?', b'?']);
        // A representable accented char encodes to its CP437 byte.
        assert_eq!(enc.encode("é"), vec![0x82]);
        assert_eq!(enc.encode("«»"), vec![0xAE, 0xAF]);
    }

    #[test]
    fn latin1_is_identity_and_round_trips() {
        let enc = latin1();
        assert_eq!(enc.decode(&[0xAE]), "®"); // Latin-1: 0xAE = ®
        for b in 0u8..=255 {
            assert_eq!(enc.encode(&enc.decode(&[b])), vec![b]);
        }
    }

    #[test]
    fn every_catalog_encoding_round_trips_every_byte() {
        for &(name, _label) in CATALOG {
            let enc = by_name(name).unwrap_or_else(|| panic!("CATALOG name {name:?} not in by_name"));
            assert_eq!(enc.name(), name, "{name}: .name() must equal its catalog name");
            if name == "utf-8" {
                continue; // multi-byte; the single-byte round-trip below doesn't apply
            }
            for b in 0u8..=255 {
                let decoded = enc.decode(&[b]);
                assert_eq!(decoded.chars().count(), 1, "{name}: byte {b:#x} -> one char");
                assert_eq!(enc.encode(&decoded), vec![b], "{name}: byte {b:#x} did not round-trip");
            }
        }
    }

    #[test]
    fn added_encodings_decode_known_anchors() {
        // A few signature characters per code page (sanity that the right table loaded).
        assert_eq!(cp866().decode(&[0x80]), "А"); // CP866 0x80 = Cyrillic capital A
        assert_eq!(windows1252().decode(&[0x80]), "€"); // 1252 0x80 = euro
        assert_eq!(windows1252().decode(&[0x93, 0x94]), "“”"); // smart double quotes
        assert_eq!(latin9().decode(&[0xA4]), "€"); // ISO-8859-15 0xA4 = euro (vs ¤ in Latin-1)
        assert_eq!(cp850().decode(&[0xB5]), "Á"); // CP850 Western letters in the box-draw band
        assert_eq!(macroman().decode(&[0xAA]), "™"); // Mac Roman 0xAA = trademark (0xA9 = ©)
        // An undefined CP1252 byte is preserved (PUA), not dropped.
        assert_eq!(windows1252().decode(&[0x81]).chars().count(), 1);
        assert_eq!(windows1252().encode(&windows1252().decode(&[0x81])), vec![0x81]);
    }

    #[test]
    fn cp866x_swaps_guillemets_and_relocates_two_letters() {
        let x = cp866x();
        // 0xAE/0xAF carry XyWrite's command guillemets (CP866 had Cyrillic о/п there).
        assert_eq!(x.decode(&[0xAE, 0xAF]), "«»");
        assert_eq!(cp866().decode(&[0xAE, 0xAF]), "оп"); // plain CP866 for contrast
        // The displaced letters live at 0xDD/0xDE (CP866 had ▌/▐ there).
        assert_eq!(x.decode(&[0xDD, 0xDE]), "оп");
        assert_eq!(cp866().decode(&[0xDD, 0xDE]), "▌▐");
        // Everything else still matches CP866 (e.g. 0x80 = А), and it encodes the markers back.
        assert_eq!(x.decode(&[0x80]), "А");
        assert_eq!(x.encode("«»"), vec![0xAE, 0xAF]);
        assert_eq!(x.encode("оп"), vec![0xDD, 0xDE]);
    }

    #[test]
    fn by_name_resolves_aliases() {
        assert_eq!(by_name("CP437").map(|e| e.name().to_string()), Some("cp437".into()));
        assert_eq!(by_name("437").map(|e| e.name().to_string()), Some("cp437".into()));
        assert_eq!(by_name("iso-8859-1").map(|e| e.name().to_string()), Some("latin1".into()));
        assert_eq!(by_name("UTF-8").map(|e| e.name().to_string()), Some("utf-8".into()));
        assert!(by_name("nonsense").is_none());
    }

    #[test]
    fn utf8_round_trips_and_steps_by_char() {
        let enc = utf8();
        assert!(!enc.is_single_byte());
        let s = "a—é日"; // a=1, em-dash=3, é=2, 日=3 bytes; 4 chars
        let bytes = enc.encode(s);
        assert_eq!(enc.decode(&bytes), s);
        assert_eq!(enc.char_count(&bytes), 4);
        assert_eq!(bytes.len(), 9);
        // Lead-byte lengths and continuation detection drive cursor stepping.
        assert_eq!(enc.lead_len(b'a'), 1);
        assert_eq!(enc.lead_len(bytes[1]), 3); // em-dash lead
        assert!(enc.is_continuation(bytes[2]));
        assert!(!enc.is_continuation(b'a'));
        // Every char is representable — encode never substitutes '?'.
        assert_eq!(enc.encode("€😀"), "€😀".as_bytes());
    }
}
