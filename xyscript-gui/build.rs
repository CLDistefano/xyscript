//! Build script: on a Windows **target**, embed a Per-Monitor-DPI-Aware v2 manifest and the
//! application icon.
//!
//! The manifest is applied before any code runs, so the OS treats us as per-monitor aware from the
//! start and sends real DPI-change events instead of bitmap-stretching a window dragged across a
//! mixed-DPI boundary (which grows + blurs it). The icon (resource ID 1 in `app.rc`) is what
//! Explorer, the taskbar, and Alt-Tab show for the executable itself.
//!
//! On Linux and macOS this is a no-op — those platforms don't embed an icon in the binary; they use
//! a `.desktop` file / `.app` bundle at packaging time. The window's own icon is set at runtime via
//! winit (see `runtime.rs`), which covers Windows and Linux/X11.
//!
//! IMPORTANT: a build script runs on the HOST, so `cfg(windows)` here would test the host, not the
//! target — which breaks cross-compiling (a Windows host targeting Linux would try, and fail, to
//! embed a Windows manifest). Gate on `CARGO_CFG_TARGET_OS` (the TARGET) instead. The embed crates
//! are Windows-host build-dependencies, so the code that calls them stays behind `cfg(windows)` (the
//! host) and only actually runs when target == host == Windows.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let windows_target = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !windows_target {
        return; // Linux/macOS target: nothing to embed here.
    }

    #[cfg(windows)]
    embed_windows_resources();
    #[cfg(not(windows))]
    println!(
        "cargo:warning=skipping Windows manifest/icon embedding: cross-compiling to Windows needs a \
         Windows host"
    );
}

/// Embed the DPI manifest and the application icon (Windows host + Windows target only).
#[cfg(windows)]
fn embed_windows_resources() {
    use embed_manifest::manifest::DpiAwareness;
    use embed_manifest::{embed_manifest, new_manifest};

    embed_manifest(new_manifest("XyScript").dpi_awareness(DpiAwareness::PerMonitorV2))
        .expect("failed to embed Windows manifest");

    // Icon only (no manifest here), so it never collides with embed_manifest above.
    let _ = embed_resource::compile("app.rc", embed_resource::NONE);

    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/xyscript.ico");
}
