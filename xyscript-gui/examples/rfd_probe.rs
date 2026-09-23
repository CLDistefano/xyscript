//! Standalone rfd probe. Args: [mode] [dir].
//!   mode "many" → the app's full 22-extension "Text files" filter; anything else → just txt,md.
//! Tests whether the SWITCH-to-All-files refresh depends on how many extensions the first filter has.
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let dir = std::env::args().nth(2);
    let many: &[&str] = &[
        "txt", "text", "md", "markdown", "rst", "tex", "typ", "log", "xpl", "kbd", "lib", "cfg",
        "json", "csv", "tsv", "xml", "yaml", "yml", "toml", "ini", "html", "css",
    ];
    let few: &[&str] = &["txt", "md"];
    let text_exts = if mode == "many" { many } else { few };
    let mut d = rfd::FileDialog::new()
        .add_filter("Text files", text_exts)
        .add_filter("All files", &["*"]);
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    println!("mode={mode} exts={} picked: {:?}", text_exts.len(), d.pick_file());
}
