//! Shared startup helpers for both front-ends (the TUI binary and the GUI).
//!
//! Keeps the startup-script resolution in one place so the terminal and GUI boot
//! identically: same `startup.xpl` discovery, same `-s <path>` override.

use crate::editor::Editor;

/// Headless batch mode: if `args` contain `--run <path>`, run that XPL script with no UI and return
/// `Some(exit_code)`; return `None` when there's no `--run` flag (the caller proceeds to its
/// interactive TUI/GUI). The script's `PR`/`MSG` output goes to **stdout**, diagnostics to
/// **stderr**. Exit codes: `0` ok · `1` fatal error · `2` bad/missing `--run` path · `3` the script
/// needs interactive input (`INPUT`/`READKEY`), which batch can't provide.
///
/// This is what a `RUN/BG`-spawned child process invokes — `$ED --run <path>` — so background XPL is
/// just an `X/BG` job that runs xyscript headlessly. The child has its OWN editor state (its own
/// buffers/vars; cwd is set by the spawner); results flow back via stdout / files it writes.
pub fn run_batch(args: &[String]) -> Option<i32> {
    // Accept single- or double-dash (`-run`/`--run`), consistent with `-s`/`--startup`.
    let i = args.iter().position(|a| a == "--run" || a == "-run")?;
    let Some(path) = args.get(i + 1) else {
        eprintln!("xyscript: --run requires a <path>");
        stderr_done_nudge();
        return Some(2);
    };
    // Everything after the path is a positional arg for the script (`$1..$N`), matching a
    // foreground `RUN <path> [args…]`. A `RUN/BG` child is spawned exactly this way.
    let script_args = &args[i + 2..];
    let (out, err, code) = run_script_file(path, script_args);
    for line in out {
        println!("{line}");
    }
    let had_err = !err.is_empty();
    for line in err {
        eprintln!("{line}");
    }
    if had_err {
        stderr_done_nudge();
    }
    Some(code)
}

/// After writing diagnostics to stderr in `--run` mode, tell the user the process is DONE so an
/// error doesn't read as a hang. The shipped `xyscript.exe` is a GUI-subsystem binary, so an
/// interactive shell does NOT wait for it — its prompt already returned, and the error text lands
/// after that prompt, leaving the cursor sitting there. This one-line nudge (only when stderr is a
/// real terminal — never for a redirected run or a piped `RUN/BG` child) makes clear that pressing
/// Enter returns a clean prompt, rather than looking stuck.
fn stderr_done_nudge() {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!("[xyscript: finished — press Enter to return to your prompt]");
    }
}

/// Run a script file headlessly, returning `(stdout_lines, stderr_lines, exit_code)`. Factored out
/// of [`run_batch`] so it's unit-testable without capturing the process's real stdout.
fn run_script_file(path: &str, script_args: &[String]) -> (Vec<String>, Vec<String>, i32) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    // `--run -` reads the script from STDIN (this is how `RUNCODE/BG` feeds code to the child —
    // no temp file); any other path reads that file.
    let src = if path == "-" {
        match std::io::read_to_string(std::io::stdin()) {
            Ok(s) => s,
            Err(e) => {
                err.push(format!("xyscript: cannot read script from stdin: {e}"));
                return (out, err, 2);
            }
        }
    } else {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                err.push(format!("xyscript: cannot read {path}: {e}"));
                return (out, err, 2);
            }
        }
    };
    let mut editor = Editor::with_default_keymap();
    let arg_vals: Vec<xyscript_xpl::Value> =
        script_args.iter().map(|a| xyscript_xpl::Value::str(a.clone())).collect();
    // `--run -` reads from stdin (no file to name); a real path labels top-level errors `<file>:<line>`.
    let label = (path != "-").then_some(path);
    editor.run_command_with_args(&src, &arg_vals, label);
    out.extend(editor.output().iter().cloned());
    // `BK on`/`BK debug` breakpoints can't prompt in batch — auto-continue them so the whole script
    // runs (inert headless), collecting each resumed pass's output (the resume blanks it per pass).
    // A bare `MSG` parks on a modal box that can't be clicked in batch — auto-continue it the same
    // way (surfacing its text on stdout). Drained together: a script can interleave the two.
    // A timed/job `WAIT` also parks the VM. The interactive GUI resumes it each frame; headless we
    // must drive it here too — a background script (a polling alarm, a `WAIT`-until loop) parks on
    // `WAIT <ms>` / `WAIT`/`WAIT/J`, and without this the job would see the suspension, find nothing
    // to auto-continue, and exit at once ("job done"). We actually sleep the interval, then resume.
    let mut guard = 0;
    while guard < 1_000_000 {
        if editor.breakpoint_request().is_some() {
            editor.resolve_breakpoint(false);
        } else if let Some(text) = editor.message_request() {
            out.push(text);
            editor.dismiss_message();
        } else if editor.wait_repaint_after().is_some() {
            out.extend(editor.poll_jobs()); // advance any X/BG jobs the script started (for WAIT/J)
            editor.poll_wait(); // resume an elapsed sleep / satisfied job-wait; arm a fresh deadline
            if let Some(d) = editor.wait_repaint_after() {
                // Still pausing — sleep the remaining interval (capped so job-waits re-poll and a
                // very long `WAIT` stays responsive; floored so a `WAIT 0` spin still yields).
                let d = d
                    .min(std::time::Duration::from_secs(60))
                    .max(std::time::Duration::from_millis(1));
                std::thread::sleep(d);
            }
        } else {
            break;
        }
        out.extend(editor.output().iter().cloned());
        guard += 1;
    }
    // "File is already open" prompts can't be answered in batch — default to (Y)es (open again),
    // preserving the pre-prompt behavior so batch OPEN/READ stays deterministic.
    while editor.open_prompt().is_some() && guard < 100_000 {
        editor.resolve_open(crate::host::OpenAnswer::Yes);
        guard += 1;
    }
    // `MSG` routes to a modal box (no UI here) — surface its text on stdout too so nothing is lost.
    while let Some((text, _timeout)) = editor.take_pending_message() {
        out.push(text);
    }
    // A background `CHECK` runs on its own thread; with no GUI event loop to poll it, wait for it here
    // so the results tab is populated before we return.
    while editor.check_in_progress() {
        if !editor.poll_check() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // A script parked on INPUT/READKEY can't be answered in batch — report it rather than hang.
    if editor.awaiting_line() || editor.awaiting_key() {
        err.push("xyscript: --run: INPUT/READKEY is not available in batch mode".to_string());
        return (out, err, 3);
    }
    if let Some(e) = editor.last_error() {
        err.push(format!("xyscript: {e}"));
        return (out, err, 1);
    }
    (out, err, 0)
}

/// Resolve the startup script to run at boot, from the process args (pass `std::env::args()`
/// already collected, *including or excluding* argv[0] — only `-s`/`--startup` flags are read):
///
/// - `-s <path>` / `--startup <path>` → that file (an error if it can't be read).
/// - otherwise → `startup.xpl` next to the executable (`$XSDIR`), if present.
///
/// Returns `Ok(Some((label, source)))` when a script is found, `Ok(None)` when none applies, or
/// `Err(msg)` when an explicit `-s` path can't be read. `label` is for diagnostics/status.
pub fn resolve_startup_script(args: &[String]) -> Result<Option<(String, String)>, String> {
    if let Some(i) = args.iter().position(|a| a == "-s" || a == "--startup") {
        let path = args
            .get(i + 1)
            .ok_or_else(|| "-s/--startup requires a <path>".to_string())?;
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read startup script {path}: {e}"))?;
        return Ok(Some((path.clone(), src)));
    }
    // Auto-discover `startup.xpl` beside the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("startup.xpl");
            if let Ok(src) = std::fs::read_to_string(&p) {
                return Ok(Some((p.to_string_lossy().into_owned(), src)));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{resolve_startup_script, run_batch, run_script_file};

    /// Write `body` to a uniquely-named temp `.xpl` and return its path.
    fn temp_script(tag: &str, body: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("xyscript_batch_{tag}.xpl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn batch_runs_script_pr_to_stdout() {
        let p = temp_script("ok", "PR hi\nPR there");
        let (out, _err, code) = run_script_file(&p.to_string_lossy(), &[]);
        assert_eq!(code, 0);
        assert_eq!(out, vec!["hi".to_string(), "there".to_string()]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn batch_surfaces_msg_on_stdout() {
        let p = temp_script("msg", "MSG hello from batch");
        let (out, _err, code) = run_script_file(&p.to_string_lossy(), &[]);
        assert_eq!(code, 0);
        assert!(out.contains(&"hello from batch".to_string()), "out = {out:?}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn batch_missing_file_exits_2() {
        let (_out, err, code) = run_script_file("no-such-batch-script-zzz.xpl", &[]);
        assert_eq!(code, 2);
        assert!(err.iter().any(|l| l.contains("cannot read")));
    }

    #[test]
    fn batch_fatal_error_exits_1() {
        // Assigning to a read-only special is a fatal error → exit 1.
        let p = temp_script("fatal", "SET OS = 1");
        let (_out, _err, code) = run_script_file(&p.to_string_lossy(), &[]);
        assert_eq!(code, 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn batch_auto_continues_bk_breakpoints() {
        // BK can't prompt in batch — it's inert: the whole script runs and all output survives.
        let p = temp_script("bk", "PR before\nBK on\nPR after\nBK debug\nPR done");
        let (out, _err, code) = run_script_file(&p.to_string_lossy(), &[]);
        assert_eq!(code, 0);
        assert!(out.iter().any(|l| l == "before"));
        assert!(out.iter().any(|l| l == "after"));
        assert!(out.iter().any(|l| l == "done"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn batch_rejects_interactive_input_exits_3() {
        let p = temp_script("input", "READKEY k");
        let (_out, err, code) = run_script_file(&p.to_string_lossy(), &[]);
        assert_eq!(code, 3);
        assert!(err.iter().any(|l| l.contains("not available in batch")));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dash_s_requires_a_following_path() {
        assert!(resolve_startup_script(&["-s".to_string()]).is_err());
        assert!(resolve_startup_script(&["--startup".to_string()]).is_err());
    }

    #[test]
    fn dash_s_reads_the_given_file() {
        let mut p = std::env::temp_dir();
        p.push("xyscript_boot_resolve_test.xpl");
        std::fs::write(&p, "PR hi").unwrap();
        let args = vec!["-s".to_string(), p.to_string_lossy().into_owned()];
        match resolve_startup_script(&args).unwrap() {
            Some((_label, src)) => assert_eq!(src, "PR hi"),
            None => panic!("expected the -s file to be found"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dash_s_nonexistent_path_is_err() {
        let args = vec!["-s".to_string(), "definitely-no-such-file-zzz.xpl".to_string()];
        assert!(resolve_startup_script(&args).is_err());
    }

    #[test]
    fn run_batch_accepts_single_or_double_dash_run() {
        // No `-run`/`--run` → not a batch invocation (the GUI path handles it).
        assert!(run_batch(&["notes.md".to_string()]).is_none());
        // Both spellings are recognized as the batch flag; a missing path exits with code 2.
        assert_eq!(run_batch(&["--run".to_string()]), Some(2));
        assert_eq!(run_batch(&["-run".to_string()]), Some(2));
        // A recognized `-run <badpath>` reports it can't read the file (code 2) — NOT ignored.
        let (_o, err, code) =
            run_script_file("definitely-no-such-file-zzz-batch.xpl", &[]);
        assert_eq!(code, 2);
        assert!(err.iter().any(|l| l.contains("cannot read")));
    }

    #[test]
    fn batch_run_drives_a_timed_wait() {
        // A `WAIT <ms>` must actually pause and RESUME in headless mode (what `RUN/BG` spawns) — the
        // bug was that a parked WAIT looked "done", so a background alarm exited instantly. Tiny
        // interval so the test is fast; the point is that the post-WAIT line runs at all.
        let mut p = std::env::temp_dir();
        p.push("xyscript_batch_wait_test.xpl");
        std::fs::write(&p, "pr before\nwait 20\npr after").unwrap();
        let path = p.to_string_lossy().into_owned();
        let (out, _err, code) = run_script_file(&path, &[]);
        let _ = std::fs::remove_file(&p);
        assert_eq!(code, 0);
        assert!(out.iter().any(|l| l == "before"), "out: {out:?}");
        assert!(out.iter().any(|l| l == "after"), "the WAIT resumed and the script continued: {out:?}");
    }
}
