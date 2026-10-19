//! Empirical check: does the Typst pipeline (bundled `typst_assets` fonts only) render emoji, and
//! does SVG export differ from PDF? Renders the same emoji line to both and reports what came out.

use xyscript_app::{gfm_to_pdf, gfm_to_svg, PreviewFonts};

const SAMPLE: &str = "Monochrome check mark: \u{2714}\n\nColour emoji: \u{1F389} party, \u{1F600} grin, \u{2764} heart, \u{1F44D} thumbs-up\n\nPlain ASCII baseline: Hello world.\n";

fn main() {
    let dir = std::path::Path::new(".");
    let fonts = PreviewFonts::default();

    let svg = gfm_to_svg(SAMPLE, dir, &fonts, None).expect("svg render");
    std::fs::write("emoji_probe.svg", &svg).unwrap();
    let pdf = gfm_to_pdf(SAMPLE, dir, &fonts, None).expect("pdf render");
    std::fs::write("emoji_probe.pdf", &pdf).unwrap();

    let svg_text = String::from_utf8_lossy(&svg);
    // Telltales in the SVG: <image> = a rasterized colour-glyph bitmap was embedded; a <path> with a
    // non-black fill = a vector colour-glyph layer; glyph <path>/<use> count ≈ how many glyphs drew.
    let images = svg_text.matches("<image").count();
    let paths = svg_text.matches("<path").count();
    let uses = svg_text.matches("<use").count();
    // Any fill that isn't black/none — a hint that a colour glyph (not plain text) was drawn.
    let colour_fills = svg_text
        .match_indices("fill=\"#")
        .filter(|(i, _)| {
            let hex = &svg_text[i + 6..(i + 6 + 6).min(svg_text.len())];
            !hex.eq_ignore_ascii_case("000000")
        })
        .count();

    println!("SVG: {} bytes | <image>={images} <path>={paths} <use>={uses} non-black-fills={colour_fills}", svg.len());
    println!("PDF: {} bytes", pdf.len());
    println!("wrote emoji_probe.svg / emoji_probe.pdf");
    println!("(open the SVG to SEE whether the emoji drew in colour, monochrome, or as .notdef boxes)");
}
