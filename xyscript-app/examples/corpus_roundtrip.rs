//! Byte-exact round-trip verification against a real legacy corpus
//! (`byte-text-engine-plan.md` step 4 — "verify read→write vs the FandH files").
//!
//! For every file under a directory, this reads the raw bytes from disk, loads
//! them into a [`Document`] (the real open path), and checks that
//! [`Document::save_bytes`] returns the *exact* same bytes — the sacred
//! guarantee for the irreplaceable diaries. It also decodes every line through
//! the chosen view to confirm display never panics, and reports the encoding the
//! provisional default picked. It prints **only paths/counts/encodings** — never
//! file contents — so personal data is not exposed.
//!
//! Usage (PowerShell):
//!   cargo run -p xyscript-app --example corpus_roundtrip -- "C:\Users\cldis\Dropbox\Copy\FandH"

use std::path::{Path, PathBuf};

use xyscript_app::document::Document;

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"C:\Users\cldis\Dropbox\Copy\FandH".to_string());
    let root = PathBuf::from(root);

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    let (mut ok, mut read_err, mut mismatch) = (0usize, 0usize, 0usize);
    let (mut cp437, mut latin1) = (0usize, 0usize);
    let mut total_bytes = 0u64;
    let mut mismatches: Vec<PathBuf> = Vec::new();

    for path in &files {
        let raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                read_err += 1;
                continue;
            }
        };
        total_bytes += raw.len() as u64;

        let mut doc = Document::new();
        doc.load(raw.clone(), path.clone());
        match doc.encoding_name() {
            "cp437" => cp437 += 1,
            "latin1" => latin1 += 1,
            _ => {}
        }
        // Decode every line — must not panic on any byte sequence.
        for l in 0..doc.buffer().line_count() {
            let _ = doc.buffer().line_text(l);
        }
        // The guarantee: save writes the bytes back verbatim.
        if doc.save_bytes() == raw {
            ok += 1;
        } else {
            mismatch += 1;
            mismatches.push(path.clone());
        }
    }

    println!("corpus root : {}", root.display());
    println!("files seen  : {}", files.len());
    println!("bytes read  : {total_bytes}");
    println!("read errors : {read_err}");
    println!("byte-exact  : {ok}");
    println!("MISMATCHES  : {mismatch}");
    println!("view cp437  : {cp437}");
    println!("view latin1 : {latin1}");
    if !mismatches.is_empty() {
        println!("-- mismatching paths (content not shown) --");
        for p in mismatches.iter().take(20) {
            println!("  {}", p.display());
        }
    }
    if mismatch == 0 && read_err == 0 {
        println!("RESULT: PASS — every file round-trips byte-for-byte.");
    } else {
        println!("RESULT: FAIL — see counts above.");
        std::process::exit(1);
    }
}

/// Recursively collect regular files under `dir` (skips directories it can't read).
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}
