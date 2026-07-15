//! Spike 2 / Phase B (verifiable part) — can cosmic-text host a real *editing
//! surface* (caret + selection + hit-testing) on multilingual/bidi text?
//!
//! Renders `cosmic_text::Editor` (caret + a selection that spans the bidi line)
//! to a PNG, and exercises pixel→cursor hit-testing. `Cursor { line, index }`
//! (index = byte offset within the line) is exactly the bridge to our core's
//! byte-offset model. The live shell/IME/physical-key parts are the follow-up.

use cosmic_text::{Attrs, Buffer, Color, Cursor, Edit, Editor, FontSystem, Metrics, Selection, Shaping, SwashCache};

const W: usize = 1100;
const H: usize = 220;

const SAMPLE: &str = "\
Scripts: English · 中文 · 日本語 · 한국어 · Ελληνικά
Bidi: Hebrew שלום עולם and Arabic مرحبا بالعالم within English.
Diacritics & emoji: café  Đắk Lắk  /ˈtʃɪkənz/  😀 🌍 ✅";

/// Blend one glyph rect (mask alpha or color) onto the white canvas.
fn blend(canvas: &mut [u8], x: i32, y: i32, w: u32, h: u32, color: Color) {
    let (sr, sg, sb) = (color.r() as f32, color.g() as f32, color.b() as f32);
    let sa = color.a() as f32 / 255.0;
    for py in y..y + h as i32 {
        for px in x..x + w as i32 {
            if px < 0 || py < 0 || px as usize >= W || py as usize >= H {
                continue;
            }
            let i = (py as usize * W + px as usize) * 4;
            for (c, s) in [(0usize, sr), (1, sg), (2, sb)] {
                let dst = canvas[i + c] as f32;
                canvas[i + c] = (s * sa + dst * (1.0 - sa)).round() as u8;
            }
        }
    }
}

fn main() {
    let mut fs = FontSystem::new();
    let mut swash = SwashCache::new();

    let mut buffer = Buffer::new(&mut fs, Metrics::new(24.0, 40.0));
    buffer.set_size(Some(W as f32), Some(H as f32));
    buffer.set_text(SAMPLE, &Attrs::new(), Shaping::Advanced, None);

    let mut editor = Editor::new(buffer);

    // Selection spanning the bidi line: from the Hebrew through the Arabic.
    let line1 = SAMPLE.lines().nth(1).unwrap();
    let anchor = line1.find("שלום").unwrap();
    let end = line1.find(" within").unwrap();
    editor.set_selection(Selection::Normal(Cursor::new(1, anchor)));
    editor.set_cursor(Cursor::new(1, end)); // caret at the end of the selection

    let mut canvas = vec![255u8; W * H * 4];
    editor.draw(
        &mut fs,
        &mut swash,
        Color::rgb(0, 0, 0),       // text
        Color::rgb(220, 40, 40),   // caret
        Color::rgb(180, 210, 255), // selection background
        Color::rgb(0, 0, 0),       // selected text
        |x, y, w, h, color| blend(&mut canvas, x, y, w, h, color),
    );

    image::RgbaImage::from_raw(W as u32, H as u32, canvas)
        .expect("canvas size")
        .save("canvas.png")
        .expect("save png");
    println!("wrote canvas.png ({W}x{H})");
    println!("selection on line 1: bytes {anchor}..{end} = {:?}", &line1[anchor..end]);

    // Hit-testing: pixel → Cursor{line, byte index}. Sample a few points.
    for (label, x, y) in [("line0 mid", 120.0_f32, 18.0_f32), ("line1 bidi", 300.0, 58.0), ("line2 emoji", 600.0, 98.0)] {
        let hit = editor.with_buffer(|b| b.hit(x, y));
        println!("hit({label}: {x},{y}) -> {hit:?}");
    }
}
