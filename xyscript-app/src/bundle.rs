//! A tiny length-prefixed container that travels a document together with its `.xymeta` sidecar as a
//! single file, for `BUNDLE` / `UNBUNDLE` (and transparent `OPEN` of a `.xyb`). No dependency — the
//! format is a magic line followed by length-prefixed records, so it is binary-safe: the document's
//! bytes are never scanned for a delimiter, only copied through by their declared length.
//!
//! ```text
//! XYBUNDLE 1\n
//! file <doc-len> <name>\n
//! <doc-len raw bytes>\n
//! meta <meta-len>\n
//! <meta-len raw bytes>\n
//! ```
//!
//! The sidecar is pure DATA (never executed) and already self-validates — it carries a length+hash
//! guard tying it to the document bytes — so a bundle needs no integrity scheme of its own: on
//! unbundle the normal open path re-checks the guard and fail-closed-ignores stale styling.

const MAGIC: &str = "XYBUNDLE 1";

/// A document and its metadata, unpacked from a bundle. `meta` is empty when the document had none.
pub struct Bundle {
    /// The document's own file name (no directory), e.g. `report.typ`.
    pub name: String,
    /// The document's raw bytes, exactly as they were on disk / would be saved.
    pub doc: Vec<u8>,
    /// The `.xymeta` sidecar text (UTF-8 bytes), or empty when there is no metadata.
    pub meta: Vec<u8>,
}

/// Does `bytes` look like a bundle? (Cheap magic-line sniff for the `OPEN` fast path.)
pub fn is_bundle(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC.as_bytes())
}

/// Pack a document and its sidecar text into one bundle. `name` is the document's bare file name;
/// `meta` is the sidecar bytes (pass an empty slice when there is no metadata).
pub fn pack(name: &str, doc: &[u8], meta: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(doc.len() + meta.len() + name.len() + 64);
    out.extend_from_slice(MAGIC.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(format!("file {} {name}\n", doc.len()).as_bytes());
    out.extend_from_slice(doc);
    out.push(b'\n');
    out.extend_from_slice(format!("meta {}\n", meta.len()).as_bytes());
    out.extend_from_slice(meta);
    out.push(b'\n');
    out
}

/// Read a line (up to and including the next `\n`) as UTF-8 starting at `pos`, returning the line
/// (trailing `\r`/`\n` trimmed) and the offset just past the `\n`. `None` if there is no newline.
fn read_line(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let nl = bytes[pos..].iter().position(|&b| b == b'\n')? + pos;
    let line = String::from_utf8_lossy(&bytes[pos..nl]).trim_end_matches('\r').to_string();
    Some((line, nl + 1))
}

/// Unpack a bundle back into its document and metadata. Returns a short error string when `bytes`
/// isn't a well-formed bundle (truncated, bad length, missing the `file` record).
pub fn unpack(bytes: &[u8]) -> Result<Bundle, String> {
    let (magic, mut pos) = read_line(bytes, 0).ok_or("not a bundle (no header)")?;
    if magic.split_whitespace().next() != Some(MAGIC.split_whitespace().next().unwrap()) {
        return Err("not a bundle (bad magic)".into());
    }
    let mut name: Option<String> = None;
    let mut doc: Option<Vec<u8>> = None;
    let mut meta: Vec<u8> = Vec::new();
    while pos < bytes.len() {
        let (header, after) = read_line(bytes, pos).ok_or("truncated bundle (no record header)")?;
        let mut it = header.splitn(3, ' ');
        let tag = it.next().unwrap_or("");
        let len: usize = it
            .next()
            .ok_or("malformed record header")?
            .parse()
            .map_err(|_| "malformed record length")?;
        let end = after.checked_add(len).filter(|&e| e <= bytes.len()).ok_or("record length past end of bundle")?;
        let payload = bytes[after..end].to_vec();
        // Skip the single `\n` terminator after the payload, if present.
        pos = if bytes.get(end) == Some(&b'\n') { end + 1 } else { end };
        match tag {
            "file" => {
                name = Some(it.next().unwrap_or("").to_string());
                doc = Some(payload);
            }
            "meta" => meta = payload,
            _ => {} // unknown record: skip (forward-compatible)
        }
    }
    let name = name.filter(|n| !n.is_empty()).ok_or("bundle has no file name")?;
    let doc = doc.ok_or("bundle has no document")?;
    Ok(Bundle { name, doc, meta })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_name_doc_and_meta() {
        let b = pack("report.typ", b"hello\nworld\n", b"xymeta 1\nlen 12\n");
        assert!(is_bundle(&b));
        let u = unpack(&b).unwrap();
        assert_eq!(u.name, "report.typ");
        assert_eq!(u.doc, b"hello\nworld\n");
        assert_eq!(u.meta, b"xymeta 1\nlen 12\n");
    }

    #[test]
    fn is_binary_safe_even_when_the_document_contains_the_magic_and_record_lines() {
        // A document whose own bytes spell out header-looking lines must still round-trip exactly,
        // because unpack copies by declared length and never scans the body.
        let nasty = b"XYBUNDLE 1\nfile 999 fake\nmeta 7\n\x00\x01\xffnot utf8";
        let b = pack("x.bin", nasty, b"");
        let u = unpack(&b).unwrap();
        assert_eq!(u.name, "x.bin");
        assert_eq!(u.doc, nasty);
        assert!(u.meta.is_empty());
    }

    #[test]
    fn empty_meta_is_allowed() {
        let b = pack("notes.md", b"# hi", b"");
        let u = unpack(&b).unwrap();
        assert!(u.meta.is_empty());
        assert_eq!(u.doc, b"# hi");
    }

    #[test]
    fn names_with_spaces_survive() {
        let b = pack("my notes.typ", b"x", b"");
        assert_eq!(unpack(&b).unwrap().name, "my notes.typ");
    }

    #[test]
    fn rejects_non_bundles_and_truncation() {
        assert!(unpack(b"just a text file\n").is_err());
        assert!(!is_bundle(b"just a text file\n"));
        // A declared length longer than the remaining bytes is rejected, not panicked on.
        let mut b = pack("a.txt", b"abcdef", b"");
        b.truncate(b.len() / 2);
        assert!(unpack(&b).is_err());
    }
}
