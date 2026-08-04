//! Document → PDF via typst (`gfm-track-plan.md` step 8c; Typst-as-input phase 0).
//!
//! Two front-ends share one engine. **GFM** ([`gfm_to_pdf`]/`gfm_to_image`): [`gfm::to_typst`]
//! produces a typst body, we prepend a small **preamble** (page geometry, justified paragraphs, link
//! styling), then compile. **Typst-as-input** ([`typst_to_pdf`]/`typst_to_image`): the buffer already
//! IS typst markup, so we compile it verbatim — no translation, no preamble (the document controls its
//! own page/styling). Both compile in-process with the embedded **typst** engine (decision G3). Fonts:
//! the bundled `typst-assets` defaults, plus — for the families a document actually names (GFM via the
//! `PreviewFonts` setting, authored Typst via [`fonts_referenced`]) — the matching installed system
//! faces. Renders to PDF or **raster** — one engine, every surface.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use typst::diag::{FileError, FileResult, SourceDiagnostic, Warned};
use typst::foundations::Bytes;
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source};
use typst::World;
use typst_as_lib::file_resolver::FileResolver;
use typst_as_lib::TypstEngine;

/// Resolves a document's `#image` / `#include` / `#read` targets from the real filesystem, so the
/// preview and `TOPDF` can embed local files (without this, the engine has ONLY the in-memory markup,
/// so every path is "file not found").
///
/// Typst treats every path as a sandboxed *virtual* path rooted at the project root, and STRIPS any
/// leading drive/root — `d:\pics\logo.png` (or `d:/pics/logo.png`) both arrive here as the virtual
/// path `pics/logo.png`. So we try each target TWICE: first under the document's own directory
/// (standard Typst: an image sitting next to the `.typ`, e.g. `#image("logo.png")`), then under the
/// document's DRIVE ROOT — which reconstitutes an absolute path on that drive (`X:\pics\logo.png`).
/// Cross-drive absolute paths aren't recoverable (the drive letter is gone by the time we're called).
struct DocFileResolver {
    doc_dir: PathBuf,
    drive_root: Option<PathBuf>,
}

impl DocFileResolver {
    fn read_bytes(&self, id: FileId) -> FileResult<Vec<u8>> {
        // No package support (`@preview/...`): only local files.
        if id.package().is_some() {
            return Err(FileError::NotFound(id.vpath().as_rootless_path().to_path_buf()));
        }
        let vpath = id.vpath().as_rootless_path();
        let mut candidates = vec![self.doc_dir.join(vpath)];
        if let Some(root) = &self.drive_root {
            candidates.push(root.join(vpath));
        }
        let mut err = None;
        for path in &candidates {
            match std::fs::read(path) {
                Ok(bytes) => return Ok(bytes),
                Err(e) => err = Some(FileError::from_io(e, path)),
            }
        }
        Err(err.unwrap_or_else(|| FileError::NotFound(vpath.to_path_buf())))
    }
}

impl FileResolver for DocFileResolver {
    fn resolve_binary(&self, id: FileId) -> FileResult<Cow<'_, Bytes>> {
        Ok(Cow::Owned(Bytes::new(self.read_bytes(id)?)))
    }
    fn resolve_source(&self, id: FileId) -> FileResult<Cow<'_, Source>> {
        let bytes = self.read_bytes(id)?;
        let text = String::from_utf8(bytes).map_err(|_| FileError::InvalidUtf8)?;
        Ok(Cow::Owned(Source::new(id, text)))
    }
}

/// The drive/filesystem root of an absolute path (`X:\proj\doc.typ` → `X:\`), for the resolver's
/// absolute-path fallback. `None` for a relative path (nothing to anchor an absolute path to).
fn root_of(dir: &Path) -> Option<PathBuf> {
    dir.is_absolute().then(|| dir.ancestors().last().map(Path::to_path_buf)).flatten()
}

/// The user-selectable font settings for **GFM/Markdown** rendering — applied identically to the
/// live Preview pane and the `TOPDF` export (both go through [`compile_gfm`]). Every field is
/// `None` by default, i.e. Typst's built-ins (Libertinus for prose, a bundled mono for `raw`).
/// Authored-Typst documents ignore this — they style themselves (see [`compile_typst`]).
#[derive(Default, Clone)]
pub struct PreviewFonts {
    /// Body typeface family name (`#set text(font: …)`); `None` = Typst default.
    pub family: Option<String>,
    /// Body size in points (`#set text(size: …)`); `None` = Typst default.
    pub size: Option<f32>,
    /// Monospace family for `code` spans / fenced blocks (`#show raw: set text(font: …)`).
    pub code: Option<String>,
    /// Page margins (`#set page(margin: …)`). Each side is a validated Typst length string (see
    /// [`normalize_length`]). Defaults to 1 inch all around.
    pub margins: PageMargins,
}

/// The four page margins for GFM rendering, each a validated Typst length string (`"1in"`, `"2.5cm"`,
/// `"18pt"`, …). Set per-side by `PVMARGINL/R/T/B` or all four by `PVMARGIN`.
#[derive(Clone)]
pub struct PageMargins {
    pub left: String,
    pub right: String,
    pub top: String,
    pub bottom: String,
}

impl Default for PageMargins {
    fn default() -> Self {
        let one = || "1in".to_string(); // 1-inch standard print margin
        PageMargins { left: one(), right: one(), top: one(), bottom: one() }
    }
}

/// Validate + normalize a margin length: a non-negative number with an optional unit suffix (`in`
/// (default), `cm`, `mm`, `pt`). Returns a clean `"<n><unit>"` string safe to embed in the Typst
/// preamble (rebuilt from the parsed parts, so junk can't inject markup), or `Err(msg)`.
pub fn normalize_length(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("expected a length like 1in, 2.5cm, 18pt (bare number = inches)".into());
    }
    // Split into the leading numeric part and a trailing (alphabetic) unit.
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num_str, unit_str) = t.split_at(split);
    let num: f64 = num_str.trim().parse().map_err(|_| format!("bad number in {t:?}"))?;
    if !num.is_finite() || num < 0.0 {
        return Err(format!("margin must be ≥ 0 (got {t:?})"));
    }
    let unit = match unit_str.trim().to_ascii_lowercase().as_str() {
        "" | "in" | "inch" | "inches" | "\"" => "in",
        "cm" => "cm",
        "mm" => "mm",
        "pt" | "point" | "points" => "pt",
        other => return Err(format!("unknown unit {other:?} (use in, cm, mm, or pt)")),
    };
    // Rebuild from parsed parts — trim a trailing ".0" so "1in" reads cleanly.
    let n = if num.fract() == 0.0 { format!("{}", num as i64) } else { format!("{num}") };
    Ok(format!("{n}{unit}"))
}

/// Base rendering config prepended to every GFM body: justified text, blue links. The page margin is
/// emitted separately in [`gfm_preamble`] (it's user-configurable via `PVMARGIN*`).
const PREAMBLE: &str = "\
#set par(justify: true)
#show link: set text(fill: blue)
";

/// System font database, scanned **once** (a rescan per preview keystroke would be far too slow) and
/// reused across every render. Lets the Preview/`TOPDF` path resolve a user-chosen family name to the
/// face bytes handed to Typst. Self-contained here so it also works headless (`--run` `TOPDF`).
fn system_fonts() -> &'static fontdb::Database {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// The face-data blobs for every face of `family` (regular/bold/italic/… so emphasis renders),
/// de-duplicated by underlying file. Empty if the family isn't installed — Typst then falls back to
/// its default, exactly as if no font were chosen.
fn family_font_bytes(family: &str) -> Vec<Vec<u8>> {
    let db = system_fonts();
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut seen: Vec<(usize, u8)> = Vec::new(); // (len, first byte) cheap de-dup key for TTC files
    for face in db.faces() {
        if !face.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(family)) {
            continue;
        }
        if let Some(bytes) = db.with_face_data(face.id, |data, _| data.to_vec()) {
            let key = (bytes.len(), bytes.first().copied().unwrap_or(0));
            if !seen.contains(&key) {
                seen.push(key);
                out.push(bytes);
            }
        }
    }
    out
}

/// Scan authored-Typst `source` for the font family names it references — `font: "Name"` or
/// `font: ("Name", "Fallback")` (as in `#set text(font: …)`, `#text(font: …)`, or a `#show raw: set
/// text(font: …)` rule) — so we can resolve *just those* from the system, giving authored Typst the
/// same system-font access as the Typst CLI WITHOUT loading every installed face. Limitations: it
/// won't follow a family named indirectly through a variable (rare), and a stray `font:` in a string
/// or comment yields at most a harmless unused-font load. Deduplicated case-insensitively.
fn fonts_referenced(source: &str) -> Vec<String> {
    let b = source.as_bytes();
    let skip_ws = |j: &mut usize| {
        while *j < b.len() && (b[*j] as char).is_whitespace() {
            *j += 1;
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut search = 0;
    while let Some(rel) = source[search..].find("font:") {
        let mut j = search + rel + 5; // past "font:"
        skip_ws(&mut j);
        // A '(' introduces a family list (`("A", "B")`); otherwise a single string follows.
        let list = j < b.len() && b[j] == b'(';
        if list {
            j += 1;
        }
        loop {
            skip_ws(&mut j);
            if j >= b.len() || b[j] != b'"' {
                break;
            }
            j += 1; // opening quote
            let start = j;
            while j < b.len() && b[j] != b'"' {
                // Step over an escaped char (`\"`, `\\`) so it doesn't end the string early.
                j += if b[j] == b'\\' && j + 1 < b.len() { 2 } else { 1 };
            }
            // Byte-offset slice keeps non-ASCII family names intact.
            let name = source[start..j.min(source.len())].replace("\\\"", "\"");
            if j < b.len() {
                j += 1; // closing quote
            }
            if !name.is_empty() && !out.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                out.push(name);
            }
            if !list {
                break;
            }
            skip_ws(&mut j);
            if j < b.len() && b[j] == b',' {
                j += 1; // another family in the list
                continue;
            }
            break;
        }
        search = (search + rel + 5).max(j);
    }
    out
}

/// Build the engine over a finished typst markup string (plus any extra font blobs the document
/// needs) and compile to a paged document. Shared by the GFM and Typst-as-input front-ends. Compile
/// errors are formatted as `line:col: message` via the engine's `World` (so the location resolves to
/// the source) — for Typst-as-input, compiled verbatim with no preamble, those line numbers map
/// exactly to the user's buffer.
fn compile_markup(
    markup: String,
    extra_fonts: Vec<Vec<u8>>,
    doc_dir: &Path,
) -> Result<PagedDocument, String> {
    // Unify the bundled `&'static [u8]` fonts and the (owned) user font blobs into one homogeneous
    // `Vec<Vec<u8>>`. `to_vec` on the bundled set matches what `.fonts()` did internally before, so
    // there's no extra per-render cost beyond the handful of user faces.
    let mut fonts: Vec<Vec<u8>> = typst_assets::fonts().map(|b| b.to_vec()).collect();
    fonts.extend(extra_fonts);
    // The filesystem resolver lets the document read local files (`#image`/`#include`/`#read`),
    // relative to its own directory and — for absolute paths, whose drive Typst strips — its drive.
    let resolver = DocFileResolver { doc_dir: doc_dir.to_path_buf(), drive_root: root_of(doc_dir) };
    let engine =
        TypstEngine::builder().main_file(markup).fonts(fonts).add_file_resolver(resolver).build();
    engine
        .with_world(|world| {
            let Warned { output, .. } = typst::compile::<PagedDocument>(world);
            output.map_err(|diags| format_diagnostics(&diags, world))
        })
        .map_err(|e| format!("typst: {e}"))?
}

/// Escape a font family name for embedding inside a typst string literal (`"…"`).
fn typst_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The GFM preamble for `fonts`: the base rules plus any `#set text` / `#show raw` overrides the
/// user chose, and the extra font blobs (body + code families) those overrides need. Blank/absent
/// fields leave Typst's defaults in place.
fn gfm_preamble(fonts: &PreviewFonts) -> (String, Vec<Vec<u8>>) {
    let m = &fonts.margins;
    // Page margins first (each side is a pre-validated Typst length), then the base rules.
    let mut pre = format!(
        "#set page(margin: (left: {}, right: {}, top: {}, bottom: {}))\n",
        m.left, m.right, m.top, m.bottom
    );
    pre.push_str(PREAMBLE);
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    match (&fonts.family, fonts.size) {
        (Some(f), Some(sz)) => {
            pre.push_str(&format!("#set text(font: \"{}\", size: {}pt)\n", typst_str(f), sz));
        }
        (Some(f), None) => pre.push_str(&format!("#set text(font: \"{}\")\n", typst_str(f))),
        (None, Some(sz)) => pre.push_str(&format!("#set text(size: {sz}pt)\n")),
        (None, None) => {}
    }
    if let Some(f) = &fonts.family {
        blobs.extend(family_font_bytes(f));
    }
    if let Some(code) = &fonts.code {
        pre.push_str(&format!("#show raw: set text(font: \"{}\")\n", typst_str(code)));
        blobs.extend(family_font_bytes(code));
    }
    pre.push('\n');
    (pre, blobs)
}

/// Format typst compile diagnostics as `line:col: message` (semicolon-joined), resolving each span's
/// location against `world`. A diagnostic with no resolvable span falls back to just its message.
fn format_diagnostics(diags: &[SourceDiagnostic], world: &dyn World) -> String {
    let mut msgs = Vec::new();
    for d in diags {
        let loc = d
            .span
            .id()
            .and_then(|id| world.source(id).ok())
            .and_then(|src| {
                let range = src.range(d.span)?;
                src.lines().byte_to_line_column(range.start)
            })
            .map(|(line, col)| format!("{}:{}: ", line + 1, col + 1)) // 1-based for display
            .unwrap_or_default();
        msgs.push(format!("{loc}{}", d.message));
    }
    if msgs.is_empty() {
        "typst: compile failed".to_string()
    } else {
        msgs.join("; ")
    }
}

/// Compile a GFM document (preamble + translated body) to a typst paged document, styled per the
/// user's [`PreviewFonts`] choice.
fn compile_gfm(
    source: &str,
    fonts: &PreviewFonts,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
) -> Result<PagedDocument, String> {
    let (preamble, blobs) = gfm_preamble(fonts);
    // With a styler, the document's MDA colours/attributes are spliced into the body as Typst markup
    // so they print; without one, this is the plain GFM→Typst translation.
    let body = match styler {
        Some(s) => crate::gfm::to_typst_styled(source, s),
        None => crate::gfm::to_typst(source),
    };
    compile_markup(format!("{preamble}{body}\n"), blobs, doc_dir)
}

/// Compile an HTML document the same way: [`crate::html`] translates it to a typst body, which gets
/// the SAME preamble as GFM — the document's own `style`/`class` are ignored (a structural
/// conversion), so the user's [`PreviewFonts`] choice governs its appearance just as it does for
/// Markdown.
fn compile_html(source: &str, fonts: &PreviewFonts, doc_dir: &Path) -> Result<PagedDocument, String> {
    let (preamble, blobs) = gfm_preamble(fonts);
    compile_markup(format!("{preamble}{}\n", crate::html::to_typst(source)), blobs, doc_dir)
}

/// Compile a document whose source IS typst markup (authored typst, not GFM): (near-)verbatim, no
/// preamble. The document controls its own fonts via `#set text(font: …)`, so the global `PreviewFonts`
/// setting doesn't apply — but we DO resolve the families it names from the system (on top of the
/// bundled set), so `#set text(font: "Georgia")` works if Georgia is installed, like the Typst CLI.
/// With a `styler`, the document's MDA colours/attributes are spliced into the markup (only around
/// prose — see [`crate::gfm::Styler::splice_typst_source`]) so the on-screen styling prints; without
/// one, the source compiles unchanged. Fonts are scanned from the ORIGINAL source (the splice adds no
/// `font:` references), and the splice adds no newlines, so compile-error line numbers still map to the
/// user's buffer.
fn compile_typst(
    source: &str,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
) -> Result<PagedDocument, String> {
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for fam in fonts_referenced(source) {
        blobs.extend(family_font_bytes(&fam));
    }
    let markup = match styler {
        Some(s) => s.splice_typst_source(source),
        None => source.to_string(),
    };
    compile_markup(markup, blobs, doc_dir)
}

/// A compiled document → PDF bytes.
fn to_pdf_bytes(doc: &PagedDocument) -> Result<Vec<u8>, String> {
    typst_pdf::pdf(doc, &typst_pdf::PdfOptions::default()).map_err(|diags| {
        diags.iter().map(|d| d.message.to_string()).collect::<Vec<_>>().join("; ")
    })
}

/// A compiled document's **pages, stacked top to bottom** into one RGBA image `(rgba, width, height)`
/// at `scale` (pixels per typst point), with a small gap between pages — so the live preview shows the
/// whole document (scrollable), not just page one. Page widths are left-aligned in the max width.
fn to_raster(doc: &PagedDocument, scale: f32) -> Result<(Vec<u8>, u32, u32), String> {
    if doc.pages.is_empty() {
        return Err("no pages".to_string());
    }
    let scale = scale.max(0.05);
    const GAP: u32 = 8; // transparent vertical px between pages
    let pms: Vec<_> = doc.pages.iter().map(|p| typst_render::render(p, scale)).collect();
    let width = pms.iter().map(|pm| pm.width()).max().unwrap_or(1).max(1);
    let total_h =
        pms.iter().map(|pm| pm.height()).sum::<u32>() + GAP * (pms.len() as u32 - 1);
    // Single page → identical to the old behavior (no gap added).
    let mut out = vec![0u8; width as usize * total_h as usize * 4];
    let mut y = 0u32;
    for pm in &pms {
        let (pw, ph, data) = (pm.width(), pm.height(), pm.data());
        for row in 0..ph as usize {
            let src = row * pw as usize * 4;
            let dst = (y as usize + row) * width as usize * 4;
            out[dst..dst + pw as usize * 4].copy_from_slice(&data[src..src + pw as usize * 4]);
        }
        y += ph + GAP;
    }
    Ok((out, width, total_h))
}

/// Render a GFM document to PDF bytes, styled per `fonts`. Errors (compile/layout failures) are
/// returned as a message so the caller can surface them trappably.
pub fn gfm_to_pdf(
    source: &str,
    doc_dir: &Path,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_gfm(source, fonts, doc_dir, styler)?)
}

/// Render a GFM document (all pages, stacked) to a raster RGBA image (the GUI live-preview path, 8d),
/// styled per `fonts`. `doc_dir` roots `#image`/`#read` file resolution. `styler` splices the
/// document's MDA styling into the output (`None` = plain).
pub fn gfm_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_gfm(source, fonts, doc_dir, styler)?, scale)
}

/// Render an HTML document to PDF bytes, styled per `fonts` (structural conversion — see
/// [`crate::html`]). Errors are returned as a message so the caller can surface them trappably.
pub fn html_to_pdf(source: &str, doc_dir: &Path, fonts: &PreviewFonts) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_html(source, fonts, doc_dir)?)
}

/// Render an HTML document (all pages, stacked) to a raster RGBA image — the GUI live-preview path.
pub fn html_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    fonts: &PreviewFonts,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_html(source, fonts, doc_dir)?, scale)
}

/// Render an authored-typst document (the buffer is typst markup) to PDF bytes. `doc_dir` roots
/// `#image`/`#include`/`#read` file resolution (relative to it, or its drive for absolute paths).
/// `styler` splices the document's MDA styling into the output (`None` = compile the source verbatim).
pub fn typst_to_pdf(
    source: &str,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_typst(source, doc_dir, styler)?)
}

/// Render an authored-typst document (all pages, stacked) to a raster RGBA image. `styler` splices the
/// document's MDA styling into the output (`None` = plain).
pub fn typst_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    styler: Option<&crate::gfm::Styler>,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_typst(source, doc_dir, styler)?, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GFM tests use Typst's default fonts unless a case says otherwise.
    fn dflt() -> PreviewFonts {
        PreviewFonts::default()
    }

    /// Doc directory for tests that don't touch `#image`/`#read` (no file resolution needed).
    fn dot() -> &'static Path {
        Path::new(".")
    }

    #[test]
    fn renders_gfm_to_a_valid_pdf() {
        let md = "# Title\n\nA paragraph with **bold**, `code`, and a [link](http://x).\n\n\
                  - one\n- two\n\n```rust\nfn main() {}\n```\n";
        let pdf = gfm_to_pdf(md, dot(), &dflt(), None).expect("should produce a PDF");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
        assert!(pdf.ends_with(b"%%EOF\n") || pdf.ends_with(b"%%EOF"), "missing EOF marker");
    }

    #[test]
    fn tricky_metacharacters_do_not_break_compilation() {
        // The escaping in `gfm.rs` must keep `#`/`$`/etc. from injecting typst.
        let md = "Loose # hash, $ dollar, * star, <angle>, @at, and #raw inside text.\n";
        let pdf = gfm_to_pdf(md, dot(), &dflt(), None).expect("escaping should keep typst happy");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn empty_document_still_compiles() {
        let pdf = gfm_to_pdf("", dot(), &dflt(), None).expect("empty doc");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn mda_styled_gfm_compiles_to_a_valid_pdf() {
        // The spliced MDA styling (attributes + colours + reverse) must produce Typst the engine
        // accepts — end-to-end proof the pre-pass output compiles.
        use xyscript_xpl::{ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};
        let md = "The quick brown fox jumps over the lazy dog.\n";
        let styler = crate::gfm::Styler::new(
            md,
            vec![(4, 9, (200, 0, 0))],                    // "quick" red
            vec![(16, 19, (255, 255, 0))],                // "fox" yellow background
            vec![(10, 15, ATTR_BOLD | ATTR_UNDERLINE), (40, 43, ATTR_REVERSE)], // "brown" bold+ul, "dog" reverse
            (0, 0, 0),
            (255, 255, 255),
            true,
        );
        let pdf = gfm_to_pdf(md, dot(), &dflt(), Some(&styler)).expect("styled GFM should compile");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
    }

    #[test]
    fn mda_styled_authored_typst_compiles_to_a_valid_pdf() {
        // MDA styling spliced into an AUTHORED-Typst document (around the prose, leaving the document's
        // own `#strong[…]` / math intact) must still produce Typst the engine accepts — both ULSPACES
        // modes. Offsets into "= Title\n\nHello there world done.\n": "Hello" [9,14), "there" [15,20),
        // "world" [21,26).
        use xyscript_xpl::{ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};
        let typ = "= Title\n\nHello there world done. #strong[keep] $x^2$\n";
        for ul_spaces in [true, false] {
            let styler = crate::gfm::Styler::new(
                typ,
                vec![(9, 14, (0, 128, 0))],   // "Hello" green
                vec![],
                vec![(15, 20, ATTR_UNDERLINE), (21, 26, ATTR_BOLD | ATTR_REVERSE)], // "there" ul, "world" bold+rev
                (0, 0, 0),
                (255, 255, 255),
                ul_spaces,
            );
            let pdf =
                typst_to_pdf(typ, dot(), Some(&styler)).expect("styled authored typst should compile");
            assert!(pdf.starts_with(b"%PDF-"), "missing PDF header (ul_spaces={ul_spaces})");
            assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
        }
    }

    #[test]
    fn preview_fonts_inject_the_expected_typst_rules() {
        // Family only.
        let (p, _) = gfm_preamble(&PreviewFonts { family: Some("Georgia".into()), ..dflt() });
        assert!(p.contains("#set text(font: \"Georgia\")"), "{p}");
        // Family + size.
        let (p, _) = gfm_preamble(&PreviewFonts {
            family: Some("Georgia".into()),
            size: Some(13.0),
            code: None,
            ..dflt()
        });
        assert!(p.contains("#set text(font: \"Georgia\", size: 13pt)"), "{p}");
        // Size only.
        let (p, _) = gfm_preamble(&PreviewFonts { size: Some(12.0), ..dflt() });
        assert!(p.contains("#set text(size: 12pt)"), "{p}");
        // Code font → a raw show-rule.
        let (p, _) = gfm_preamble(&PreviewFonts { code: Some("Fira Code".into()), ..dflt() });
        assert!(p.contains("#show raw: set text(font: \"Fira Code\")"), "{p}");
        // Default = no text/raw overrides at all.
        let (p, blobs) = gfm_preamble(&dflt());
        assert!(!p.contains("#set text(font"), "{p}");
        assert!(!p.contains("#show raw"), "{p}");
        assert!(blobs.is_empty());
        // A quote in a family name is escaped, not injected.
        let (p, _) = gfm_preamble(&PreviewFonts { family: Some("A\"B".into()), ..dflt() });
        assert!(p.contains("\\\""), "quote should be escaped: {p}");
    }

    #[test]
    fn page_margins_emit_the_set_page_rule() {
        // Default = 1 inch all around.
        let (p, _) = gfm_preamble(&dflt());
        assert!(
            p.contains("#set page(margin: (left: 1in, right: 1in, top: 1in, bottom: 1in))"),
            "{p}"
        );
        // Custom per-side.
        let fonts = PreviewFonts {
            margins: PageMargins {
                left: "2cm".into(),
                right: "2cm".into(),
                top: "1.5in".into(),
                bottom: "18pt".into(),
            },
            ..dflt()
        };
        let (p, _) = gfm_preamble(&fonts);
        assert!(
            p.contains("#set page(margin: (left: 2cm, right: 2cm, top: 1.5in, bottom: 18pt))"),
            "{p}"
        );
    }

    #[test]
    fn normalize_length_parses_units_and_rejects_junk() {
        assert_eq!(normalize_length("1"), Ok("1in".into())); // bare number = inches
        assert_eq!(normalize_length("0.75"), Ok("0.75in".into()));
        assert_eq!(normalize_length("2.5cm"), Ok("2.5cm".into()));
        assert_eq!(normalize_length(" 18 pt "), Ok("18pt".into()));
        assert_eq!(normalize_length("20mm"), Ok("20mm".into()));
        assert_eq!(normalize_length("1in"), Ok("1in".into()));
        assert_eq!(normalize_length("1.0in"), Ok("1in".into())); // trailing .0 trimmed
        assert!(normalize_length("").is_err());
        assert!(normalize_length("-1in").is_err()); // negative
        assert!(normalize_length("2 furlongs").is_err()); // unknown unit
        assert!(normalize_length("wide").is_err()); // no number
        // Injection attempt: a `)` can't smuggle markup — it fails the number/unit parse.
        assert!(normalize_length("1in) #import evil").is_err());
    }

    #[test]
    fn gfm_with_a_missing_family_still_renders() {
        // A family that isn't installed contributes no bytes and Typst falls back — no error.
        let fonts = PreviewFonts {
            family: Some("Definitely Not An Installed Font 9000".into()),
            size: Some(12.0),
            code: Some("Also Not Installed 9000".into()),
            ..dflt()
        };
        let pdf = gfm_to_pdf("# Hi\n\nbody `code`\n", dot(), &fonts, None).expect("still compiles");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn renders_gfm_to_a_raster_image() {
        let (rgba, w, h) =
            gfm_to_image("# Title\n\nbody **text**\n", dot(), 1.0, &dflt(), None).expect("image");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize); // RGBA
        assert!(rgba.iter().any(|&b| b != 0)); // not all-transparent/black
    }

    #[test]
    fn raster_stacks_all_pages_not_just_the_first() {
        // One page vs an explicit two-page document (forced via #pagebreak): the preview must show
        // BOTH pages, so the two-page raster is taller than the one-page one (was first-page-only).
        let (_, w1, h1) = typst_to_image("Page one.", dot(), 1.0, None).expect("one page");
        let (rgba, w2, h2) =
            typst_to_image("Page one.#pagebreak()Page two.", dot(), 1.0, None).expect("two pages");
        assert_eq!(w1, w2, "same page width");
        assert!(h2 > h1 + h1 / 2, "two pages ({h2}) should be ~twice one page ({h1})");
        assert_eq!(rgba.len(), (w2 * h2 * 4) as usize);
    }

    // ---- Typst-as-input (Phase 0): the buffer IS typst markup -----------------

    #[test]
    fn renders_authored_typst_to_a_valid_pdf() {
        // Real typst features that GFM can't express: a #set rule, math, a #let function, a table.
        let typ = "\
#set page(margin: 1in)
#let note(b) = block(fill: luma(230), inset: 6pt)[*Note:* #b]
= Heading
Body with $a^2 + b^2 = c^2$ and #strong[bold].
#note[a callout]
#table(columns: 2, [a], [b], [c], [d])
";
        let pdf = typst_to_pdf(typ, dot(), None).expect("authored typst should compile");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
    }

    #[test]
    fn typst_compile_error_is_reported() {
        // An unknown function is a typst compile error → Err (not a panic).
        let err = typst_to_pdf("#this_function_does_not_exist()", dot(), None).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn typst_compile_error_reports_line_and_column() {
        // The error is on line 3 (`#zzznope()`). Because Typst-as-input is compiled verbatim (no
        // preamble), the reported line maps to the user's buffer.
        let typ = "= Title\n\n#zzznope()\n";
        let err = typst_to_pdf(typ, dot(), None).unwrap_err();
        assert!(err.contains("3:"), "want line 3 in: {err}");
        assert!(err.contains("zzznope"), "want the offending name in: {err}");
    }

    #[test]
    fn typst_font_references_are_extracted() {
        assert_eq!(fonts_referenced("#set text(font: \"Georgia\")"), vec!["Georgia"]);
        // A fallback list yields each family, in order.
        assert_eq!(fonts_referenced("#set text(font: (\"A\", \"B\"))"), vec!["A", "B"]);
        // Inline #text(...) and a raw show-rule, no space after the colon.
        assert_eq!(
            fonts_referenced("#text(font:\"C\")[x]\n#show raw: set text(font: \"D\")"),
            vec!["C", "D"]
        );
        // No reference → nothing; case-insensitive de-dup.
        assert!(fonts_referenced("plain body, no fonts").is_empty());
        assert_eq!(fonts_referenced("font: \"X\" ... font: \"x\""), vec!["X"]);
    }

    #[test]
    fn authored_typst_referencing_a_font_still_compiles() {
        // Whether or not the named family is installed, naming it must not break compilation
        // (Typst falls back when it's absent).
        let typ = "#set text(font: \"Definitely Not Installed 9000\")\n= Hi\n\nbody text\n";
        assert!(typst_to_pdf(typ, dot(), None).expect("compiles").starts_with(b"%PDF-"));
    }

    #[test]
    fn authored_typst_is_not_gfm_translated() {
        // `= Heading` is a typst heading verbatim; GFM translation would instead have mangled
        // markdown. A bare markdown `# Heading` would be a typst comment-ish/error, proving no
        // GFM step runs: this typst-native doc compiles, and the empty doc does too.
        assert!(typst_to_pdf("= Just a typst heading\n", dot(), None).is_ok());
        assert!(typst_to_pdf("", dot(), None).expect("empty typst").starts_with(b"%PDF-"));
    }

    #[test]
    fn renders_authored_typst_to_a_raster_image() {
        let (rgba, w, h) = typst_to_image("= Title\n\nbody *text*\n", dot(), 1.0, None).expect("image");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.iter().any(|&b| b != 0));
    }

    #[test]
    fn image_resolves_relative_to_the_document_dir() {
        // `#image("logo.png")` must find a file next to the document — the reported "file not found
        // even though the file is there" was the engine having NO filesystem resolver at all.
        let dir = std::env::temp_dir().join(format!("xy_typst_img_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("logo.png"), b"not a real png").unwrap();
        // The file IS found+read (so the failure is an image-DECODE error, not "file not found").
        let err = typst_to_pdf(r#"#image("logo.png")"#, &dir, None).unwrap_err();
        assert!(!err.contains("file not found"), "resolver should find the file: {err}");
        // A genuinely-missing file still reports not-found.
        let miss = typst_to_pdf(r#"#image("nope.png")"#, &dir, None).unwrap_err();
        assert!(miss.contains("not found"), "{miss}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn absolute_image_path_resolves_via_the_drive_root() {
        // An ABSOLUTE path (Typst strips its drive) resolves against the document's drive root — even
        // when the document lives in a DIFFERENT directory on that drive.
        let dir = std::env::temp_dir().join(format!("xy_typst_abs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("pic.png");
        std::fs::write(&img, b"bytes").unwrap();
        // Forward slashes — backslashes are string escapes in Typst.
        let src = format!("#image(\"{}\")", img.to_string_lossy().replace('\\', "/"));
        let err = typst_to_pdf(&src, std::env::temp_dir().as_path(), None).unwrap_err();
        assert!(!err.contains("file not found"), "absolute path should resolve: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
