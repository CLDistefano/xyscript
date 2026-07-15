//! typst path of the G3 spike: compile typst markup (the GFM-equivalent of our
//! sample) to PDF in-process. typst is a full typesetting system — it owns
//! layout/justification/page-breaking/fonts; we'd feed it generated markup
//! (a GFM→typst transform) rather than coordinates.

use typst_as_lib::TypstEngine;

const FONTS: &str = r"C:\Windows\Fonts";

// typst markup mirroring the genpdf sample. (GFM→typst is a transform, like
// GFM→HTML: `=`/`==` headings, `*bold*`, `_italic_`, `` `code` ``, `- ` lists,
// ```` ``` ```` raw blocks.)
const MARKUP: &str = r###"
#set page(margin: 18mm)
#set text(font: "Arial", size: 11pt)
#show raw: set text(font: "Consolas")

= XyScript GFM → PDF — typst

== Inline styles
This paragraph mixes *bold*, _italic_, and `monospaced code` — and it is long
enough that typst must perform automatic line breaking and justification across
the available page width, which is exactly the layout work this backend owns.

== A list
- First item
- Second item, a bit longer so it may wrap onto a second line within the list.
- Third item

== A code block
```rust
fn main() {
    println!("hello");
}
```
"###;

fn load_fonts() -> Vec<Vec<u8>> {
    ["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf", "consola.ttf", "consolab.ttf"]
        .iter()
        .map(|f| std::fs::read(format!("{FONTS}\\{f}")).unwrap())
        .collect()
}

/// Compile a typst source FILE to PDF — used to validate 8b's GFM→typst output.
/// Returns (output path, byte size).
pub fn run_file(in_path: &str, out: &str) -> (String, u64) {
    // Leak to 'static to satisfy the builder without lifetime gymnastics (spike).
    let markup: &'static str = Box::leak(std::fs::read_to_string(in_path).unwrap().into_boxed_str());
    let engine = TypstEngine::builder().main_file(markup).fonts(load_fonts()).build();
    let doc = engine.compile().output.expect("typst compile (from GFM)");
    let pdf = typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).expect("typst pdf");
    std::fs::write(out, &pdf).unwrap();
    (out.to_string(), pdf.len() as u64)
}

/// Compile a typst FILE using typst's own BUNDLED fonts (New Computer Modern + math + mono),
/// so a feature demo with math / default fonts renders without any Windows-font dependency.
/// Returns (output path, byte size).
pub fn run_demo(in_path: &str, out: &str) -> (String, u64) {
    let markup: &'static str = Box::leak(std::fs::read_to_string(in_path).unwrap().into_boxed_str());
    let fonts: Vec<Vec<u8>> = typst_assets::fonts().map(|f| f.to_vec()).collect();
    let engine = TypstEngine::builder().main_file(markup).fonts(fonts).build();
    let doc = engine.compile().output.expect("typst compile (demo)");
    let pdf = typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).expect("typst pdf");
    std::fs::write(out, &pdf).unwrap();
    (out.to_string(), pdf.len() as u64)
}

/// Returns (output path, byte size).
pub fn run(out: &str) -> (String, u64) {
    let fonts: Vec<Vec<u8>> =
        ["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf", "consola.ttf", "consolab.ttf"]
            .iter()
            .map(|f| std::fs::read(format!("{FONTS}\\{f}")).unwrap())
            .collect();

    let engine = TypstEngine::builder().main_file(MARKUP).fonts(fonts).build();
    let doc = engine.compile().output.expect("typst compile");
    let pdf = typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).expect("typst pdf");
    std::fs::write(out, &pdf).unwrap();
    (out.to_string(), pdf.len() as u64)
}
