//! BE1 spike — byte storage structure for the 100–300 MB bar
//! (`byte-text-engine-plan.md` decision BE1, step 6).
//!
//! Re-spikes the candidates that would replace the flat `Vec<u8>` *behind the
//! stable `Buffer` API*: a **piece-table** (immutable original + append buffer +
//! piece list) and the **rope family**, represented here by a **chunked buffer**
//! (a flat list of 64 KB chunks — a true rope is its balanced-tree form, with the
//! same asymptotics but a smaller find constant). `Vec<u8>` is the baseline that
//! shows why we change at all.
//!
//! Each structure implements the operations the real `Buffer` needs: build,
//! insert, delete, and the line geometry (`byte_to_line`/`line_to_byte`). A
//! correctness oracle cross-checks every structure against `Vec<u8>` on a small
//! input before the big run, so the timings come from *correct* implementations.
//!
//! Spike caveats (stated so the comparison is honest):
//! - find uses a **linear scan** over chunks/pieces; production uses a balanced
//!   tree (O(log)). The element counts are small (≈4096 chunks for 256 MB; pieces
//!   ∝ edits), so latencies are µs-range and dominated by element count, *not*
//!   raw byte size — which is exactly the property the decision turns on.
//! - this is throwaway code: not wired to `Buffer`, not undo-aware.
//!
//! Usage:  cargo run -p xyscript-app --release --example byte_storage_spike

use std::time::{Duration, Instant};

use xyscript_app::ByteRope;

const CHUNK: usize = 64 * 1024;

// ---- the operations every candidate must support --------------------------

trait ByteStore {
    fn name(&self) -> &'static str;
    fn from_bytes(bytes: Vec<u8>) -> Self
    where
        Self: Sized;
    fn len(&self) -> usize;
    fn insert(&mut self, at: usize, data: &[u8]);
    fn delete(&mut self, at: usize, n: usize);
    fn byte_to_line(&self, pos: usize) -> usize;
    fn line_to_byte(&self, line: usize) -> usize;
    /// Approximate resident bytes of the structure (for the memory column).
    fn mem(&self) -> usize;
    /// Materialize the bytes (correctness checks only).
    fn to_vec(&self) -> Vec<u8>;
}

#[inline]
fn count_nl(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

// ---- baseline: flat Vec<u8> ------------------------------------------------

struct VecBuf(Vec<u8>);

impl ByteStore for VecBuf {
    fn name(&self) -> &'static str {
        "Vec<u8>"
    }
    fn from_bytes(bytes: Vec<u8>) -> Self {
        VecBuf(bytes)
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn insert(&mut self, at: usize, data: &[u8]) {
        self.0.splice(at..at, data.iter().copied());
    }
    fn delete(&mut self, at: usize, n: usize) {
        self.0.drain(at..at + n);
    }
    fn byte_to_line(&self, pos: usize) -> usize {
        count_nl(&self.0[..pos])
    }
    fn line_to_byte(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut seen = 0;
        for (i, &b) in self.0.iter().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == line {
                    return i + 1;
                }
            }
        }
        self.0.len()
    }
    fn mem(&self) -> usize {
        self.0.capacity()
    }
    fn to_vec(&self) -> Vec<u8> {
        self.0.clone()
    }
}

// ---- rope family: chunked buffer (flat list of bounded chunks) -------------

struct Chunk {
    data: Vec<u8>,
    nl: usize,
}

struct Chunked {
    chunks: Vec<Chunk>,
    len: usize,
}

impl Chunked {
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
        let i = self.chunks.len().saturating_sub(1);
        (i, self.chunks.get(i).map(|c| c.data.len()).unwrap_or(0))
    }
}

impl ByteStore for Chunked {
    fn name(&self) -> &'static str {
        "chunked(64K)"
    }
    fn from_bytes(bytes: Vec<u8>) -> Self {
        let mut chunks: Vec<Chunk> = bytes
            .chunks(CHUNK)
            .map(|c| Chunk { data: c.to_vec(), nl: count_nl(c) })
            .collect();
        if chunks.is_empty() {
            chunks.push(Chunk { data: Vec::new(), nl: 0 });
        }
        let len = bytes.len();
        Chunked { chunks, len }
    }
    fn len(&self) -> usize {
        self.len
    }
    fn insert(&mut self, at: usize, data: &[u8]) {
        let (i, off) = self.locate(at);
        let c = &mut self.chunks[i];
        c.data.splice(off..off, data.iter().copied());
        c.nl = count_nl(&c.data);
        // Split if the chunk grew too large (keeps edits bounded).
        if c.data.len() > 2 * CHUNK {
            let tail = c.data.split_off(CHUNK);
            c.nl = count_nl(&c.data);
            let nl = count_nl(&tail);
            self.chunks.insert(i + 1, Chunk { data: tail, nl });
        }
        self.len += data.len();
    }
    fn delete(&mut self, at: usize, n: usize) {
        let mut remaining = n;
        while remaining > 0 {
            let (i, off) = self.locate(at);
            let c = &mut self.chunks[i];
            let take = remaining.min(c.data.len() - off);
            if take == 0 {
                break;
            }
            c.data.drain(off..off + take);
            c.nl = count_nl(&c.data);
            remaining -= take;
            if c.data.is_empty() && self.chunks.len() > 1 {
                self.chunks.remove(i);
            }
        }
        self.len -= n;
    }
    fn byte_to_line(&self, pos: usize) -> usize {
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
    fn line_to_byte(&self, line: usize) -> usize {
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
    fn mem(&self) -> usize {
        self.chunks.iter().map(|c| c.data.capacity()).sum::<usize>()
            + self.chunks.capacity() * std::mem::size_of::<Chunk>()
    }
    fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len);
        for c in &self.chunks {
            v.extend_from_slice(&c.data);
        }
        v
    }
}

// ---- piece-table: immutable original + append buffer + piece list ----------

#[derive(Clone, Copy, PartialEq)]
enum Src {
    Orig,
    Add,
}

#[derive(Clone, Copy)]
struct Piece {
    src: Src,
    start: usize,
    len: usize,
    nl: usize,
}

struct PieceTable {
    orig: Vec<u8>,
    add: Vec<u8>,
    pieces: Vec<Piece>,
    len: usize,
}

impl PieceTable {
    fn buf(&self, src: Src) -> &[u8] {
        match src {
            Src::Orig => &self.orig,
            Src::Add => &self.add,
        }
    }
    /// A sub-range of an existing piece, with its newline count recomputed.
    fn sub(&self, p: &Piece, delta: usize, len: usize) -> Piece {
        let start = p.start + delta;
        let nl = count_nl(&self.buf(p.src)[start..start + len]);
        Piece { src: p.src, start, len, nl }
    }
}

impl ByteStore for PieceTable {
    fn name(&self) -> &'static str {
        "piece-table"
    }
    fn from_bytes(bytes: Vec<u8>) -> Self {
        let nl = count_nl(&bytes);
        let len = bytes.len();
        let pieces = if len == 0 {
            Vec::new()
        } else {
            vec![Piece { src: Src::Orig, start: 0, len, nl }]
        };
        PieceTable { orig: bytes, add: Vec::new(), pieces, len }
    }
    fn len(&self) -> usize {
        self.len
    }
    fn insert(&mut self, at: usize, data: &[u8]) {
        let add_start = self.add.len();
        self.add.extend_from_slice(data);
        let np = Piece { src: Src::Add, start: add_start, len: data.len(), nl: count_nl(data) };
        if self.pieces.is_empty() {
            self.pieces.push(np);
            self.len += data.len();
            return;
        }
        let mut cum = 0;
        let mut idx = self.pieces.len();
        let mut off = 0;
        for (i, p) in self.pieces.iter().enumerate() {
            if at <= cum + p.len {
                idx = i;
                off = at - cum;
                break;
            }
            cum += p.len;
        }
        if idx == self.pieces.len() {
            self.pieces.push(np);
        } else if off == 0 {
            self.pieces.insert(idx, np);
        } else if off == self.pieces[idx].len {
            self.pieces.insert(idx + 1, np);
        } else {
            let p = self.pieces[idx];
            let left = self.sub(&p, 0, off);
            let right = self.sub(&p, off, p.len - off);
            self.pieces.splice(idx..idx + 1, [left, np, right]);
        }
        self.len += data.len();
    }
    fn delete(&mut self, at: usize, n: usize) {
        if n == 0 {
            return;
        }
        let end = at + n;
        let mut out: Vec<Piece> = Vec::with_capacity(self.pieces.len() + 1);
        let mut cum = 0;
        for p in &self.pieces {
            let p_start = cum;
            let p_end = cum + p.len;
            cum = p_end;
            if p_end <= at || p_start >= end {
                out.push(*p); // fully outside the deletion
            } else {
                if p_start < at {
                    out.push(self.sub(p, 0, at - p_start)); // head survives
                }
                if p_end > end {
                    let skip = end - p_start;
                    out.push(self.sub(p, skip, p.len - skip)); // tail survives
                }
            }
        }
        self.pieces = out;
        self.len -= n;
    }
    fn byte_to_line(&self, pos: usize) -> usize {
        let mut cum = 0;
        let mut lines = 0;
        for p in &self.pieces {
            if pos <= cum + p.len {
                let off = pos - cum;
                return lines + count_nl(&self.buf(p.src)[p.start..p.start + off]);
            }
            lines += p.nl;
            cum += p.len;
        }
        lines
    }
    fn line_to_byte(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut cum = 0;
        let mut seen = 0;
        for p in &self.pieces {
            if seen + p.nl >= line {
                let need = line - seen;
                let b = &self.buf(p.src)[p.start..p.start + p.len];
                let mut count = 0;
                for (i, &by) in b.iter().enumerate() {
                    if by == b'\n' {
                        count += 1;
                        if count == need {
                            return cum + i + 1;
                        }
                    }
                }
            }
            seen += p.nl;
            cum += p.len;
        }
        self.len
    }
    fn mem(&self) -> usize {
        self.orig.capacity()
            + self.add.capacity()
            + self.pieces.capacity() * std::mem::size_of::<Piece>()
    }
    fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len);
        for p in &self.pieces {
            v.extend_from_slice(&self.buf(p.src)[p.start..p.start + p.len]);
        }
        v
    }
}

// ---- the PRODUCTION rope (xyscript_app::ByteRope) --------------------------

struct LibRope(ByteRope);

impl ByteStore for LibRope {
    fn name(&self) -> &'static str {
        "ByteRope (lib)"
    }
    fn from_bytes(bytes: Vec<u8>) -> Self {
        LibRope(ByteRope::from_bytes(bytes))
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn insert(&mut self, at: usize, data: &[u8]) {
        self.0.insert(at, data);
    }
    fn delete(&mut self, at: usize, n: usize) {
        self.0.delete(at, n);
    }
    fn byte_to_line(&self, pos: usize) -> usize {
        self.0.byte_to_line(pos)
    }
    fn line_to_byte(&self, line: usize) -> usize {
        self.0.line_to_byte(line)
    }
    fn mem(&self) -> usize {
        self.0.len() // internal chunk capacity not exposed; ~len + small overhead
    }
    fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

// ---- deterministic RNG (no rand crate; reproducible) -----------------------

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

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

// ---- correctness: cross-check each structure against Vec<u8> ---------------

fn check<T: ByteStore>(base: &[u8]) {
    let mut oracle = VecBuf::from_bytes(base.to_vec());
    let mut s = T::from_bytes(base.to_vec());
    let mut rng = Rng(12345);
    for step in 0..4000 {
        if rng.below(2) == 0 || oracle.len() < 8 {
            let at = rng.below(oracle.len() + 1);
            oracle.insert(at, b"Q\nR");
            s.insert(at, b"Q\nR");
        } else {
            let at = rng.below(oracle.len() - 4);
            oracle.delete(at, 4);
            s.delete(at, 4);
        }
        if step % 800 == 0 {
            assert_eq!(oracle.to_vec(), s.to_vec(), "{}: bytes mismatch @ step {step}", s.name());
        }
    }
    assert_eq!(oracle.to_vec(), s.to_vec(), "{}: final bytes mismatch", s.name());
    let v = oracle.to_vec();
    for _ in 0..1000 {
        let p = rng.below(v.len() + 1);
        assert_eq!(oracle.byte_to_line(p), s.byte_to_line(p), "{}: byte_to_line({p})", s.name());
    }
    let total = oracle.byte_to_line(v.len());
    for _ in 0..1000 {
        let l = rng.below(total + 1);
        assert_eq!(oracle.line_to_byte(l), s.line_to_byte(l), "{}: line_to_byte({l})", s.name());
    }
    println!("  {:<14} correctness OK", s.name());
}

// ---- benchmark -------------------------------------------------------------

struct Timings {
    name: &'static str,
    build: Duration,
    insert: Duration,
    delete: Duration,
    b2l: Duration,
    l2b: Duration,
    mem_mb: f64,
}

fn bench<T: ByteStore>(data: &[u8], k_edit: usize, k_nav: usize) -> Timings {
    let t = Instant::now();
    let mut s = T::from_bytes(data.to_vec());
    let build = t.elapsed();

    let mut rng = Rng(SEED);
    let ins = b"X\nY";

    let t = Instant::now();
    for _ in 0..k_edit {
        let at = rng.below(s.len() + 1);
        s.insert(at, ins);
    }
    let insert = t.elapsed() / k_edit as u32;

    let t = Instant::now();
    for _ in 0..k_edit {
        if s.len() < 8 {
            break;
        }
        let at = rng.below(s.len() - 4);
        s.delete(at, 4);
    }
    let delete = t.elapsed() / k_edit as u32;

    let mut acc = 0usize;
    let t = Instant::now();
    for _ in 0..k_nav {
        let p = rng.below(s.len() + 1);
        acc = acc.wrapping_add(s.byte_to_line(p));
    }
    let b2l = t.elapsed() / k_nav as u32;

    let total = s.byte_to_line(s.len());
    let t = Instant::now();
    for _ in 0..k_nav {
        let l = rng.below(total + 1);
        acc = acc.wrapping_add(s.line_to_byte(l));
    }
    let l2b = t.elapsed() / k_nav as u32;
    std::hint::black_box(acc);

    Timings {
        name: s.name(),
        build,
        insert,
        delete,
        b2l,
        l2b,
        mem_mb: s.mem() as f64 / (1024.0 * 1024.0),
    }
}

fn row(t: &Timings) {
    println!(
        "  {:<14} {:>9.0?} {:>11.1?} {:>11.1?} {:>11.1?} {:>11.1?} {:>9.1}",
        t.name, t.build, t.insert, t.delete, t.b2l, t.l2b, t.mem_mb
    );
}

fn main() {
    println!("== correctness oracle (small input) ==");
    let base: Vec<u8> = (0..3000u32)
        .map(|i| if i % 13 == 0 { b'\n' } else { b'a' + (i % 26) as u8 })
        .collect();
    check::<VecBuf>(&base);
    check::<Chunked>(&base);
    check::<PieceTable>(&base);
    check::<LibRope>(&base);

    // ~256 MB of short lines (newline-dense; stresses line geometry).
    let line = b"The quick brown fox jumps over 0123\n"; // 36 bytes
    let target = 256 * 1024 * 1024;
    let mut data = Vec::with_capacity(target + line.len());
    while data.len() < target {
        data.extend_from_slice(line);
    }
    println!(
        "\n== benchmark: {:.0} MB, {} lines ==",
        data.len() as f64 / (1024.0 * 1024.0),
        count_nl(&data)
    );
    println!("  (insert/delete/b2l/l2b are PER-OP averages; Vec uses fewer ops, same latency basis)\n");
    println!(
        "  {:<14} {:>9} {:>11} {:>11} {:>11} {:>11} {:>9}",
        "structure", "build", "insert", "delete", "byte→line", "line→byte", "mem MB"
    );

    // Vec gets fewer edit/nav ops (its O(n) ops are slow); latency is per-op so
    // it stays comparable.
    row(&bench::<VecBuf>(&data, 120, 120));
    row(&bench::<Chunked>(&data, 4000, 20000));
    row(&bench::<PieceTable>(&data, 4000, 20000));
    row(&bench::<LibRope>(&data, 4000, 20000));

    println!("\nnote: find is linear-scan (spike); a tree-backed version lowers the");
    println!("constant for both piece-table and rope but not the asymptotics shown.");
}
