//! genpdf path of the G3 spike: build a representative GFM-ish document and
//! typeset it to PDF. genpdf does its OWN layout (line-breaking, page breaks) —
//! you hand it elements, not coordinates — which is the key architectural point
//! for G3 (a layout-owning backend vs a primitive painter).

use std::path::Path;

use genpdf::elements::{Break, Paragraph, UnorderedList};
use genpdf::fonts::{FontData, FontFamily};
use genpdf::style::Style;
use genpdf::{Document, SimplePageDecorator};

const FONTS: &str = r"C:\Windows\Fonts";

fn family(reg: &str, bold: &str, italic: &str, bolditalic: &str) -> FontFamily<FontData> {
    let load = |f: &str| FontData::new(std::fs::read(Path::new(FONTS).join(f)).unwrap(), None).unwrap();
    FontFamily {
        regular: load(reg),
        bold: load(bold),
        italic: load(italic),
        bold_italic: load(bolditalic),
    }
}

/// Returns (output path, byte size).
pub fn run(out: &str) -> (String, u64) {
    let proportional = family("arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf");
    let mono = family("consola.ttf", "consolab.ttf", "consolai.ttf", "consolaz.ttf");

    let mut doc = Document::new(proportional);
    doc.set_title("XyScript GFM → PDF (genpdf)");
    let mono_family = doc.add_font_family(mono);

    let mut deco = SimplePageDecorator::new();
    deco.set_margins(18);
    doc.set_page_decorator(deco);

    // H1
    let mut h1 = Paragraph::default();
    h1.push_styled("XyScript GFM → PDF — genpdf", Style::new().bold().with_font_size(20));
    doc.push(h1);
    doc.push(Break::new(0.5));

    // H2
    doc.push(Paragraph::default().styled_string("Inline styles", Style::new().bold().with_font_size(14)));

    // Paragraph with inline bold/italic/code; genpdf wraps it to the page width.
    let mut p = Paragraph::default();
    p.push("This paragraph mixes ");
    p.push_styled("bold", Style::new().bold());
    p.push(", ");
    p.push_styled("italic", Style::new().italic());
    p.push(", and ");
    p.push_styled("monospaced code", Style::new().with_font_family(mono_family));
    p.push(" — and it is long enough that genpdf must perform automatic line breaking across the available page width, which is exactly the layout work a backend would own.");
    doc.push(p);
    doc.push(Break::new(0.5));

    // Bullet list
    doc.push(Paragraph::default().styled_string("A list", Style::new().bold().with_font_size(14)));
    let mut list = UnorderedList::new();
    list.push(Paragraph::new("First item"));
    list.push(Paragraph::new("Second item, a bit longer so it may wrap onto a second line within the list."));
    list.push(Paragraph::new("Third item"));
    doc.push(list);
    doc.push(Break::new(0.5));

    // "Code block" — a monospaced paragraph (no real box/background without more work).
    doc.push(Paragraph::default().styled_string("A code block", Style::new().bold().with_font_size(14)));
    for line in ["fn main() {", "    println!(\"hello\");", "}"] {
        doc.push(Paragraph::default().styled_string(line, Style::new().with_font_family(mono_family)));
    }

    doc.render_to_file(out).expect("render genpdf");
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    (out.to_string(), size)
}
