//! `ropey` validation spike for Stage 3 (decisions D1 + D2).
//!
//! Confirms, on real hardware, that the chosen text engine clears the bar set by
//! the 100–300MB-file requirement, and that the inverse-op undo model round-trips
//! correctly — *before* the engine is built into the editor.
//!
//! Run in **release** (perf is meaningless in debug):
//!
//! ```text
//! cargo run -p xyscript-app --example ropey_spike --release            # ~300 MB
//! cargo run -p xyscript-app --example ropey_spike --release -- 150     # ~150 MB
//! cargo run -p xyscript-app --example ropey_spike --release -- 1       # quick smoke
//! ```
//!
//! What it measures / asserts:
//!  - **Open** — stream a synthetic N-MB file into a `Rope` (`from_reader`), the
//!    realistic load path. (D1: expect a couple of seconds for 300MB.)
//!  - **Edit latency** — many single-char inserts/deletes at *scattered* (far-
//!    jump) positions. (D1: expect ~microseconds each, *flat* regardless of file
//!    size — this is the property a gap buffer would fail.)
//!  - **Line navigation** — many `char_to_line`/`line_to_char` lookups. (D1:
//!    expect O(log n), sub-microsecond.)
//!  - **Save** — `Rope::write_to` back to disk.
//!  - **Undo correctness (D2)** — apply inverse-op edits across ASCII, multi-byte
//!    (UTF-8), and CRLF boundaries and confirm the buffer is restored exactly.
//!
//! Memory isn't measured here (no extra deps): the rope holds ~the file's bytes
//! resident, so watch peak RSS in Task Manager — expect roughly N MB + overhead.

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::time::Instant;

use ropey::Rope;

fn main() {
    let size_mb: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    println!("=== ropey spike — target ~{size_mb} MB ===\n");

    // Always run the (fast, in-memory) correctness check first, so a smoke run
    // (`-- 1`) still validates the D2 undo model.
    inverse_op_roundtrip();
    println!();

    let mut path = std::env::temp_dir();
    path.push("xyscript_ropey_spike.txt");
    let mut out_path = std::env::temp_dir();
    out_path.push("xyscript_ropey_spike.out.txt");

    // 1. Write a synthetic file (also exercises write-scale). Streamed so we
    //    never hold the whole text in a String.
    let w_start = Instant::now();
    let (bytes, lines) = match write_synthetic(&path, size_mb) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to write synthetic file: {e}");
            return;
        }
    };
    println!(
        "write synthetic file : {:>8.3}s  ({} MiB, {} lines)",
        w_start.elapsed().as_secs_f64(),
        bytes / (1024 * 1024),
        lines,
    );

    // 2. Open: stream the file into a Rope (the realistic load path).
    let o_start = Instant::now();
    let mut rope = match File::open(&path).and_then(|f| Rope::from_reader(BufReader::new(f))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to load file into rope: {e}");
            return;
        }
    };
    println!(
        "open (from_reader)   : {:>8.3}s  (len_chars={}, len_lines={}, len_bytes={})",
        o_start.elapsed().as_secs_f64(),
        rope.len_chars(),
        rope.len_lines(),
        rope.len_bytes(),
    );

    // 3. Scattered single-char inserts (far-jump edits).
    let n = 10_000;
    let mut seed = 0x9E3779B9u64;
    let i_start = Instant::now();
    for _ in 0..n {
        let at = next_pos(&mut seed, rope.len_chars() + 1);
        rope.insert(at, "X");
    }
    report("scattered inserts", i_start, n);

    // 4. Scattered single-char deletes.
    let d_start = Instant::now();
    for _ in 0..n {
        let len = rope.len_chars();
        if len == 0 {
            break;
        }
        let at = next_pos(&mut seed, len);
        rope.remove(at..at + 1);
    }
    report("scattered deletes", d_start, n);

    // 5. Line navigation lookups.
    let navs = 200_000;
    let mut acc = 0usize;
    let nav_start = Instant::now();
    for _ in 0..navs {
        let at = next_pos(&mut seed, rope.len_chars().max(1));
        let line = rope.char_to_line(at);
        acc = acc.wrapping_add(rope.line_to_char(line));
    }
    report("char<->line lookups", nav_start, navs);
    std::hint::black_box(acc);

    // 6. Save: write the rope back out.
    let s_start = Instant::now();
    let save = File::create(&out_path).and_then(|f| rope.write_to(BufWriter::new(f)));
    match save {
        Ok(()) => println!(
            "save (write_to)      : {:>8.3}s",
            s_start.elapsed().as_secs_f64()
        ),
        Err(e) => eprintln!("save failed: {e}"),
    }

    // Cleanup.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&out_path);

    println!(
        "\nMemory: the rope holds ~the file's bytes resident — watch peak RSS \
         (expect roughly {size_mb} MB + overhead).\n\
         Interpretation: per-edit and per-lookup averages should be ~microseconds \
         and *flat* vs. file size; open/save a few seconds for 300MB."
    );
}

/// A simple LCG so positions are deterministic without an RNG dependency.
fn next_pos(seed: &mut u64, modulo: usize) -> usize {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) as usize) % modulo.max(1)
}

fn report(label: &str, start: Instant, n: u32) {
    let total = start.elapsed();
    let avg = total / n;
    println!(
        "{label:<20} : total {:>8.3}s  avg {:>10.3?}/op  ({n} ops)",
        total.as_secs_f64(),
        avg,
    );
}

/// Write ~`size_mb` MB of synthetic UTF-8 text (with non-ASCII so char != byte
/// indices, exercising the rope's char indexing realistically). Streamed.
fn write_synthetic(path: &std::path::Path, size_mb: usize) -> std::io::Result<(u64, usize)> {
    let target = (size_mb as u64) * 1024 * 1024;
    // A repeating unit with multi-byte chars (—, café, ünïcøde) and a newline.
    let unit = "The quick brown fox jumps over the lazy dog — 0123456789.\n\
                Lorem ipsum dolor sit amet, café ünïcøde, 日本語テキスト.\n";
    let unit_bytes = unit.len() as u64;
    let unit_lines = unit.matches('\n').count();

    let mut w = BufWriter::new(File::create(path)?);
    let mut written = 0u64;
    let mut lines = 0usize;
    while written < target {
        w.write_all(unit.as_bytes())?;
        written += unit_bytes;
        lines += unit_lines;
    }
    w.flush()?;
    Ok((written, lines))
}

/// D2 correctness: model an edit as `(at, removed, inserted)` in char units;
/// apply forward, then invert to undo, and confirm the buffer is restored —
/// across ASCII, multi-byte, and CRLF boundaries, including a compounding stack.
fn inverse_op_roundtrip() {
    println!("--- D2: inverse-op undo round-trip ---");

    // A buffer with CRLF line endings, multi-byte chars, and a CJK line.
    let original = "ASCII line one\r\ncafé — naïve ünïcøde\r\n日本語テキスト\r\n";

    // Independent single-edit cases (each: apply, undo, must equal original).
    case("ASCII insert mid-line", original, |r| do_edit(r, 6, 0, "INSERTED "));
    case("delete across multi-byte (café )", original, |r| {
        let at = char_idx(original, "café ");
        do_edit(r, at, "café ".chars().count(), "")
    });
    case("insert AT a CRLF boundary (\\r|\\n)", original, |r| {
        // Between the first '\r' and its '\n'.
        let at = char_idx(original, "\r\n");
        do_edit(r, at + 1, 0, "Z")
    });
    case("delete a whole CRLF pair", original, |r| {
        let at = char_idx(original, "\r\n");
        do_edit(r, at, 2, "")
    });
    case("replace multi-byte with ASCII", original, |r| {
        let at = char_idx(original, "—");
        do_edit(r, at, "—".chars().count(), "--")
    });

    // Compounding: several edits, then undo all in reverse (validates ordering).
    {
        let mut rope = Rope::from_str(original);
        let mut log = Vec::new();
        log.push(do_edit(&mut rope, 0, 0, ">> "));
        let end = rope.len_chars();
        log.push(do_edit(&mut rope, end, 0, " <<"));
        let mid = rope.len_chars() / 2;
        log.push(do_edit(&mut rope, mid, 0, "[mid]"));
        for e in log.iter().rev() {
            undo_edit(&mut rope, e);
        }
        assert_eq(&rope, original, "compounding undo-all");
    }

    println!("  all undo round-trips restored the buffer exactly. ✓");
}

/// One independent case: apply `edit`, undo it, assert the buffer is restored.
fn case(name: &str, original: &str, edit: impl FnOnce(&mut Rope) -> Edit) {
    let mut rope = Rope::from_str(original);
    let e = edit(&mut rope);
    undo_edit(&mut rope, &e);
    assert_eq(&rope, original, name);
    println!("  ok: {name}");
}

struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

/// Forward op: remove `remove_chars` at `at`, then insert `insert` there.
fn do_edit(rope: &mut Rope, at: usize, remove_chars: usize, insert: &str) -> Edit {
    let removed: String = rope.slice(at..at + remove_chars).chars().collect();
    rope.remove(at..at + remove_chars);
    rope.insert(at, insert);
    Edit { at, removed, inserted: insert.to_string() }
}

/// Inverse op: remove the inserted text, restore the removed text.
fn undo_edit(rope: &mut Rope, e: &Edit) {
    let ins_len = e.inserted.chars().count();
    rope.remove(e.at..e.at + ins_len);
    rope.insert(e.at, &e.removed);
}

/// Char index of the first occurrence of `needle` in `s`.
fn char_idx(s: &str, needle: &str) -> usize {
    let byte = s.find(needle).expect("needle present");
    s[..byte].chars().count()
}

fn assert_eq(rope: &Rope, expected: &str, ctx: &str) {
    let got: String = rope.chunks().collect();
    assert!(
        got == expected,
        "{ctx}: undo did NOT restore the buffer\n  expected: {expected:?}\n  got:      {got:?}"
    );
}
