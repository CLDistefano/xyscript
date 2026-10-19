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
#[derive(Clone)]
pub struct PreviewFonts {
    /// Body typeface family name (`#set text(font: …)`); `None` = Typst default.
    pub family: Option<String>,
    /// Body size in points (`#set text(size: …)`); `None` = Typst default.
    pub size: Option<f32>,
    /// Monospace family for `code` spans / fenced blocks (`#show raw: set text(font: …)`).
    pub code: Option<String>,
    /// Code (monospace) size in points (`#show raw: set text(size: …)`). `None` = code inherits the
    /// body size (`size` above, or Typst's default) — the historical behaviour; `Some` pins it so code
    /// can be a different size from the body.
    pub code_size: Option<f32>,
    /// Page margins (`#set page(margin: …)`). Each side is a validated Typst length string (see
    /// [`normalize_length`]). Defaults to 1 inch all around.
    pub margins: PageMargins,
    /// Page format as a friendly token (`letter`/`legal`/`tabloid`/`a4`/`a5`/`a6`), mapped to a Typst
    /// `paper` name in [`gfm_preamble`]. Default `letter`.
    pub paper: String,
    /// Line spacing — `#set par(leading: …)`. `None` = Typst's default (0.65em).
    pub leading: Option<String>,
    /// Extra space between paragraphs — `#set par(spacing: …)`. `None` = Typst's default (1.2em).
    pub spacing: Option<String>,
    /// Page-number pattern (`#set page(numbering: …)`) — e.g. `1`, `— 1 —`, `Page 1`, `1 of 1`.
    /// `None` = no page numbers.
    pub page_num: Option<String>,
    /// Page-number position as a friendly token (`bottom-center`, `top-right`, …), mapped to a Typst
    /// alignment in [`gfm_preamble`]. Only used when `page_num` is `Some`.
    pub page_align: String,
    /// Justify body text — `#set par(justify: …)`. Default `true`.
    pub justify: bool,
    /// First-line paragraph indent — `#set par(first-line-indent: …)`. `None` = none.
    pub indent: Option<String>,
    /// Optional page header / footer templates — `#set page(header/footer: …)`. `|`-separated
    /// left/center/right segments; the tokens `{date}` `{file}` `{page}` `{pages}` expand. `None` = none.
    pub header: Option<String>,
    pub footer: Option<String>,
    /// The active document's display name, for the `{file}` token in a header/footer. `None` = untitled.
    pub doc_name: Option<String>,
}

impl Default for PreviewFonts {
    fn default() -> Self {
        PreviewFonts {
            family: None,
            size: None,
            code: None,
            code_size: None,
            margins: PageMargins::default(),
            paper: "letter".to_string(),
            leading: None,
            spacing: None,
            page_num: None,
            page_align: "bottom-center".to_string(),
            justify: true,
            indent: None,
            header: None,
            footer: None,
            doc_name: None,
        }
    }
}

/// Preview-only page/body tint (RGB), applied ONLY on the raster (live-preview) path — `TOPDF`/print
/// output is never tinted (its `*_to_pdf` functions pass [`PreviewTint::default`]). Each `None` field
/// keeps Typst's default (white paper / black body text). `PVBGCOLOR` sets `bg`, `PVFGCOLOR` sets `fg`.
///
/// Implemented as a `#set page(fill: …)` / `#set text(fill: …)` prelude prepended to the compiled
/// markup, so it recolours the paper and default body text while the document's own explicit colours
/// (links, `#text(fill:)` spans, MDA styling) still win. Because it can add lines ahead of authored
/// Typst, the prelude's line count is subtracted back out of compile diagnostics (see
/// [`compile_markup`]) so error locations still map to the user's buffer.
#[derive(Clone, Copy, Default)]
pub struct PreviewTint {
    /// Page (paper) fill — `#set page(fill: …)`. `None` = Typst default (white).
    pub bg: Option<(u8, u8, u8)>,
    /// Default body text colour — `#set text(fill: …)`. `None` = Typst default (black).
    pub fg: Option<(u8, u8, u8)>,
}

impl PreviewTint {
    /// The `#set` prelude (one rule per set field, each newline-terminated), or `""` when nothing is set.
    fn prelude(&self) -> String {
        let mut s = String::new();
        if let Some((r, g, b)) = self.bg {
            s.push_str(&format!("#set page(fill: rgb({r}, {g}, {b}))\n"));
        }
        if let Some((r, g, b)) = self.fg {
            s.push_str(&format!("#set text(fill: rgb({r}, {g}, {b}))\n"));
        }
        s
    }

    /// Number of source lines [`Self::prelude`] prepends — subtracted from diagnostic line numbers so
    /// they still point at the user's buffer.
    fn line_count(&self) -> usize {
        self.bg.is_some() as usize + self.fg.is_some() as usize
    }
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

/// Validate + canonicalize a paper name to a friendly token (`letter`/`legal`/`tabloid`/`a4`/`a5`/`a6`).
/// `Err(msg)` for anything else. [`paper_typst`] maps the token to the Typst `paper:` name.
pub fn canon_paper(s: &str) -> Result<String, String> {
    let t = s.trim().to_ascii_lowercase();
    let canon = match t.as_str() {
        "letter" | "us-letter" | "usletter" => "letter",
        "legal" | "us-legal" | "uslegal" => "legal",
        "tabloid" | "us-tabloid" | "ledger" => "tabloid",
        "a4" => "a4",
        "a5" => "a5",
        "a6" => "a6",
        _ => return Err(format!("unknown paper {s:?} (use letter, legal, tabloid, a4, a5, or a6)")),
    };
    Ok(canon.to_string())
}

/// Map a friendly paper token (from [`canon_paper`]) to the Typst `paper:` name.
fn paper_typst(friendly: &str) -> &'static str {
    match friendly {
        "legal" => "us-legal",
        "tabloid" => "us-tabloid",
        "a4" => "a4",
        "a5" => "a5",
        "a6" => "a6",
        _ => "us-letter", // "letter" and any unexpected value fall back to Letter
    }
}

/// Validate + normalize a paragraph length (line spacing / paragraph spacing): a non-negative number
/// with an optional unit — `em` (the default for a bare number), `pt`, `cm`, `mm`, or `in`. Returns a
/// clean `"<n><unit>"` safe to embed, or `Err(msg)`.
pub fn normalize_par_len(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("expected a length like 1.5em, 12pt, 0.4cm (bare number = em)".into());
    }
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num_str, unit_str) = t.split_at(split);
    let num: f64 = num_str.trim().parse().map_err(|_| format!("bad number in {t:?}"))?;
    if !num.is_finite() || num < 0.0 {
        return Err(format!("length must be ≥ 0 (got {t:?})"));
    }
    let unit = match unit_str.trim().to_ascii_lowercase().as_str() {
        "" | "em" => "em",
        "pt" | "point" | "points" => "pt",
        "cm" => "cm",
        "mm" => "mm",
        "in" | "inch" | "inches" | "\"" => "in",
        other => return Err(format!("unknown unit {other:?} (use em, pt, cm, mm, or in)")),
    };
    let n = if num.fract() == 0.0 { format!("{}", num as i64) } else { format!("{num}") };
    Ok(format!("{n}{unit}"))
}

/// Validate + canonicalize a page-number position to a friendly `"<vertical>-<horizontal>"` token,
/// e.g. `bottom-center`, `top-right`. Accepts words or initials in either order (`br`, `bottom right`,
/// `right-bottom`, `center`, …); missing halves default to `bottom` / `center`. [`page_align_typst`]
/// maps it to a Typst alignment.
pub fn canon_page_align(s: &str) -> Result<String, String> {
    // Classify one token as a vertical and/or horizontal choice.
    fn axis(tok: &str) -> Result<(Option<&'static str>, Option<&'static str>), String> {
        Ok(match tok {
            "top" | "t" => (Some("top"), None),
            "bottom" | "bot" | "b" => (Some("bottom"), None),
            "left" | "l" => (None, Some("left")),
            "right" | "r" => (None, Some("right")),
            "center" | "centre" | "c" | "middle" | "m" => (None, Some("center")),
            other => {
                return Err(format!(
                    "unknown position {other:?} (use top/bottom + left/center/right)"
                ))
            }
        })
    }
    let (mut vert, mut horiz) = (None, None);
    let lower = s.trim().to_ascii_lowercase();
    for tok in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        // A 2-letter combo like "br"/"tl" is a vertical+horizontal pair; else a single word/initial.
        let parts: Vec<&str> = if tok.len() == 2
            && matches!(&tok[0..1], "t" | "b")
            && matches!(&tok[1..2], "l" | "c" | "r")
        {
            vec![&tok[0..1], &tok[1..2]]
        } else {
            vec![tok]
        };
        for p in parts {
            let (v, h) = axis(p)?;
            if v.is_some() {
                vert = v;
            }
            if h.is_some() {
                horiz = h;
            }
        }
    }
    Ok(format!("{}-{}", vert.unwrap_or("bottom"), horiz.unwrap_or("center")))
}

/// Map a friendly page-align token (from [`canon_page_align`]) to a Typst `number-align` value
/// (`horizontal + vertical`).
fn page_align_typst(friendly: &str) -> String {
    let (vert, horiz) = friendly.split_once('-').unwrap_or(("bottom", "center"));
    format!("{horiz} + {vert}")
}

/// Render a header/footer template to Typst header/footer CONTENT (goes inside `[…]`). `|` splits it
/// into left/center/right segments joined by `#h(1fr)`; the tokens `{date}`/`{file}`/`{page}`/`{pages}`
/// expand to Typst code, and all literal text is escaped so it can never inject markup. `{file}` uses
/// `doc_name` (else "untitled"). An unknown `{token}` is left as literal text.
fn render_header_footer(template: &str, doc_name: Option<&str>) -> String {
    template
        .split('|')
        .map(|seg| render_hf_segment(seg, doc_name))
        .collect::<Vec<_>>()
        .join(" #h(1fr) ")
}

fn render_hf_segment(seg: &str, doc_name: Option<&str>) -> String {
    use crate::typst_esc::escape_text;
    let mut out = String::new();
    let mut rest = seg;
    while let Some(open) = rest.find('{') {
        out.push_str(&escape_text(&rest[..open]));
        rest = &rest[open..];
        if let Some(close) = rest.find('}') {
            let tok = rest[1..close].trim().to_ascii_lowercase();
            match tok.as_str() {
                "date" => out.push_str(
                    "#datetime.today().display(\"[month repr:long] [day padding:none], [year]\")",
                ),
                "file" => out.push_str(&escape_text(doc_name.unwrap_or("untitled"))),
                "page" => out.push_str("#context here().page()"),
                "pages" => out.push_str("#context counter(page).final().first()"),
                _ => out.push_str(&escape_text(&rest[..=close])), // unknown → keep literal "{…}"
            }
            rest = &rest[close + 1..];
        } else {
            // Unterminated "{" — emit the remainder literally.
            out.push_str(&escape_text(rest));
            rest = "";
        }
    }
    out.push_str(&escape_text(rest));
    out
}

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
    diag_line_offset: usize,
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
            output.map_err(|diags| format_diagnostics(&diags, world, diag_line_offset))
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
    // `#set page(...)`: paper, margins (each side a pre-validated length), and optional page numbering.
    let mut page = format!(
        "paper: \"{}\", margin: (left: {}, right: {}, top: {}, bottom: {})",
        paper_typst(&fonts.paper),
        m.left,
        m.right,
        m.top,
        m.bottom
    );
    if let Some(pat) = &fonts.page_num {
        page.push_str(&format!(
            ", numbering: \"{}\", number-align: {}",
            typst_str(pat),
            page_align_typst(&fonts.page_align)
        ));
    }
    // Optional page header / footer (a custom footer supersedes the auto page number at that edge —
    // put a {page} token in it to keep the number).
    if let Some(h) = &fonts.header {
        page.push_str(&format!(", header: [{}]", render_header_footer(h, fonts.doc_name.as_deref())));
    }
    if let Some(f) = &fonts.footer {
        page.push_str(&format!(", footer: [{}]", render_header_footer(f, fonts.doc_name.as_deref())));
    }
    let mut pre = format!("#set page({page})\n");
    // `#set par(...)`: justify, optional first-line indent, and the line-/paragraph-spacing overrides.
    let mut par = format!("justify: {}", fonts.justify);
    if let Some(ind) = &fonts.indent {
        par.push_str(&format!(", first-line-indent: {ind}"));
    }
    if let Some(l) = &fonts.leading {
        par.push_str(&format!(", leading: {l}"));
    }
    if let Some(sp) = &fonts.spacing {
        par.push_str(&format!(", spacing: {sp}"));
    }
    pre.push_str(&format!("#set par({par})\n"));
    pre.push_str("#show link: set text(fill: blue)\n");
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
    // `#show raw: set text(...)` — the code font family and/or an explicit code size. When no size is
    // set, code keeps inheriting the body size (`#set text(size:)` above); a size here pins it so code
    // can differ from the body.
    match (&fonts.code, fonts.code_size) {
        (Some(c), Some(sz)) => {
            pre.push_str(&format!("#show raw: set text(font: \"{}\", size: {sz}pt)\n", typst_str(c)))
        }
        (Some(c), None) => {
            pre.push_str(&format!("#show raw: set text(font: \"{}\")\n", typst_str(c)))
        }
        (None, Some(sz)) => pre.push_str(&format!("#show raw: set text(size: {sz}pt)\n")),
        (None, None) => {}
    }
    if let Some(code) = &fonts.code {
        blobs.extend(family_font_bytes(code));
    }
    pre.push('\n');
    (pre, blobs)
}

/// Format typst compile diagnostics as `line:col: message` (semicolon-joined), resolving each span's
/// location against `world`. A diagnostic with no resolvable span falls back to just its message.
fn format_diagnostics(diags: &[SourceDiagnostic], world: &dyn World, line_offset: usize) -> String {
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
            // 1-based for display, minus any preview-tint prelude prepended ahead of the user's buffer.
            .map(|(line, col)| format!("{}:{}: ", (line + 1).saturating_sub(line_offset), col + 1))
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
    tint: PreviewTint,
) -> Result<PagedDocument, String> {
    let (preamble, blobs) = gfm_preamble(fonts);
    // With a styler, the document's MDA colours/attributes are spliced into the body as Typst markup
    // so they print; without one, this is the plain GFM→Typst translation.
    let body = match styler {
        Some(s) => crate::gfm::to_typst_styled(source, s),
        None => crate::gfm::to_typst(source),
    };
    // GFM diagnostics already reference the generated typst (not the user's markdown), so no offset.
    compile_markup(format!("{}{preamble}{body}\n", tint.prelude()), blobs, doc_dir, 0)
}

/// Compile a PLAIN-TEXT document: the buffer rendered **verbatim** — every source newline becomes a
/// hard line break and no Markdown/Typst syntax is interpreted (`#`, `*`, `=` … print literally) — with
/// the SAME preamble (page setup + fonts) as GFM. This is the auto default for non-.md text files and
/// the target of `DOCTYPE text`. See [`plain_text_body`] for the verbatim transform.
fn compile_text(
    source: &str,
    fonts: &PreviewFonts,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
    tint: PreviewTint,
) -> Result<PagedDocument, String> {
    let (preamble, blobs) = gfm_preamble(fonts);
    // With a styler the document's MDA styling (and any EMPH/URL marks folded in) is spliced around
    // the verbatim text; without one, the plain escaped body.
    let body = match styler {
        Some(s) => s.plain_text_body(source),
        None => plain_text_body(source),
    };
    compile_markup(format!("{}{preamble}{body}\n", tint.prelude()), blobs, doc_dir, 0)
}

/// Turn raw text into a Typst body that renders VERBATIM. Every source line is emitted with all Typst
/// markup characters escaped (so nothing is interpreted), lines are joined with hard breaks (each source
/// newline → one line break; a run of blank lines survives as blank lines), and leading indentation is
/// preserved with non-breaking spaces because Typst trims leading whitespace (a tab ≈ 8). Long lines
/// still wrap at the page margin — only *leading* runs are made non-breaking; interior space runs keep
/// Typst's normal wrapping/collapse.
fn plain_text_body(src: &str) -> String {
    let mut out = String::new();
    for (i, raw) in src.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw); // tolerate CRLF buffers
        if i > 0 {
            // Trailing space is required: `#linebreak()` immediately followed by `(`/`[`/`{` parses as a
            // chained call (see gfm.rs LineBreak).
            out.push_str("#linebreak() ");
        }
        let indent_end = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
        for c in line[..indent_end].chars() {
            out.push_str(if c == '\t' { "~~~~~~~~" } else { "~" }); // ~ = Typst non-breaking space
        }
        out.push_str(&crate::typst_esc::escape_text(&line[indent_end..]));
    }
    out
}

/// Compile an HTML document the same way: [`crate::html`] translates it to a typst body, which gets
/// the SAME preamble as GFM — the document's own `style`/`class` are ignored (a structural
/// conversion), so the user's [`PreviewFonts`] choice governs its appearance just as it does for
/// Markdown.
fn compile_html(
    source: &str,
    fonts: &PreviewFonts,
    doc_dir: &Path,
    tint: PreviewTint,
) -> Result<PagedDocument, String> {
    let (preamble, blobs) = gfm_preamble(fonts);
    compile_markup(
        format!("{}{preamble}{}\n", tint.prelude(), crate::html::to_typst(source)),
        blobs,
        doc_dir,
        0,
    )
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
    tint: PreviewTint,
) -> Result<PagedDocument, String> {
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for fam in fonts_referenced(source) {
        blobs.extend(family_font_bytes(&fam));
    }
    let markup = match styler {
        Some(s) => s.splice_typst_source(source),
        None => source.to_string(),
    };
    // The tint prelude (if any) is prepended; its line count is subtracted back out of diagnostics so
    // error locations still map to the user's buffer. `set` rules for fill are cumulative, so the
    // document's own `#set page`/`#set text` (font, size, margins) keep our fill unless they set one too.
    compile_markup(format!("{}{markup}", tint.prelude()), blobs, doc_dir, tint.line_count())
}

/// A compiled document → PDF bytes.
fn to_pdf_bytes(doc: &PagedDocument) -> Result<Vec<u8>, String> {
    typst_pdf::pdf(doc, &typst_pdf::PdfOptions::default()).map_err(|diags| {
        diags.iter().map(|d| d.message.to_string()).collect::<Vec<_>>().join("; ")
    })
}

/// A compiled document → a single merged SVG (all pages stacked top-to-bottom with a small gap),
/// as UTF-8 bytes. Unlike PDF export, the SVG exporter embeds colour-glyph layers, so a colour emoji
/// from a registered colour font comes through in colour where PDF export flattens or drops it.
fn to_svg_bytes(doc: &PagedDocument) -> Result<Vec<u8>, String> {
    let gap = typst::layout::Abs::pt(8.0); // same visual page gap as the raster preview
    Ok(typst_svg::svg_merged(doc, gap).into_bytes())
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
    to_pdf_bytes(&compile_gfm(source, fonts, doc_dir, styler, PreviewTint::default())?)
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
    tint: PreviewTint,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_gfm(source, fonts, doc_dir, styler, tint)?, scale)
}

/// Render a PLAIN-TEXT document to PDF bytes (verbatim, no Markdown interpretation), styled per `fonts`.
/// `styler` splices the document's MDA styling (and any EMPH/URL marks) into the output (`None` = plain).
/// Errors are returned as a message so the caller can surface them trappably.
pub fn text_to_pdf(
    source: &str,
    doc_dir: &Path,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_text(source, fonts, doc_dir, styler, PreviewTint::default())?)
}

/// Render a PLAIN-TEXT document (all pages, stacked) to a raster RGBA image — the GUI live-preview path.
/// `styler` splices the document's MDA styling (and any EMPH/URL marks) into the output (`None` = plain).
pub fn text_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
    tint: PreviewTint,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_text(source, fonts, doc_dir, styler, tint)?, scale)
}

/// Render an HTML document to PDF bytes, styled per `fonts` (structural conversion — see
/// [`crate::html`]). Errors are returned as a message so the caller can surface them trappably.
pub fn html_to_pdf(source: &str, doc_dir: &Path, fonts: &PreviewFonts) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_html(source, fonts, doc_dir, PreviewTint::default())?)
}

/// Render an HTML document (all pages, stacked) to a raster RGBA image — the GUI live-preview path.
pub fn html_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    fonts: &PreviewFonts,
    tint: PreviewTint,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_html(source, fonts, doc_dir, tint)?, scale)
}

/// Render an authored-typst document (the buffer is typst markup) to PDF bytes. `doc_dir` roots
/// `#image`/`#include`/`#read` file resolution (relative to it, or its drive for absolute paths).
/// `styler` splices the document's MDA styling into the output (`None` = compile the source verbatim).
pub fn typst_to_pdf(
    source: &str,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_pdf_bytes(&compile_typst(source, doc_dir, styler, PreviewTint::default())?)
}

// ---- SVG export (TOSVG) — same four render kinds as the PDF export, merged into one SVG ----------

/// Render a GFM document to merged-SVG bytes, styled per `fonts`.
pub fn gfm_to_svg(
    source: &str,
    doc_dir: &Path,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_svg_bytes(&compile_gfm(source, fonts, doc_dir, styler, PreviewTint::default())?)
}

/// Render a PLAIN-TEXT document (verbatim) to merged-SVG bytes, styled per `fonts`.
pub fn text_to_svg(
    source: &str,
    doc_dir: &Path,
    fonts: &PreviewFonts,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_svg_bytes(&compile_text(source, fonts, doc_dir, styler, PreviewTint::default())?)
}

/// Render an HTML document (structural conversion) to merged-SVG bytes, styled per `fonts`.
pub fn html_to_svg(source: &str, doc_dir: &Path, fonts: &PreviewFonts) -> Result<Vec<u8>, String> {
    to_svg_bytes(&compile_html(source, fonts, doc_dir, PreviewTint::default())?)
}

/// Render an authored-typst document to merged-SVG bytes. `styler` splices MDA styling (`None` = plain).
pub fn typst_to_svg(
    source: &str,
    doc_dir: &Path,
    styler: Option<&crate::gfm::Styler>,
) -> Result<Vec<u8>, String> {
    to_svg_bytes(&compile_typst(source, doc_dir, styler, PreviewTint::default())?)
}

/// Render an authored-typst document (all pages, stacked) to a raster RGBA image. `styler` splices the
/// document's MDA styling into the output (`None` = plain).
pub fn typst_to_image(
    source: &str,
    doc_dir: &Path,
    scale: f32,
    styler: Option<&crate::gfm::Styler>,
    tint: PreviewTint,
) -> Result<(Vec<u8>, u32, u32), String> {
    to_raster(&compile_typst(source, doc_dir, styler, tint)?, scale)
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
    fn renders_gfm_to_a_valid_svg() {
        let md = "# Title\n\nA paragraph with **bold** and `code`.\n\n- one\n- two\n";
        let svg = gfm_to_svg(md, dot(), &dflt(), None).expect("should produce an SVG");
        let s = String::from_utf8(svg).expect("SVG is UTF-8");
        assert!(s.contains("<svg"), "no <svg> root: {}", &s[..s.len().min(120)]);
        assert!(s.contains("</svg>"), "SVG not closed");
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
        // Code SIZE (PVCODESIZE): with a family → font + size together.
        let (p, _) = gfm_preamble(&PreviewFonts {
            code: Some("Fira Code".into()),
            code_size: Some(9.0),
            ..dflt()
        });
        assert!(p.contains("#show raw: set text(font: \"Fira Code\", size: 9pt)"), "{p}");
        // Code size with NO code family → a size-only raw show-rule (code differs from body size).
        let (p, _) = gfm_preamble(&PreviewFonts { code_size: Some(8.5), ..dflt() });
        assert!(p.contains("#show raw: set text(size: 8.5pt)"), "{p}");
        assert!(!p.contains("font:"), "size-only code rule must not name a font: {p}");
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
        // Default = US Letter, 1 inch all around.
        let (p, _) = gfm_preamble(&dflt());
        assert!(
            p.contains(
                "#set page(paper: \"us-letter\", margin: (left: 1in, right: 1in, top: 1in, bottom: 1in))"
            ),
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
            p.contains(
                "#set page(paper: \"us-letter\", margin: (left: 2cm, right: 2cm, top: 1.5in, bottom: 18pt))"
            ),
            "{p}"
        );
    }

    #[test]
    fn paper_leading_spacing_and_page_numbers_emit_the_expected_rules() {
        // Paper mapping (friendly → Typst name).
        let (p, _) = gfm_preamble(&PreviewFonts { paper: "legal".into(), ..dflt() });
        assert!(p.contains("#set page(paper: \"us-legal\","), "{p}");
        let (p, _) = gfm_preamble(&PreviewFonts { paper: "tabloid".into(), ..dflt() });
        assert!(p.contains("paper: \"us-tabloid\""), "{p}");
        let (p, _) = gfm_preamble(&PreviewFonts { paper: "a4".into(), ..dflt() });
        assert!(p.contains("paper: \"a4\""), "{p}");
        // Line spacing + paragraph spacing fold into `#set par`.
        let (p, _) = gfm_preamble(&PreviewFonts {
            leading: Some("1.5em".into()),
            spacing: Some("2em".into()),
            ..dflt()
        });
        assert!(p.contains("#set par(justify: true, leading: 1.5em, spacing: 2em)"), "{p}");
        // Default = no leading/spacing overrides, no numbering.
        let (p, _) = gfm_preamble(&dflt());
        assert!(p.contains("#set par(justify: true)"), "{p}");
        assert!(!p.contains("numbering:"), "{p}");
        // Page numbers: pattern + position → numbering + number-align.
        let (p, _) = gfm_preamble(&PreviewFonts {
            page_num: Some("Page 1".into()),
            page_align: "top-right".into(),
            ..dflt()
        });
        assert!(p.contains("numbering: \"Page 1\", number-align: right + top"), "{p}");
        // A quote in the pattern is escaped, not injected.
        let (p, _) = gfm_preamble(&PreviewFonts { page_num: Some("a\"b".into()), ..dflt() });
        assert!(p.contains("\\\""), "pattern quote should be escaped: {p}");
    }

    #[test]
    fn paper_and_position_validators_canonicalize_and_reject() {
        assert_eq!(canon_paper("Letter").unwrap(), "letter");
        assert_eq!(canon_paper("us-legal").unwrap(), "legal");
        assert_eq!(canon_paper("A4").unwrap(), "a4");
        assert!(canon_paper("foolscap").is_err());
        assert_eq!(canon_page_align("br").unwrap(), "bottom-right");
        assert_eq!(canon_page_align("top center").unwrap(), "top-center");
        assert_eq!(canon_page_align("right").unwrap(), "bottom-right"); // vertical defaults to bottom
        assert!(canon_page_align("sideways").is_err());
        assert_eq!(normalize_par_len("1.5").unwrap(), "1.5em"); // bare → em
        assert_eq!(normalize_par_len("12pt").unwrap(), "12pt");
        assert!(normalize_par_len("1parsec").is_err());
    }

    #[test]
    fn justify_indent_and_header_footer_emit_the_expected_rules() {
        // Justify off + first-line indent fold into `#set par`.
        let (p, _) =
            gfm_preamble(&PreviewFonts { justify: false, indent: Some("1.5em".into()), ..dflt() });
        assert!(p.contains("#set par(justify: false, first-line-indent: 1.5em)"), "{p}");
        // Header: `|`-segments joined by #h(1fr), {file} filled, {date} → a datetime call.
        let (p, _) = gfm_preamble(&PreviewFonts {
            header: Some("{file}|Draft|{date}".into()),
            doc_name: Some("report.md".into()),
            ..dflt()
        });
        assert!(p.contains("header: [report.md #h(1fr) Draft #h(1fr) #datetime.today()"), "{p}");
        // Footer: {page}/{pages} → context page counters; a leading/trailing `|` centers via #h(1fr).
        let (p, _) = gfm_preamble(&PreviewFonts { footer: Some("|{page} of {pages}|".into()), ..dflt() });
        assert!(
            p.contains("footer: [ #h(1fr) #context here().page() of #context counter(page).final().first() #h(1fr) ]"),
            "{p}"
        );
        // A `#` in header text is escaped, not injected as code.
        let (p, _) = gfm_preamble(&PreviewFonts { header: Some("a #b".into()), ..dflt() });
        assert!(!p.contains("header: [a #b]"), "raw # should be escaped: {p}");
    }

    #[test]
    fn header_footer_tokens_compile_to_a_valid_pdf() {
        // End-to-end proof the {date}/{file}/{page}/{pages} Typst expands and compiles.
        let md = "# Title\n\nSome body text spread over a couple of lines to fill a page.\n";
        let fonts = PreviewFonts {
            header: Some("{file}|Draft|{date}".into()),
            footer: Some("|Page {page} of {pages}|".into()),
            doc_name: Some("report.md".into()),
            justify: false,
            indent: Some("1em".into()),
            ..dflt()
        };
        let pdf = gfm_to_pdf(md, dot(), &fonts, None).expect("header/footer GFM should compile");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
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
            gfm_to_image("# Title\n\nbody **text**\n", dot(), 1.0, &dflt(), None, PreviewTint::default())
                .expect("image");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize); // RGBA
        assert!(rgba.iter().any(|&b| b != 0)); // not all-transparent/black
    }

    #[test]
    fn raster_stacks_all_pages_not_just_the_first() {
        // One page vs an explicit two-page document (forced via #pagebreak): the preview must show
        // BOTH pages, so the two-page raster is taller than the one-page one (was first-page-only).
        let (_, w1, h1) =
            typst_to_image("Page one.", dot(), 1.0, None, PreviewTint::default()).expect("one page");
        let (rgba, w2, h2) =
            typst_to_image("Page one.#pagebreak()Page two.", dot(), 1.0, None, PreviewTint::default())
                .expect("two pages");
        assert_eq!(w1, w2, "same page width");
        assert!(h2 > h1 + h1 / 2, "two pages ({h2}) should be ~twice one page ({h1})");
        assert_eq!(rgba.len(), (w2 * h2 * 4) as usize);
    }

    // ---- Plain text (verbatim, no Markdown interpretation) --------------------

    #[test]
    fn plain_text_body_preserves_newlines_and_indentation() {
        // Each source newline → a hard break; leading indent → non-breaking spaces; a blank line survives.
        let body = plain_text_body("first\n\n    indented\nlast");
        assert_eq!(body.matches("#linebreak() ").count(), 3, "3 newlines → 3 breaks: {body}");
        assert!(body.contains("~~~~"), "leading spaces → nbsp: {body}");
        assert!(body.contains("indented") && body.contains("first") && body.contains("last"));
    }

    #[test]
    fn plain_text_does_not_interpret_markdown_and_compiles() {
        // A leading `#`, `*`, `=`, `-` must print literally (escaped), not become a heading/list/etc.,
        // and the tricky metacharacters must not inject Typst — end-to-end proof it compiles.
        let txt = "# not a heading\n* not a bullet\n= not typst\nloose $ # * < @ chars\n";
        let pdf = text_to_pdf(txt, dot(), &dflt(), None).expect("plain text should compile");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        // The `#` survives as literal text (escaped), never as a Typst code sigil.
        let body = plain_text_body(txt);
        assert!(!body.contains("#linebreak() #"), "a line's leading # must be escaped, not raw: {body}");
    }

    #[test]
    fn renders_plain_text_to_a_raster_image() {
        let (rgba, w, h) =
            text_to_image("line one\nline two\n", dot(), 1.0, &dflt(), None, PreviewTint::default())
                .expect("image");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.iter().any(|&b| b != 0));
    }

    #[test]
    fn empty_plain_text_still_compiles() {
        assert!(text_to_pdf("", dot(), &dflt(), None).expect("empty text").starts_with(b"%PDF-"));
    }

    #[test]
    fn mda_styled_plain_text_compiles() {
        // MDA styling (colour + bold/underline) spliced into a PLAIN-TEXT body must produce Typst the
        // engine accepts — proof the styled plain-text path compiles. Offsets into `txt`: "one" [11,14)
        // on line 1, "bold" [22,26) on line 2.
        use xyscript_xpl::{ATTR_BOLD, ATTR_UNDERLINE};
        let txt = "plain line one\nsecond bold line\n";
        let styler = crate::gfm::Styler::new(
            txt,
            vec![(11, 14, (200, 0, 0))],                 // "one" red
            vec![],
            vec![(22, 26, ATTR_BOLD | ATTR_UNDERLINE)],  // "bold" bold+underline
            (0, 0, 0),
            (255, 255, 255),
            true,
        );
        let pdf = text_to_pdf(txt, dot(), &dflt(), Some(&styler)).expect("styled plain text compiles");
        assert!(pdf.starts_with(b"%PDF-"), "missing PDF header");
        assert!(pdf.len() > 1000, "PDF implausibly small: {} bytes", pdf.len());
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
        let (rgba, w, h) =
            typst_to_image("= Title\n\nbody *text*\n", dot(), 1.0, None, PreviewTint::default())
                .expect("image");
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(rgba.iter().any(|&b| b != 0));
    }

    #[test]
    fn preview_tint_recolours_the_page_but_pdf_stays_white() {
        // The preview page paper is white by default; a PVBGCOLOR tint repaints it. The top-left pixel
        // is page paper (the page fills the raster width). Default → white; black tint → black.
        let plain = PreviewTint::default();
        let dark = PreviewTint { bg: Some((0, 0, 0)), fg: Some((240, 240, 240)) };
        let (white, w, _) = typst_to_image("Hello", dot(), 1.0, None, plain).expect("plain");
        let (black, _, _) = typst_to_image("Hello", dot(), 1.0, None, dark).expect("tinted");
        // Top-left corner (paper), opaque in both.
        assert_eq!(&white[0..4], &[255, 255, 255, 255], "default paper is white");
        assert_eq!(&black[0..4], &[0, 0, 0, 255], "PVBGCOLOR repaints the paper");
        let _ = w;
        // The tint is preview-only: TOPDF still compiles the source verbatim (no fill injected). We
        // can't easily inspect PDF pixels, but the PDF path must ignore the tint and still succeed.
        assert!(typst_to_pdf("Hello", dot(), None).is_ok());
    }

    #[test]
    fn preview_tint_diagnostics_still_map_to_the_buffer() {
        // A two-field tint prepends two `#set` lines ahead of authored typst. A compile error on the
        // user's line 3 must still be reported as 3 (the prelude lines are subtracted back out).
        let src = "#set text(size: 14pt)\n\n#nonexistent_fn()";
        let dark = PreviewTint { bg: Some((10, 10, 10)), fg: Some((230, 230, 230)) };
        let err = typst_to_image(src, dot(), 1.0, None, dark).unwrap_err();
        assert!(err.starts_with("3:"), "error should point at buffer line 3, got: {err}");
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
