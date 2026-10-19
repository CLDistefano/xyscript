//! Empirical check + timing for the color-emoji font. Renders an emoji line through the real Typst
//! pipeline (which now also loads any face in `$XSDIR/fonts/`) to SVG, PDF, and a raster (the live
//! preview path), reporting what came out and how long each render took — so we can see whether a
//! 24 MB emoji font in `fonts/` slows the preview.

use std::time::Instant;
use xyscript_app::{gfm_to_image, gfm_to_pdf, gfm_to_svg, PreviewFonts};

const SAMPLE: &str = "Monochrome check mark: \u{2714}\n\nColour emoji: \u{1F389} party, \u{1F600} grin, \u{2764} heart, \u{1F44D} thumbs-up\n\nPlain ASCII baseline: Hello world.\n";

fn main() {
    let dir = std::path::Path::new(".");
    let fonts = PreviewFonts::default();

    // Warm up (first render pays the one-time font-dir scan + face indexing).
    let _ = gfm_to_image(SAMPLE, dir, 2.0, &fonts, None, Default::default());

    let n = 10;
    let t = Instant::now();
    for _ in 0..n {
        gfm_to_image(SAMPLE, dir, 2.0, &fonts, None, Default::default()).expect("raster");
    }
    let per_preview = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

    let t = Instant::now();
    for _ in 0..n {
        gfm_to_svg(SAMPLE, dir, &fonts, None).expect("svg");
    }
    let per_svg = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

    let svg = gfm_to_svg(SAMPLE, dir, &fonts, None).unwrap().into_iter().next().unwrap(); // page 1
    std::fs::write("emoji_probe.svg", &svg).unwrap();
    let pdf = gfm_to_pdf(SAMPLE, dir, &fonts, None).unwrap();
    std::fs::write("emoji_probe.pdf", &pdf).unwrap();
    let images = String::from_utf8_lossy(&svg).matches("<image").count();

    println!("preview raster: {per_preview:.1} ms/render | svg export: {per_svg:.1} ms/render");
    println!("SVG {} bytes, <image>={images} (colour-glyph bitmaps) | PDF {} bytes", svg.len(), pdf.len());
    println!("wrote emoji_probe.svg / emoji_probe.pdf");
}
