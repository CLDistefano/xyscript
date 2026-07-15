//! GUI-shell spike (`gui-shell-plan.md`, GUI1: egui).
//!
//! Validates the integration shape — **not** a full editor:
//! 1. **Input** — egui key events → canonical `KeyEvent` → `Editor::feed_key`
//!    (through the real KBD pipeline, "it's all XPL"). NB the framework gives
//!    *logical* keys, not OS scancodes, so we map them ourselves — the key GUI2
//!    finding (see the report).
//! 2. **Render** — draw the active doc via the core's highlighter spans
//!    (`styled_line`), proving the styled-span pipeline renders in the GUI.
//! 3. **Preview** — render the doc's typst markup to a raster image (the same
//!    markup 8c prints) and show it beside the editor — the first live preview.
//!
//! `--render-only` writes the preview PNG headlessly (no window) for inspection.

use eframe::egui;
use egui::{Color32, FontId, TextFormat};

use xyscript_app::style::{Color as XyColor, Span};
use xyscript_app::Editor;
use xyscript_kbd::event::KeyEvent;
use xyscript_kbd::{load, LoadOutcome};

const INTERNAL_KBD: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../xyscript-kbd/kbd/xyscript-internal.kbd"));

const SAMPLE_MD: &str = "# XyScript GUI spike\n\nType to edit. The **left** pane is the source with \
*live syntax highlighting*; the right pane is the typst-rendered `preview`.\n\n- one\n- two\n";

const PREAMBLE: &str = "#set page(margin: 1.5cm)\n#set par(justify: true)\n\
#show link: set text(fill: blue)\n\n";

// ---- typst preview (shared by --render-only and the live pane) -------------

fn compile(source: &str) -> Result<typst::layout::PagedDocument, String> {
    let markup = format!("{PREAMBLE}{}\n", xyscript_app::to_typst(source));
    let engine = typst_as_lib::TypstEngine::builder()
        .main_file(markup)
        .fonts(typst_assets::fonts())
        .build();
    engine.compile().output.map_err(|e| format!("{e}"))
}

fn render_pixmap(source: &str, scale: f32) -> Result<tiny_skia::Pixmap, String> {
    let doc = compile(source)?;
    let page = doc.pages.first().ok_or_else(|| "no pages".to_string())?;
    Ok(typst_render::render(page, scale))
}

// ---- input: egui logical key -> canonical scancode (set-1, matches the KBD) -

fn key_to_code(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    Some(match key {
        Q => 16, W => 17, E => 18, R => 19, T => 20, Y => 21, U => 22, I => 23, O => 24, P => 25,
        A => 30, S => 31, D => 32, F => 33, G => 34, H => 35, J => 36, K => 37, L => 38,
        Z => 44, X => 45, C => 46, V => 47, B => 48, N => 49, M => 50,
        Space => 57,
        Enter => 28,
        Backspace => 14,
        _ => return None,
    })
}

// ---- the app ---------------------------------------------------------------

struct SpikeApp {
    editor: Editor,
    preview: Option<egui::TextureHandle>,
    dirty: bool,
}

impl SpikeApp {
    fn new() -> SpikeApp {
        let km = match load(INTERNAL_KBD, Some(120)) {
            LoadOutcome::Installed(km) => km,
            LoadOutcome::Rejected(_) => panic!("sample kbd failed to parse"),
        };
        let mut editor = Editor::new(km);
        editor.host_mut().virtual_fs = true;
        editor.host_mut().files.insert("scratch.md".into(), SAMPLE_MD.into());
        editor.run_command("open scratch.md");
        editor.run_command("HIGHLIGHT on");
        SpikeApp { editor, preview: None, dirty: true }
    }

    fn refresh_preview(&mut self, ctx: &egui::Context) {
        let text = self.editor.document().text();
        if let Ok(pm) = render_pixmap(&text, 2.0) {
            let img = egui::ColorImage::from_rgba_premultiplied(
                [pm.width() as usize, pm.height() as usize],
                pm.data(),
            );
            self.preview = Some(ctx.load_texture("preview", img, Default::default()));
        }
        self.dirty = false;
    }
}

fn span_format(span: &Span) -> TextFormat {
    let color = match span.style.fg {
        XyColor::Default => Color32::from_gray(220),
        XyColor::Black => Color32::BLACK,
        XyColor::Red => Color32::from_rgb(220, 60, 60),
        XyColor::Green => Color32::from_rgb(60, 180, 90),
        XyColor::Yellow => Color32::from_rgb(200, 170, 40),
        XyColor::Blue => Color32::from_rgb(90, 140, 240),
        XyColor::Magenta => Color32::from_rgb(200, 100, 220),
        XyColor::Cyan => Color32::from_rgb(80, 190, 200),
        XyColor::White => Color32::WHITE,
    };
    TextFormat {
        font_id: FontId::monospace(15.0),
        color,
        italics: span.style.italic,
        ..Default::default()
    }
}

impl eframe::App for SpikeApp {
    // eframe 0.34 hands us a root `Ui`; we compose panels with `show_inside`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // (1) Input: route egui keys through the real KBD pipeline.
        for ev in ctx.input(|i| i.events.clone()) {
            if let egui::Event::Key { key, pressed: true, .. } = ev {
                if let Some(code) = key_to_code(key) {
                    self.editor.feed_key(KeyEvent::down(code));
                    self.dirty = true;
                }
            }
        }
        if self.dirty {
            self.refresh_preview(&ctx);
        }

        // (2) Editor pane: render the core's highlighter spans per line.
        egui::Panel::left("editor").resizable(true).default_size(440.0).show_inside(ui, |ui| {
            let doc = self.editor.document();
            ui.label(format!("scratch.md  ln {}:{}  [highlight on]", doc.line(), doc.column()));
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let tokens = self.editor.highlights().to_vec();
                let count = self.editor.document().line_count();
                for l in 0..count {
                    let doc = self.editor.document();
                    let (start, _end) = doc.line_byte_range(l);
                    let text = doc.line_text(l);
                    let spans = xyscript_app::highlight::styled_line(&text, start, &tokens);
                    let mut job = egui::text::LayoutJob::default();
                    if spans.is_empty() {
                        job.append(" ", 0.0, span_format(&Span::plain(" ")));
                    }
                    for s in &spans {
                        job.append(&s.text, 0.0, span_format(s));
                    }
                    ui.label(job);
                }
            });
        });

        // (3) Preview pane (the remaining central area): the typst-rendered image.
        ui.label("preview (typst → raster)");
        ui.separator();
        egui::ScrollArea::both().show(ui, |ui| {
            if let Some(tex) = &self.preview {
                ui.image((tex.id(), tex.size_vec2()));
            } else {
                ui.label("(rendering…)");
            }
        });
    }
}

fn main() {
    if std::env::args().any(|a| a == "--render-only") {
        match render_pixmap(SAMPLE_MD, 2.0).and_then(|pm| pm.encode_png().map_err(|e| e.to_string()))
        {
            Ok(png) => {
                std::fs::write("preview.png", &png).unwrap();
                println!("wrote preview.png ({} bytes)", png.len());
            }
            Err(e) => eprintln!("render failed: {e}"),
        }
        return;
    }

    let opts = eframe::NativeOptions::default();
    eframe::run_native("XyScript GUI spike", opts, Box::new(|_cc| Ok(Box::new(SpikeApp::new()))))
        .unwrap();
}
