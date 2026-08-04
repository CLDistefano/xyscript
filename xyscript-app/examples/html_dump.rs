//! Scratch harness: convert a real HTML file and compile the result, to check robustness against
//! messy real-world markup. `cargo run -p xyscript-app --example html_dump -- <file>`
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).expect("usage: html_dump <file>");
    let src = std::fs::read_to_string(&path).expect("read");
    let body = xyscript_app::html::to_typst(&src);
    println!("input {} bytes -> body {} bytes", src.len(), body.len());
    println!("--- first 700 chars of emitted typst ---\n{}", &body[..body.len().min(700)]);
    match xyscript_app::pdf::typst_to_pdf(&body, Path::new("."), None) {
        Ok(pdf) => println!("\n--- COMPILED OK: {} bytes of PDF ---", pdf.len()),
        Err(e) => println!("\n--- COMPILE FAILED ---\n{e}"),
    }
}
