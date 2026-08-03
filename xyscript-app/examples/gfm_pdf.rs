//! 8c manual check: render a GFM sample to PDF through the app's real
//! `gfm_to_pdf` (typst-assets default fonts, justified paragraphs, blue links).

use xyscript_app::{gfm_to_pdf, PreviewFonts};

const SAMPLE: &str = r###"# XyScript — print to PDF (8c)

This is a justified paragraph long enough to show typst's line breaking and
justification across the page: the right edge should be flush. It mixes **bold**,
*italic*, `inline code`, and a [link to example.com](https://example.com) that
should render in blue.

## Checklist
- [x] GFM parsed by comrak
- [ ] task boxes (a known first-cut gap)
- bullet items wrap and align nicely

## A quote
> The art of writing is the art of discovering what you believe.

## Some code
```rust
fn greet(name: &str) {
    println!("Hello, {name}!"); // a comment with a # and $ inside
}
```

| Feature | Status |
|---------|--------|
| Headings | ok |
| Tables   | ok |
"###;

fn main() {
    let pdf = gfm_to_pdf(SAMPLE, std::path::Path::new("."), &PreviewFonts::default(), None).expect("render");
    std::fs::write("gfm_out.pdf", &pdf).unwrap();
    println!("wrote gfm_out.pdf ({} bytes)", pdf.len());
}
