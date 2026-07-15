# Packaging & icons

The XyScript icon is delivered three different ways because the three OSes handle app icons
differently. The source art is `../xyscript.ico` / `../icon-final-1024.png`; regenerate the
platform assets with `scratchpad/make_platform_icons.ps1` if the art changes.

| Platform | Executable / bundle icon | Window (titlebar/taskbar) icon |
|----------|--------------------------|--------------------------------|
| Windows  | Embedded `.ico` resource (build.rs) | winit `with_window_icon` **+** the embedded resource |
| Linux/X11 | — (use the `.desktop` launcher) | winit `with_window_icon` (`_NET_WM_ICON`) |
| Linux/Wayland | — | `.desktop` + hicolor PNG, matched by app-id `xyscript` |
| macOS    | `.icns` inside `XyScript.app` | n/a (macOS has no per-window icon) |

## Windows

Nothing to do — `cargo build --release -p xyscript-gui` embeds `assets/xyscript.ico` into the
`.exe` (via `build.rs` + the `embed-resource` crate). Explorer, the taskbar, and Alt-Tab pick it up.

## Linux

The app sets its X11 `WM_CLASS` and Wayland app-id to `xyscript` at runtime, matching
`linux/xyscript.desktop` (`StartupWMClass=xyscript`). To install the launcher + icons per-user:

```sh
cargo build --release -p xyscript-gui        # produces target/release/xyscript-gui
cd packaging/linux
./install.sh                                  # finds the release binary automatically
```

This drops the binary at `~/.local/bin/xyscript`, the launcher in
`~/.local/share/applications/`, and PNGs into `~/.local/share/icons/hicolor/*/apps/xyscript.png`.
On X11 the window already carries the icon at runtime; on Wayland the compositor reads it from the
installed `.desktop` + hicolor PNGs.

To build a `.deb` instead:

```sh
cargo install cargo-bundle
cargo bundle --release                        # reads [package.metadata.bundle] in Cargo.toml
```

## macOS

`assets/xyscript.icns` is a multi-resolution icon (16–1024 px, PNG-backed). Build a `.app` bundle:

```sh
cargo install cargo-bundle
cargo bundle --release        # -> target/release/bundle/osx/XyScript.app (icns = Dock/Finder icon)
```

Before publishing, change `identifier` in `[package.metadata.bundle]` (currently the placeholder
`net.xyscript.editor`) to a reverse-DNS you control, and code-sign/notarize as needed.

If you'd rather not use `cargo-bundle`, drop `xyscript.icns` into any hand-assembled
`XyScript.app/Contents/Resources/` and reference it from `Info.plist` via `CFBundleIconFile`.
