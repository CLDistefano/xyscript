//! Rebuild the PDF versions of the authored-Typst docs (`docs/reference.typ`,
//! `docs/manual.typ`) through the app's real `typst_to_pdf` — same engine, fonts,
//! and file-resolution the GUI `TOPDF`/preview use. `doc_dir` is each file's own
//! folder so any `#image`/`#read` resolves.
//!
//! Run: `cargo run -p xyscript-app --example build_docs_pdf`

use std::path::Path;
use xyscript_app::typst_to_pdf;

fn build(typ: &str) {
    let src = std::fs::read_to_string(typ).unwrap_or_else(|e| panic!("read {typ}: {e}"));
    let dir = Path::new(typ).parent().unwrap_or_else(|| Path::new("."));
    let pdf = typst_to_pdf(&src, dir).unwrap_or_else(|e| panic!("compile {typ}:\n{e}"));
    let out = Path::new(typ).with_extension("pdf");
    std::fs::write(&out, &pdf).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    println!("wrote {} ({} bytes)", out.display(), pdf.len());
}

fn main() {
    build("docs/reference.typ");
    build("docs/manual.typ");
}
