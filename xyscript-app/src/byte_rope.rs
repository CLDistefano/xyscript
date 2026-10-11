//! A hand-rolled **byte rope** (`byte-text-engine-plan.md` step 6, decision BE1).
//!
//! Replaces the flat `Vec<u8>` document storage so editing 100–300 MB files has
//! no visible lag: a flat `Vec<u8>` shifts O(n) bytes on every insert/delete
//! (milliseconds at 256 MB — see `examples/byte_storage_spike.rs`), whereas this
//! keeps edits and line navigation in the microsecond range.
//!
//! ## Shape
//!
//! A **chunked rope**: an ordered list of bounded byte chunks (default 64 KiB),
//! each caching its newline count so line geometry is O(chunks) instead of O(n).
//! Edits touch (and at most split/merge) one chunk, so cost scales with chunk
//! *size*, not document size. This flat-list form is the rope family's simplest
//! member; the chunk count is small (≈4 K chunks for 256 MB), so the linear
//! `locate` scan is microseconds. If multi-GB documents ever demand it, a
//! balanced tree of the same chunks slots in behind this exact API — the
//! `Buffer` above never sees the difference.
//!
//! All offsets are **byte offsets**; the rope is encoding-agnostic (it stores
//! bytes, the truth), which is what lets the same structure serve new-UTF-8
//! authoring and legacy single-byte files alike.

/// Default chunk target. Chunks split above `2×` this and merge below `½` this,
/// so the count stays ≈ `len / chunk_size`.
const DEFAULT_CHUNK: usize = 64 * 1024;

#[inline]
fn count_nl(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

/// Count whitespace-delimited words in a byte slice — a "word" is any run of
/// non-whitespace bytes (whitespace = ASCII space/tab/newline/return/form-feed, whose
/// byte values are shared by UTF-8 and the single-byte code pages, so this is
/// encoding-agnostic). Matches QN's `WORDCOUNT` "any non-empty whitespace run".
pub fn count_words(bytes: &[u8]) -> usize {
    let mut count = 0;
    let mut prev_ws = true; // start-of-text behaves as if preceded by whitespace
    for &b in bytes {
        let ws = b.is_ascii_whitespace();
        if !ws && prev_ws {
            count += 1;
        }
        prev_ws = ws;
    }
    count
}

#[derive(Debug, Clone)]
struct Chunk {
    data: Vec<u8>,
    /// Cached count of `\n` in `data` (kept in sync on every mutation).
    nl: usize,
}

impl Chunk {
    fn new(data: Vec<u8>) -> Chunk {
        let nl = count_nl(&data);
        Chunk { data, nl }
    }
    fn refresh(&mut self) {
        self.nl = count_nl(&self.data);
    }
}

/// A byte rope: the document's bytes, stored as bounded chunks.
#[derive(Debug, Clone)]
pub struct ByteRope {
    chunks: Vec<Chunk>, // always non-empty (one empty chunk represents "")
    len: usize,
    chunk_size: usize,
    /// Monotonic edit counter, bumped on every real insert/delete. Lets callers
    /// (e.g. a render cache) detect content changes in O(1) without comparing bytes.
    rev: u64,
}

impl Default for ByteRope {
    fn default() -> ByteRope {
        ByteRope { chunks: vec![Chunk::new(Vec::new())], len: 0, chunk_size: DEFAULT_CHUNK, rev: 0 }
    }
}

impl ByteRope {
    pub fn new() -> ByteRope {
        ByteRope::default()
    }

    /// Build from raw bytes (verbatim — the byte-exact load path).
    pub fn from_bytes(bytes: Vec<u8>) -> ByteRope {
        ByteRope::chunked(bytes, DEFAULT_CHUNK)
    }

    fn chunked(bytes: Vec<u8>, chunk_size: usize) -> ByteRope {
        let len = bytes.len();
        let mut chunks: Vec<Chunk> =
            bytes.chunks(chunk_size).map(|c| Chunk::new(c.to_vec())).collect();
        if chunks.is_empty() {
            chunks.push(Chunk::new(Vec::new()));
        }
        ByteRope { chunks, len, chunk_size, rev: 0 }
    }

    /// The monotonic edit counter (bumped on each real insert/delete).
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Find the chunk holding byte `at` and the offset within it. `at == len`
    /// returns the last chunk at its end.
    fn locate(&self, at: usize) -> (usize, usize) {
        let mut cum = 0;
        for (i, c) in self.chunks.iter().enumerate() {
            if at <= cum + c.data.len() {
                return (i, at - cum);
            }
            cum += c.data.len();
        }
        let i = self.chunks.len() - 1;
        (i, self.chunks[i].data.len())
    }

    /// The single byte at offset `i` (`i < len`).
    pub fn byte(&self, i: usize) -> u8 {
        let (ci, off) = self.locate(i);
        // At a chunk boundary `locate` reports the end of chunk `ci` (off ==
        // len); the byte actually lives at the start of the next chunk.
        if off < self.chunks[ci].data.len() {
            self.chunks[ci].data[off]
        } else {
            self.chunks[ci + 1].data[0]
        }
    }

    /// Insert `data` at byte offset `at`.
    pub fn insert(&mut self, at: usize, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.rev = self.rev.wrapping_add(1);
        let (i, off) = self.locate(at);
        let c = &mut self.chunks[i];
        c.data.splice(off..off, data.iter().copied());
        c.refresh();
        self.len += data.len();
        self.split_if_large(i);
    }

    /// Delete `n` bytes starting at offset `at`.
    pub fn delete(&mut self, at: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.rev = self.rev.wrapping_add(1);
        // Locate once, then walk forward: at a chunk boundary `locate` reports the
        // end of the current chunk, so re-locating the same offset would stick —
        // advancing through chunks instead keeps a multi-chunk delete moving.
        let (mut i, mut off) = self.locate(at);
        let mut remaining = n;
        while remaining > 0 && i < self.chunks.len() {
            let avail = self.chunks[i].data.len() - off;
            let take = remaining.min(avail);
            if take > 0 {
                self.chunks[i].data.drain(off..off + take);
                self.chunks[i].refresh();
                remaining -= take;
            }
            if self.chunks[i].data.is_empty() && self.chunks.len() > 1 {
                self.chunks.remove(i); // next chunk shifts into slot i
            } else {
                i += 1;
            }
            off = 0;
        }
        self.len -= n - remaining;
        self.coalesce();
    }

    /// Repartition chunk `i` into pieces of `chunk_size` (last between `chunk_size` and
    /// `2×chunk_size`) when it has grown past `2×chunk_size`. Done in a **single pass** —
    /// the old `split_off`-in-a-loop recopied the shrinking tail on every iteration, so a
    /// large single insert (e.g. a multi-MB paste) was `O(n²)`; this copies each byte once.
    fn split_if_large(&mut self, i: usize) {
        let cs = self.chunk_size;
        if self.chunks[i].data.len() <= cs * 2 {
            return;
        }
        let data = std::mem::take(&mut self.chunks[i].data);
        let mut pieces = Vec::with_capacity(data.len() / cs + 1);
        let mut start = 0;
        while data.len() - start > cs * 2 {
            pieces.push(Chunk::new(data[start..start + cs].to_vec()));
            start += cs;
        }
        pieces.push(Chunk::new(data[start..].to_vec())); // remainder ∈ (cs, 2·cs]
        self.chunks.splice(i..=i, pieces);
    }

    /// Merge adjacent small chunks so deletes don't leave a long tail of tiny
    /// chunks (which would slow `locate`). Runs only after a delete.
    fn coalesce(&mut self) {
        let small = self.chunk_size / 2;
        let max = self.chunk_size * 2;
        let mut i = 0;
        while i + 1 < self.chunks.len() {
            let combined = self.chunks[i].data.len() + self.chunks[i + 1].data.len();
            if self.chunks[i].data.len() < small && combined <= max {
                let next = self.chunks.remove(i + 1);
                self.chunks[i].data.extend_from_slice(&next.data);
                self.chunks[i].refresh();
                // stay at i to keep merging into it
            } else {
                i += 1;
            }
        }
    }

    /// Copy the byte range `[start, end)` into a fresh `Vec`.
    pub fn slice_to_vec(&self, start: usize, end: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(end - start);
        let mut cum = 0;
        for c in &self.chunks {
            let cstart = cum;
            let cend = cum + c.data.len();
            cum = cend;
            if cend <= start || cstart >= end {
                continue;
            }
            let a = start.max(cstart) - cstart;
            let b = end.min(cend) - cstart;
            out.extend_from_slice(&c.data[a..b]);
        }
        out
    }

    /// Sum a per-byte table over the byte range `[start, end)`, streamed across chunks with no
    /// allocation. For a SINGLE-byte encoding, feeding a byte→decoded-UTF-8-length table gives the
    /// decoded length of the range without decoding it into a `String` — the fast path
    /// [`Buffer::build_dec_index`](crate::text::Buffer) uses on big non-UTF-8 files.
    pub fn sum_table_range(&self, start: usize, end: usize, tbl: &[usize; 256]) -> usize {
        let mut sum = 0usize;
        let mut cum = 0usize;
        for c in &self.chunks {
            let cstart = cum;
            let cend = cum + c.data.len();
            cum = cend;
            if cend <= start || cstart >= end {
                continue;
            }
            let a = start.max(cstart) - cstart;
            let b = end.min(cend) - cstart;
            for &byte in &c.data[a..b] {
                sum += tbl[byte as usize];
            }
        }
        sum
    }

    /// Materialize all bytes (the byte-exact save path / whole-document decode).
    pub fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len);
        for c in &self.chunks {
            v.extend_from_slice(&c.data);
        }
        v
    }

    /// Total line count: number of `\n` plus one.
    pub fn line_count(&self) -> usize {
        self.chunks.iter().map(|c| c.nl).sum::<usize>() + 1
    }

    /// Whole-document word count, streamed across chunks (no full-document copy, so it
    /// stays cheap even at hundreds of MB). Same semantics as [`count_words`], carrying
    /// the whitespace state across chunk boundaries so a word split by a boundary counts
    /// once.
    pub fn word_count(&self) -> usize {
        let mut count = 0;
        let mut prev_ws = true;
        for c in &self.chunks {
            for &b in &c.data {
                let ws = b.is_ascii_whitespace();
                if !ws && prev_ws {
                    count += 1;
                }
                prev_ws = ws;
            }
        }
        count
    }

    /// Whole-document character count. For a single-byte view it's just the byte length;
    /// for UTF-8 it's the number of non-continuation bytes (lead + ASCII), which is
    /// additive across chunks even when a multi-byte char straddles a boundary.
    pub fn char_count(&self, single_byte: bool) -> usize {
        if single_byte {
            self.len
        } else {
            self.chunks
                .iter()
                .map(|c| c.data.iter().filter(|&&b| (b & 0xC0) != 0x80).count())
                .sum()
        }
    }

    /// 0-based line index containing byte offset `pos`.
    pub fn byte_to_line(&self, pos: usize) -> usize {
        let pos = pos.min(self.len);
        let mut cum = 0;
        let mut lines = 0;
        for c in &self.chunks {
            if pos <= cum + c.data.len() {
                return lines + count_nl(&c.data[..pos - cum]);
            }
            lines += c.nl;
            cum += c.data.len();
        }
        lines
    }

    /// Byte offset of the start of (0-based) `line`. A line index at/past the end
    /// returns `len`.
    pub fn line_to_byte(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut cum = 0;
        let mut seen = 0;
        for c in &self.chunks {
            if seen + c.nl >= line {
                let need = line - seen;
                let mut count = 0;
                for (i, &b) in c.data.iter().enumerate() {
                    if b == b'\n' {
                        count += 1;
                        if count == need {
                            return cum + i + 1;
                        }
                    }
                }
            }
            seen += c.nl;
            cum += c.data.len();
        }
        self.len
    }

    #[cfg(test)]
    fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_materialize_round_trips() {
        let b = b"hello\nworld\n".to_vec();
        let r = ByteRope::from_bytes(b.clone());
        assert_eq!(r.len(), b.len());
        assert!(!r.is_empty());
        assert_eq!(r.to_vec(), b);
        assert!(ByteRope::new().is_empty());
        assert_eq!(ByteRope::new().to_vec(), Vec::<u8>::new());
    }

    #[test]
    fn insert_delete_at_boundaries() {
        let mut r = ByteRope::new();
        r.insert(0, b"ac");
        r.insert(1, b"b"); // middle -> "abc"
        assert_eq!(r.to_vec(), b"abc");
        r.insert(3, b"!"); // end
        assert_eq!(r.to_vec(), b"abc!");
        r.delete(0, 1); // front
        assert_eq!(r.to_vec(), b"bc!");
        r.delete(2, 1); // end
        assert_eq!(r.to_vec(), b"bc");
        r.delete(0, 2); // all
        assert_eq!(r.to_vec(), b"");
        assert!(r.is_empty());
    }

    #[test]
    fn line_geometry() {
        let r = ByteRope::from_bytes(b"ab\ncde\nf".to_vec());
        assert_eq!(r.line_count(), 3);
        assert_eq!(r.byte_to_line(0), 0);
        assert_eq!(r.byte_to_line(3), 1); // first byte of "cde"
        assert_eq!(r.byte_to_line(8), 2); // end
        assert_eq!(r.line_to_byte(0), 0);
        assert_eq!(r.line_to_byte(1), 3);
        assert_eq!(r.line_to_byte(2), 7);
        assert_eq!(r.line_to_byte(3), 8); // past end -> len
        assert_eq!(r.byte(2), b'\n');
    }

    #[test]
    fn byte_reads_across_chunk_boundaries() {
        // chunk_size 4 puts boundaries at 4/8/12/16; byte() must cross them.
        let r = ByteRope::chunked((0..20u8).collect(), 4);
        assert!(r.chunk_count() >= 5);
        for i in 0..20 {
            assert_eq!(r.byte(i), i as u8, "byte({i})");
        }
    }

    #[test]
    fn slice_across_chunks() {
        // chunk_size 4 forces multiple chunks for this content.
        let r = ByteRope::chunked(b"0123456789".to_vec(), 4);
        assert!(r.chunk_count() >= 2);
        assert_eq!(r.slice_to_vec(2, 8), b"234567");
        assert_eq!(r.slice_to_vec(0, 10), b"0123456789");
        assert_eq!(r.slice_to_vec(5, 5), b"");
    }

    #[test]
    fn large_insert_splits_into_bounded_chunks() {
        let mut r = ByteRope::chunked(Vec::new(), 8);
        let big = vec![b'x'; 100];
        r.insert(0, &big);
        assert_eq!(r.len(), 100);
        assert_eq!(r.to_vec(), big);
        assert!(r.chunk_count() > 1, "a 100-byte insert at chunk_size 8 must split");
        for c in &r.chunks {
            assert!(c.data.len() <= 16, "no chunk exceeds 2×chunk_size");
        }
    }

    #[test]
    fn large_single_insert_is_bounded_and_correct() {
        // One big insert (think: pasting a large clipboard). Must stay O(n) and produce
        // correctly-bounded chunks — under the old repeated-`split_off` loop this was
        // O(n²) and would crawl; here each byte is copied once.
        let mut r = ByteRope::chunked(Vec::new(), 64);
        let big: Vec<u8> = (0..256 * 1024u32)
            .map(|i| if i % 80 == 0 { b'\n' } else { b'a' + (i % 26) as u8 })
            .collect();
        r.insert(0, &big);
        assert_eq!(r.len(), big.len());
        assert_eq!(r.to_vec(), big);
        for c in &r.chunks {
            assert!(c.data.len() <= 128, "chunk exceeds 2×chunk_size");
            assert!(!c.data.is_empty(), "empty chunk");
        }
        // line geometry matches a straight scan
        let nl = count_nl(&big);
        assert_eq!(r.line_count(), nl + 1);
        assert_eq!(r.line_to_byte(nl), big.iter().rposition(|&b| b == b'\n').unwrap() + 1);
        // a mid-document insert into the now-large rope still round-trips
        r.insert(100_000, b"MARK\n");
        let mut oracle = big.clone();
        oracle.splice(100_000..100_000, b"MARK\n".iter().copied());
        assert_eq!(r.to_vec(), oracle);
    }

    #[test]
    fn deletes_coalesce_small_chunks() {
        let mut r = ByteRope::chunked((0..200u32).map(|i| b'a' + (i % 26) as u8).collect(), 8);
        let before = r.chunk_count();
        // Delete most of the content; chunk count must not stay inflated.
        r.delete(10, 180);
        assert_eq!(r.len(), 20);
        assert!(r.chunk_count() <= before, "coalesce should keep chunks bounded");
        // Bytes are still correct.
        let oracle: Vec<u8> = {
            let mut v: Vec<u8> = (0..200u32).map(|i| b'a' + (i % 26) as u8).collect();
            v.drain(10..190);
            v
        };
        assert_eq!(r.to_vec(), oracle);
    }

    // ---- oracle fuzz: cross-check every op against a Vec<u8> ---------------

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    #[test]
    fn fuzz_matches_vec_oracle_across_chunk_boundaries() {
        // Tiny chunk_size so small ops constantly hit split/merge/cross-chunk.
        let base: Vec<u8> =
            (0..300u32).map(|i| if i % 11 == 0 { b'\n' } else { b'a' + (i % 26) as u8 }).collect();
        let mut oracle = base.clone();
        let mut rope = ByteRope::chunked(base, 8);
        let mut rng = Rng(0xDEAD_BEEF_CAFE_1234);

        for step in 0..6000 {
            if rng.below(2) == 0 || oracle.len() < 8 {
                let at = rng.below(oracle.len() + 1);
                let ins: &[u8] = if rng.below(3) == 0 { b"Q\nR" } else { b"zz" };
                oracle.splice(at..at, ins.iter().copied());
                rope.insert(at, ins);
            } else {
                let max = oracle.len().clamp(1, 6);
                let n = 1 + rng.below(max);
                let at = rng.below(oracle.len() - n + 1);
                oracle.drain(at..at + n);
                rope.delete(at, n);
            }
            if step % 500 == 0 {
                assert_eq!(rope.to_vec(), oracle, "bytes diverged at step {step}");
            }
        }
        assert_eq!(rope.to_vec(), oracle, "final bytes diverged");

        // Line geometry must match a straight Vec scan.
        let nl_total = count_nl(&oracle);
        assert_eq!(rope.line_count(), nl_total + 1);
        for _ in 0..1000 {
            let p = rng.below(oracle.len() + 1);
            assert_eq!(rope.byte_to_line(p), count_nl(&oracle[..p]), "byte_to_line({p})");
        }
        for line in 0..=nl_total + 1 {
            let expect = if line == 0 {
                0
            } else {
                let mut seen = 0;
                let mut found = oracle.len();
                for (i, &b) in oracle.iter().enumerate() {
                    if b == b'\n' {
                        seen += 1;
                        if seen == line {
                            found = i + 1;
                            break;
                        }
                    }
                }
                found
            };
            assert_eq!(rope.line_to_byte(line), expect, "line_to_byte({line})");
        }
    }
}
