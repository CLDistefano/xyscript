# XyScript keyboard test — Quickstart

Thanks for helping test! This takes about 20 minutes and needs no Rust knowledge.
For the full details and the report-back template, see `TESTING.md`. This page is
the short version.

1. **Unzip `xyscript-kbd.zip`.** You'll get a folder named `xyscript-kbd`.

2. **Open a terminal and go into that folder.** Everything below must be run from
   *inside* it — the folder where you can see `Cargo.toml` next to `src/`:

   ```sh
   cd path/to/xyscript-kbd
   ```

3. **Install Rust** (skip if `cargo --version` already prints 1.74 or higher):

   - macOS/Linux: run
     `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`,
     then open a new terminal.
   - Windows: get the installer from <https://rustup.rs> and follow it.

4. **Build and self-test:**

   ```sh
   cargo test
   ```

   Expect it to compile and end with `test result: ok` (15 passed). If this
   fails, **stop and send the output** — nothing below will help until it's green.

5. **Quick no-permissions demo** (prints a scripted run and exits on its own):

   ```sh
   cargo run -- demo --kbd kbd/xyscript-internal.kbd
   ```

   You should see lines like `dispatch "q"`.

6. **Run the live key test for your OS:**

   - **Windows** (no special permissions):
     ```sh
     cargo run -- diag --kbd kbd/xyscript-internal.kbd
     ```
   - **Linux** — you must be at the machine's **physical keyboard / console, NOT
     over SSH** (SSH keystrokes are invisible to it). Then:
     ```sh
     cargo build
     sudo ./target/debug/xykbd diag --kbd kbd/xyscript-internal.kbd
     ```
   - **macOS** — first, in **System Settings -> Privacy & Security**, add your
     terminal app under **both** **Accessibility** and **Input Monitoring**, then
     **fully quit and reopen** the terminal. Then:
     ```sh
     cargo run -- diag --kbd kbd/xyscript-internal.kbd
     ```

7. **Press these keys slowly, one at a time, and watch the lines that appear:**
   Caps Lock, Left/Right Shift, Left/Right Ctrl, Left/Right Alt, Numpad 5,
   F1-F10, the letters q/w/e and a/s/d, and Esc and Tab. For each key you want to
   see a clean `DOWN` line and an `UP` line.

8. **Quit** with **Ctrl-C**. If it won't respond, just close the terminal window.

9. **Send back** the OS and version you used and what you saw — especially any key
   that gave *no* line, only one edge, a doubled line, or a stuck / never-released
   line. A screenshot of the output is perfect.

That's it — thank you!
