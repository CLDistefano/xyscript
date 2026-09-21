//! The executor (`stage1-interpreter-plan.md` §1, §11, §13.7–§13.9).
//!
//! A plain instruction pointer over the lowered [`ir::Program`] plus a call-frame
//! stack. There is **no `after_idle` loop and no runtime block stack** — `GOTO`
//! re-iteration is just `ip` moving backward, and IF/WHILE/BREAK/CONTINUE are
//! pre-resolved jumps (`ir.rs`). The VM:
//!
//! - evaluates `SET`/`IF`/`WHILE` expressions against an [`EvalContext`] that
//!   layers built-in specials (`$ERROR`, `$#`, `$*`, `$RV`, positional `$1`…),
//!   then the [`Host`]'s live specials, then user variables (specials win);
//! - dispatches `Command`s through a small verb set, routing editor verbs,
//!   `LOADKBD`, and file reads to the [`Host`];
//! - resolves `CALL` against the script's own subs **and the loaded library**
//!   (merged in at compile time by [`ir::lower_with`]);
//! - enforces the two-tier error model: a [`XplError::Trappable`] from a verb
//!   sets `$ERROR=1` and execution continues; a [`XplError::Fatal`] aborts.
//!   `$ERROR` auto-clears at each *action* dispatch (not the exempt verbs).
//!
//! Execution is a free [`execute`] function over an [`ExecCtx`] so `RUN` can
//! re-enter it for a nested script that shares the same env/library/host.
//!
//! [`Host`]: crate::host::Host

use crate::env::{is_special_name, Env};
use crate::error::{VmResult, XplError};
use crate::expr::{self, EvalContext, Expr};
use crate::host::{
    BinderSource, CalcContext, CaretShape, CaseKind, ClipKind, CmlinePlacement, ColorGround, DefineKind, DeleteKind, DirOnly,
    DirOpts, DirOrder, DocTypeReq, EditorOp, Host, LockKey, MarkGutterMode, Motion, MsgColorSlot, PersistKind, PvMarginSide,
    SearchSpec, SortSpec, UiSizeKind, UnderlineStyle, ViewFlag, ViewToggle,
};
use crate::ir::{self, Instr};
use crate::library::{parse_lib, validate_names, LibSource, Library, Routine};
use crate::subst::substitute;
use crate::value::{fold_ident, Value};
use std::collections::BTreeMap;

/// Verbs that must NOT auto-clear/auto-set `$ERROR` (control/test/info verbs).
/// Action verbs (everything else) clear `$ERROR` on entry and set it on a
/// trappable failure. Folded names.
const NO_ERROR_TOUCH: &[&str] = &[
    "CALL", "ENV", "SETERR", "CLERR", "QUIET",
    "?", "ABOUT", "HELP", "REF", "MAN", "COMMANDS", "LISTLIB", "LISTVAR", "LISTVARS", "HISTORY", "HI", "BUFLIST",
    "HLKINDS",
];

/// Verbs whose single argument IS a file to act upon. When one of these is issued with NO argument
/// while the caret is on a file in a `DIR`/`LS` listing, the argument is filled from that file
/// (point-and-shoot — see `exec_command`). Two-argument commands (`COPY`, `RENAME`) and destination
/// verbs that write the active buffer (`ST`, `SAAS`) are deliberately excluded.
const POINT_AND_SHOOT_VERBS: &[&str] = &[
    "OPEN", "CA", "ED", "EDIT", "READ", "DEL", "ERNV", "RUN", "BEDIT", "LOADKBD", "LOADLIB",
    "LOADAR", "LOADVARS", "LANG", "NEW", "NE", "THEME",
];

/// Single-path file verbs: the whole (trimmed) argument is one filesystem path. A user may wrap it
/// in double-quotes — from habit, or because the path contains spaces — so before dispatch we strip
/// ONE surrounding quote pair (see [`unquote_path`]) for these verbs. Stripped WITHOUT the string-
/// escape lexer, so raw backslashes survive (`OPEN "C:\Users\me"` opens that file, not `C:\Users<CR>e`).
/// Excluded: multi-token verbs (`RUN <file> args…`) and the two-path verbs (`COPY`/`RENAME`, which
/// already tokenize with quote handling), and the `literal_text` verbs, which unquote themselves.
const SINGLE_PATH_VERBS: &[&str] = &[
    "OPEN", "CA", "ED", "EDIT", "READ", "NEW", "NE", "SAVE", "SA", "ST", "SAAS", "SAVEAS", "SAD",
    "MERGE", "ME", "BEDIT", "BCLOSE", "DEL", "ERNV", "CD", "PRINTEX", "LOADKBD", "LOADLIB", "ADDLIB",
    "LOADAR", "SAVEAR", "LOADVARS", "SAVEVARS", "LOADCFG", "SAVECFG", "LANG", "THEME",
];

/// Instruction budget for one cooperative-preemption slice. The foreground editor runs scripts
/// through [`Interpreter::run_sliced`]/[`resume_yield`](Interpreter::resume_yield) with this
/// granularity and time-boxes the per-frame work; the WAIT/INPUT resume path
/// ([`drive_resume`](Interpreter::drive_resume)) uses it too, so NO in-process continuation —
/// however it was entered — can spin the CPU without periodically yielding control back to the
/// GUI. That is what keeps a runaway script (e.g. `WHILE 1` with no `WAIT`) from freezing the
/// editor, and what lets a Ctrl+Break abort actually be seen. Small enough that abort latency is
/// a fraction of a frame; large enough that per-slice setup is negligible.
pub const PREEMPT_SLICE: u32 = 65_536;

/// The body of the built-in `KBDDOC` command — an XPL script (embedded, not on disk) that annotates a
/// KBD file with a `GETKEY` key-name column. The `KBDDOC` verb runs it via [`run_source_sync`], seeding
/// `$1` with the optional file argument.
const KBDDOC_SRC: &str = include_str!("kbddoc.xpl");

/// A persistent interpreter: owns the script-global [`Env`] and the [`Library`]
/// so user variables and loaded routines survive across `run()` calls (the
/// app-global behavior QN has — a key press can set a var or load a library that
/// a later key press uses).
pub struct Interpreter {
    env: Env,
    library: Library,
    /// Ordered user-library load paths, replayed by `RELOADLIB`.
    lib_loads: Vec<String>,
    output: Vec<String>,
    /// A transient status/feedback line from the most recent run: an op result
    /// ("opened X", "saved X") or a trappable error message. Distinct from
    /// `output` (which is `PR`/`MSG` script output) so the two don't collide.
    last_status: Option<String>,
    /// The continuation stack of parked scripts, OUTERMOST-first (`.last()` is the innermost,
    /// currently-waiting frame whose `reason` drives the editor). A single top-level pause is a
    /// one-element stack; a pause inside a nested `RUN`/`RUNCODE` stacks the outer frames below it,
    /// so resuming cascades back out. Non-empty ⇒ a script is parked; no new top-level run starts.
    suspended: Vec<SuspendedScript>,
}

impl Default for Interpreter {
    fn default() -> Self {
        Interpreter::new()
    }
}

impl Interpreter {
    pub fn new() -> Interpreter {
        Interpreter {
            env: Env::new(),
            library: Library::new(),
            lib_loads: Vec::new(),
            output: Vec::new(),
            last_status: None,
            suspended: Vec::new(),
        }
    }

    /// Whether a script is currently parked (on `READKEY`/`INPUT`/`WAIT`).
    pub fn is_suspended(&self) -> bool {
        !self.suspended.is_empty()
    }

    /// Abort EVERY parked in-process script in one shot: drop the entire continuation stack and
    /// close any open `CAPTURE` redirect (so it can't swallow later output). Returns `true` if a
    /// script was actually running/parked. This is the orderly stop the editor wires to Ctrl+Break
    /// for a runaway or hung script — it reaches the whole in-process stack (like `EXITALL`), but
    /// NOT background `RUN/BG` child processes, which have their own `KILL`. Leaves a "Script
    /// aborted" status for the caller to surface.
    pub fn abort(&mut self) -> bool {
        let was_running = !self.suspended.is_empty();
        self.suspended.clear();
        self.env.capture.clear();
        self.output.clear();
        self.last_status = Some("Script aborted".into());
        was_running
    }

    /// The pending `READKEY`/`INPUT` request if a script is parked awaiting input.
    pub fn input_request(&self) -> Option<InputRequest> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ReadKey { message, describe, .. }) => {
                Some(InputRequest { kind: InputKind::Key, prompt: message.clone(), describe: *describe })
            }
            Some(SuspendReason::InputLine { prompt, .. }) => {
                Some(InputRequest { kind: InputKind::Line, prompt: prompt.clone(), describe: false })
            }
            _ => None,
        }
    }

    /// The `(title, items)` of a parked `PICK` list chooser, if a script is on one (`title` `None` =
    /// the default). The editor shows the chooser and resumes via [`resume_pick`](Self::resume_pick).
    pub fn pick_request(&self) -> Option<(Option<String>, Vec<String>)> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(
                SuspendReason::Pick { title, items, .. }
                | SuspendReason::CpPick { title, items, .. }
                | SuspendReason::ReopenPick { title, items, .. }
                | SuspendReason::TabPick { title, items, .. }
                | SuspendReason::MarkPick { title, items, .. }
                | SuspendReason::LintFixPick { title, items, .. },
            ) => Some((title.clone(), items.clone())),
            _ => None,
        }
    }

    /// Whether the parked chooser is a `LISTTABS` tab-switch (as opposed to a `PICK`/`REOPEN`/`CP`).
    /// The editor uses this to make a single click switch tabs immediately, rather than only
    /// highlighting the row (a tab list is act-on-click, like clicking the tab itself).
    pub fn pick_is_tab_switch(&self) -> bool {
        matches!(self.suspended.last().map(|s| &s.reason), Some(SuspendReason::TabPick { .. }))
    }

    /// Resume a `PICK`-parked script. `choice` is the chosen item's index into the parked `items`
    /// (`None` = cancelled / out of range). Writes the result into the destination variable (the
    /// item's string, or its 1-based index under `/I`) and sets/clears `$ERROR` accordingly, then
    /// drives the continuation. No-op if nothing is parked on a `PICK`.
    pub fn resume_pick(&mut self, choice: Option<usize>, host: &mut dyn Host) -> VmResult<()> {
        enum Kind {
            Var(String, Vec<String>, bool),
            Cp(Vec<usize>),
            Reopen(Vec<(String, usize)>),
            Tab(Vec<usize>),
            Mark(Vec<usize>),
            LintFix((usize, usize), Vec<String>),
        }
        let kind = match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::Pick { var, items, index_mode, .. }) => {
                Kind::Var(var.clone(), items.clone(), *index_mode)
            }
            Some(SuspendReason::CpPick { sources, .. }) => Kind::Cp(sources.clone()),
            Some(SuspendReason::ReopenPick { entries, .. }) => Kind::Reopen(entries.clone()),
            Some(SuspendReason::TabPick { targets, .. }) => Kind::Tab(targets.clone()),
            Some(SuspendReason::MarkPick { targets, .. }) => Kind::Mark(targets.clone()),
            Some(SuspendReason::LintFixPick { span, replacements, .. }) => {
                Kind::LintFix(*span, replacements.clone())
            }
            None => return Ok(()),
            _ => return self.drive_resume(host), // not a pick — drive harmlessly
        };
        match kind {
            Kind::Var(var, items, index_mode) => {
                let value = match choice {
                    Some(i) if i < items.len() => {
                        self.env.error = 0;
                        if index_mode {
                            Value::Int((i + 1) as i64)
                        } else {
                            Value::str(items[i].clone())
                        }
                    }
                    _ => {
                        self.env.error = 1; // cancelled / out of range
                        if index_mode { Value::Int(0) } else { Value::str(String::new()) }
                    }
                };
                self.env.vars.insert(var, value);
            }
            Kind::Cp(sources) => match choice.and_then(|i| sources.get(i).copied()) {
                Some(buf) => {
                    self.env.error = 0;
                    host.copy_selection_from(buf)?; // insert that tab's selection at the caret
                }
                None => self.env.error = 1, // cancelled
            },
            Kind::Reopen(entries) => match choice.and_then(|i| entries.get(i).cloned()) {
                Some((path, caret)) => {
                    self.env.error = 0;
                    host.reopen_file(&path, caret)?; // open + restore caret
                }
                None => self.env.error = 1, // cancelled
            },
            Kind::Tab(targets) => match choice.and_then(|i| targets.get(i).copied()) {
                Some(buf) => {
                    self.env.error = 0;
                    host.switch_to_buffer(buf)?; // make that tab active
                }
                None => self.env.error = 1, // cancelled — active tab unchanged
            },
            Kind::Mark(targets) => match choice.and_then(|i| targets.get(i).copied()) {
                Some(off) => {
                    self.env.error = 0;
                    host.jump_to_offset(off)?; // move the caret to that bookmark (centered)
                }
                None => self.env.error = 1, // cancelled — caret unchanged
            },
            Kind::LintFix(span, replacements) => {
                match choice.and_then(|i| replacements.get(i).cloned()) {
                    Some(rep) => {
                        self.env.error = 0;
                        host.apply_lint_fix(span, &rep)?; // replace the issue with the chosen fix
                    }
                    None => self.env.error = 1, // cancelled — document unchanged
                }
            }
        }
        self.drive_resume(host)
    }

    /// What a parked `WAIT`/`WAIT/J` is waiting on, if a script is suspended on one.
    pub fn wait_request(&self) -> Option<WaitTarget> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::Wait(t)) => Some(t.clone()),
            _ => None,
        }
    }

    /// The interval (ms) of a parked `WAIT <ms>` timed pause, if a script is on one. The
    /// editor times it and calls [`resume_wait`](Self::resume_wait) when it elapses.
    pub fn sleep_request(&self) -> Option<u64> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::Sleep(ms)) => Some(*ms),
            _ => None,
        }
    }

    /// The `(title, text)` of a parked `TXT` modal dialog, if a script is on one (`title`
    /// `None` = the default). The editor resumes via [`resume_wait`](Self::resume_wait) when
    /// the user dismisses the dialog.
    pub fn text_request(&self) -> Option<(Option<String>, String)> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ShowText { title, text, .. }) => Some((title.clone(), text.clone())),
            _ => None,
        }
    }

    /// The optional two-column `(name, description)` grid for a parked `ShowText`, if it
    /// carries one (currently only `LISTLIB`). The GUI renders this as a wrapping grid;
    /// callers that just want the flat text use [`text_request`](Self::text_request).
    pub fn columns_request(&self) -> Option<Vec<(String, String)>> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ShowText { columns, .. }) => columns.clone(),
            _ => None,
        }
    }

    /// The optional per-line colours for a parked `ShowText` (currently only `HLKINDS`), one entry
    /// per line of `text` (`None` = the line uses the default text colour). Lets the GUI paint each
    /// line's label in its own colour.
    pub fn line_colors_request(&self) -> Option<Vec<Option<(u8, u8, u8)>>> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ShowText { line_colors, .. }) => line_colors.clone(),
            _ => None,
        }
    }

    /// The text of a parked bare-`MSG` modal box, if a script is on one. The editor resumes via
    /// [`resume_wait`](Self::resume_wait) when the user dismisses it. (`MSG/T:` toasts don't park.)
    pub fn message_request(&self) -> Option<String> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ShowMessage { text }) => Some(text.clone()),
            _ => None,
        }
    }

    /// If a script is parked on a `BK` breakpoint, returns `Some(debug)` (`true` = the variable-dump
    /// `BK debug` form, `false` = the simple `BK on` Y/N stop prompt). The editor shows the prompt
    /// and either resumes (continue, via [`resume_wait`](Self::resume_wait)) or [`abort`](Self::abort)s.
    pub fn breakpoint_request(&self) -> Option<bool> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::Breakpoint { debug, .. }) => Some(*debug),
            _ => None,
        }
    }

    /// The `(line, col)` of the `BK` a script is paused on (1-based; `col` 0 = unknown), for the
    /// breakpoint dialog header. `None` if not parked on a breakpoint.
    pub fn breakpoint_location(&self) -> Option<(usize, usize)> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::Breakpoint { line, col, .. }) => Some((*line, *col)),
            _ => None,
        }
    }

    /// A snapshot of the user variables (name, display value), sorted by name — for the `BK debug`
    /// dialog. Specials/positionals aren't user vars, so this is exactly what the script has set.
    pub fn vars_snapshot(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            self.env.vars.iter().map(|(k, val)| (k.clone(), val.to_str())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Resume an input-parked script, writing `value` into the innermost frame's destination
    /// variable, then driving the continuation. No-op if nothing is parked (or the active
    /// suspension isn't an input request).
    pub fn resume_input(&mut self, value: Value, host: &mut dyn Host) -> VmResult<()> {
        match self.suspended.last().map(|s| &s.reason) {
            Some(SuspendReason::ReadKey { var, .. } | SuspendReason::InputLine { var, .. }) => {
                let var = var.clone();
                self.env.vars.insert(var, value);
            }
            None => return Ok(()),
            _ => {} // active suspension isn't input — still drive (harmless)
        }
        self.drive_resume(host)
    }

    /// Resume a `WAIT`-parked script (no value injected). No-op if nothing is parked.
    pub fn resume_wait(&mut self, host: &mut dyn Host) -> VmResult<()> {
        if self.suspended.is_empty() {
            return Ok(());
        }
        self.drive_resume(host)
    }

    /// Re-enter the parked continuation from the innermost frame, **cascading** outward: resume the
    /// active frame; if it finishes, resume the frame that was waiting on it (its caller continuing
    /// after the `RUN`), and so on — stopping when a frame suspends again on a real reason (the new
    /// innermost) or the whole stack drains. A frame that itself spawns a nested suspension re-parks
    /// with that nested continuation on top.
    fn drive_resume(&mut self, host: &mut dyn Host) -> VmResult<()> {
        // Each resume is a fresh "pass": blank the PR/status channel so a looping script's `PR`
        // (e.g. `WHILE … PR … WAIT … ENDWHILE`) overwrites the status line rather than appending to
        // an ever-growing join. The output channel reflects the MOST RECENT pass.
        self.output.clear();
        self.last_status = None;
        while let Some(s) = self.suspended.pop() {
            let SuspendedScript { program, ip, frames, rv, .. } = s;
            let mut pending: Vec<SuspendedScript> = Vec::new();
            let mut exit_all = false;
            let outcome = {
                let mut ctx = ExecCtx {
                    env: &mut self.env,
                    host,
                    library: &mut self.library,
                    lib_loads: &mut self.lib_loads,
                    output: &mut self.output,
                    status: &mut self.last_status,
                    pending: &mut pending,
                    exit_all: &mut exit_all,
                    err_line: 0,
                    err_located: false,
                    cur_command: String::new(),
                    cur_file: None,
                    err_logged: false,
                };
                resume_program(&program, &mut ctx, ip, frames, rv, Some(PREEMPT_SLICE))?
            };
            if exit_all {
                // EXITALL during this resume: drop the entire remaining continuation stack.
                self.suspended.clear();
                return Ok(());
            }
            match outcome {
                // Finished: cascade — loop to resume the next (outer) frame after its RUN.
                ExecOutcome::Done { .. } => {}
                // Paused again: re-park this frame plus any nested it spawned (innermost last).
                ExecOutcome::Suspended { ip, frames, rv, reason } => {
                    self.suspended.push(SuspendedScript { program, ip, frames, rv, reason });
                    pending.reverse();
                    self.suspended.extend(pending);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Park the continuation chain from a just-run top-level `program`: its `outcome` (the outer
    /// frame) plus the nested continuations `pending` it spawned (innermost-first). The resulting
    /// stack is outermost-first (`.last()` = innermost). A `Done` outcome parks nothing.
    fn park_run(&mut self, program: ir::Program, outcome: ExecOutcome, mut pending: Vec<SuspendedScript>) {
        if let ExecOutcome::Suspended { ip, frames, rv, reason } = outcome {
            self.suspended.push(SuspendedScript { program, ip, frames, rv, reason });
            pending.reverse();
            self.suspended.extend(pending);
        }
    }

    /// Output collected by `PR`/`MSG`/`LISTLIB` during the **most recent** run.
    pub fn output(&self) -> &[String] {
        &self.output
    }

    /// Transient status/feedback from the most recent run (op result or trappable
    /// error), if any.
    pub fn status_message(&self) -> Option<&str> {
        self.last_status.as_deref()
    }

    /// Set (or clear) the status line directly, for host-driven UI actions that don't
    /// run through `run` — e.g. activating a directory-listing entry on Enter.
    pub fn set_status(&mut self, msg: Option<String>) {
        self.last_status = msg;
    }

    /// Whether `$ERROR` is currently set (non-zero).
    pub fn error_flag(&self) -> bool {
        self.env.error != 0
    }

    /// The current `$ERROR` code (`0` = no error).
    pub fn error_code(&self) -> u32 {
        self.env.error
    }

    /// Read a user variable (for tests/inspection).
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.env.get(name)
    }

    /// Set a user variable from outside a run — e.g. the editor delivering a finished `/R:<var>`
    /// background job's output into the parent script's variable. Folds the name like `SET`.
    pub fn set_var(&mut self, name: &str, value: Value) {
        self.env.set(name, value);
    }

    /// Install the **system** library from `.lib` source (app/test setup; no host
    /// needed). Shadowing routines are dropped; returns the rejection warnings.
    pub fn load_system_library_source(&mut self, text: &str) -> Result<Vec<String>, Vec<String>> {
        let routines = parse_lib(text, LibSource::System)?;
        let (accepted, rejected) = validate_names(routines);
        self.library.set_system(accepted);
        Ok(rejected)
    }

    /// Install the **user** library from `.lib` source (app/test setup).
    pub fn load_user_library_source(&mut self, text: &str) -> Result<Vec<String>, Vec<String>> {
        let routines = parse_lib(text, LibSource::User)?;
        let (accepted, rejected) = validate_names(routines);
        self.library.set_user(accepted);
        self.lib_loads.clear();
        Ok(rejected)
    }

    /// Number of loaded user-library routines (for inspection/tests).
    pub fn user_library_count(&self) -> usize {
        self.library.user_count()
    }

    /// Compile and run `source`, with no positional arguments.
    pub fn run(&mut self, source: &str, host: &mut dyn Host) -> VmResult<()> {
        self.run_with_args(source, &[], host)
    }

    /// Compile and run `source` with positional args (`$1..$N`).
    pub fn run_with_args(
        &mut self,
        source: &str,
        args: &[Value],
        host: &mut dyn Host,
    ) -> VmResult<()> {
        self.output.clear();
        self.last_status = None;
        self.suspended.clear(); // a fresh top-level run cancels any stale parked continuation
        self.env.capture.clear(); // drop any capture left dangling by an aborted run
        // Merge the library's routines in as hoisted subs. `effective()` returns
        // owned bodies, so the borrow of `self.library` ends before `ctx` takes
        // it mutably (letting LOADLIB mutate it during the run).
        let extra = self.library.effective();
        let program = ir::compile_with_library(source, &extra).map_err(XplError::fatal)?;
        drop(extra);
        let mut pending: Vec<SuspendedScript> = Vec::new();
        let mut exit_all = false;
        let outcome = {
            let mut ctx = ExecCtx {
                env: &mut self.env,
                host,
                library: &mut self.library,
                lib_loads: &mut self.lib_loads,
                output: &mut self.output,
                status: &mut self.last_status,
                pending: &mut pending,
                exit_all: &mut exit_all,
                err_line: 0,
                err_located: false,
                cur_command: String::new(),
                cur_file: None,
                err_logged: false,
            };
            execute(&program, &mut ctx, args, None)?
        };
        // `EXITALL` aborts everything: park nothing (the stack was cleared at run start).
        if !exit_all {
            self.park_run(program, outcome, pending);
        }
        Ok(())
    }

    /// Compile and run `source` with an instruction **budget** (a slice): it executes at most
    /// `budget` instructions, then parks on [`SuspendReason::Yield`] (or any other suspension it
    /// hits first — `WAIT`/`INPUT`/…). Drive it to completion by calling [`resume_yield`] while
    /// [`is_yielded`] is true. This is the preemption primitive the `RUN/BG` scheduler will use;
    /// foreground `run` is unbudgeted and unaffected.
    pub fn run_sliced(&mut self, source: &str, budget: u32, host: &mut dyn Host) -> VmResult<()> {
        self.run_sliced_with_args(source, &[], None, budget, host)
    }

    /// Like [`run_sliced`](Self::run_sliced), but seeds the top-level script's positional args
    /// (`$1..$N`) — used by the headless `--run <path> [args…]` entry so a batch/`RUN/BG` child
    /// receives the same arguments a foreground `RUN` would pass. `label` (a source path/name, e.g.
    /// `startup.xpl` or the `--run` file) stamps a top-level fatal error as `<file>:<line>: …`; pass
    /// `None` for an ad-hoc command line (no file to name).
    pub fn run_sliced_with_args(
        &mut self,
        source: &str,
        args: &[Value],
        label: Option<&str>,
        budget: u32,
        host: &mut dyn Host,
    ) -> VmResult<()> {
        self.output.clear();
        self.last_status = None;
        self.suspended.clear();
        let extra = self.library.effective();
        let program = ir::compile_with_library(source, &extra).map_err(XplError::fatal)?;
        drop(extra);
        let mut pending: Vec<SuspendedScript> = Vec::new();
        let mut exit_all = false;
        let outcome = {
            let mut ctx = ExecCtx {
                env: &mut self.env,
                host,
                library: &mut self.library,
                lib_loads: &mut self.lib_loads,
                output: &mut self.output,
                status: &mut self.last_status,
                pending: &mut pending,
                exit_all: &mut exit_all,
                err_line: 0,
                err_located: false,
                cur_command: String::new(),
                cur_file: label.map(|s| s.to_string()),
                err_logged: false,
            };
            match execute(&program, &mut ctx, args, Some(budget)) {
                Ok(o) => o,
                // A fatal error directly in the top-level file (not already located by a nested RUN)
                // gets stamped `<file>:<line>` too, so a bad line in `startup.xpl` itself points at it.
                Err(e) => return Err(locate_top_level(e, label, ctx.err_line, ctx.err_located)),
            }
        };
        if !exit_all {
            self.park_run(program, outcome, pending);
        }
        Ok(())
    }

    /// Resume a [`SuspendReason::Yield`]-parked script with a fresh instruction `budget`. No-op if
    /// nothing is parked or the parked script is waiting on something else (`WAIT`/`INPUT`/…) —
    /// those resume through their own paths. Output/status accumulate across slices (not cleared).
    pub fn resume_yield(&mut self, budget: u32, host: &mut dyn Host) -> VmResult<()> {
        // Only resume when the active (innermost) frame is a budget Yield; otherwise it's waiting on
        // something else (WAIT/INPUT/…) that resumes through its own path.
        if !matches!(self.suspended.last().map(|s| &s.reason), Some(SuspendReason::Yield)) {
            return Ok(());
        }
        // Unlike `drive_resume`, do NOT clear output/status: a Yield is mid-execution, not a fresh
        // pass, so PR/MSG output must accumulate across slices. Otherwise this mirrors
        // `drive_resume`: resume the active frame with a fresh `budget`, and CASCADE outward — if a
        // sliced nested `RUN` finishes, resume the outer frame that was parked on it (continuing
        // after the `RUN`) — stopping when a frame yields/suspends again or the stack drains.
        while let Some(s) = self.suspended.pop() {
            let SuspendedScript { program, ip, frames, rv, .. } = s;
            let mut pending: Vec<SuspendedScript> = Vec::new();
            let mut exit_all = false;
            let outcome = {
                let mut ctx = ExecCtx {
                    env: &mut self.env,
                    host,
                    library: &mut self.library,
                    lib_loads: &mut self.lib_loads,
                    output: &mut self.output,
                    status: &mut self.last_status,
                    pending: &mut pending,
                    exit_all: &mut exit_all,
                    err_line: 0,
                    err_located: false,
                    cur_command: String::new(),
                    cur_file: None,
                    err_logged: false,
                };
                resume_program(&program, &mut ctx, ip, frames, rv, Some(budget))?
            };
            if exit_all {
                self.suspended.clear();
                return Ok(());
            }
            match outcome {
                // Finished: cascade — resume the next (outer) frame after its RUN.
                ExecOutcome::Done { .. } => {}
                // Paused again (yielded or a real WAIT/INPUT): re-park this frame plus any nested
                // it spawned (innermost last) and stop.
                ExecOutcome::Suspended { ip, frames, rv, reason } => {
                    self.suspended.push(SuspendedScript { program, ip, frames, rv, reason });
                    pending.reverse();
                    self.suspended.extend(pending);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Whether a script is parked on a preemption [`SuspendReason::Yield`] (vs. `WAIT`/`INPUT`/…),
    /// i.e. it should be resumed with a fresh budget via [`resume_yield`].
    pub fn is_yielded(&self) -> bool {
        matches!(self.suspended.last().map(|s| &s.reason), Some(SuspendReason::Yield))
    }
}

/// The mutable execution context shared by the top-level run and any nested
/// `RUN`. Distinct disjoint borrows so they can be held together.
struct ExecCtx<'h> {
    env: &'h mut Env,
    host: &'h mut dyn Host,
    library: &'h mut Library,
    lib_loads: &'h mut Vec<String>,
    output: &'h mut Vec<String>,
    status: &'h mut Option<String>,
    /// Continuations parked by nested `RUN`/`RUNCODE` scripts that suspended, innermost-first.
    /// Collected after the run into the interpreter's continuation stack (`Interpreter::suspended`).
    pending: &'h mut Vec<SuspendedScript>,
    /// Set by `EXITALL`/`EX1` to abort EVERY in-process script: it propagates out through nested
    /// `RUN`s (each aborts rather than continuing after the `RUN`) and drops the whole continuation
    /// stack. Shared across nested executes so a deep `EXITALL` reaches the top.
    exit_all: &'h mut bool,
    /// The source line of the command currently executing (updated per command). When a `RUN`-ed
    /// file errors, `exec_run` reads this to report `<file>:<line>: …`. Shared across nested executes
    /// (the innermost run overwrites it), so it names the file+line where the failure actually is.
    err_line: usize,
    /// Set once `exec_run` has stamped an error with `<file>:<line>` — so an OUTER `RUN` in the same
    /// chain doesn't re-stamp it with the wrong (outer) file/line. Reset with each top-level run.
    err_located: bool,
    /// The verb + raw args of the command currently executing — for the error log's `command` column.
    /// Updated per command; a fold site (`fail_trap`) or the fatal hook reads it. Shared across nested
    /// executes, so it names the actual failing command.
    cur_command: String,
    /// The source file the current run is executing (a `RUN` path or the top-level `--run`/startup
    /// label) — for the error log's `location` column. `None` for an ad-hoc command line. Saved and
    /// restored around a nested `RUN` so the outer file is not left stale.
    cur_file: Option<String>,
    /// Set once a FATAL error has been written to the error log, so it is logged exactly once at its
    /// origin and not again as it unwinds through outer `RUN` frames. Reset with each top-level run.
    err_logged: bool,
}

/// Run a lowered program against `ctx` with positional `args`, from the top.
fn execute(
    program: &ir::Program,
    ctx: &mut ExecCtx,
    args: &[Value],
    budget: Option<u32>,
) -> VmResult<ExecOutcome> {
    let vm = Vm {
        code: &program.code,
        subs: &program.subs,
        ctx,
        frames: vec![Frame { return_ip: usize::MAX, args: args.to_vec(), locals: Vec::new() }],
        rv: Value::str(""),
        ip: 0,
        suspend: None,
        budget,
    };
    vm.finish()
}

/// Resume a parked program from a saved `(ip, frames, rv)` state.
fn resume_program(
    program: &ir::Program,
    ctx: &mut ExecCtx,
    ip: usize,
    frames: Vec<Frame>,
    rv: Value,
    budget: Option<u32>,
) -> VmResult<ExecOutcome> {
    let vm =
        Vm { code: &program.code, subs: &program.subs, ctx, frames, rv, ip, suspend: None, budget };
    vm.finish()
}

/// A call frame.
struct Frame {
    /// Where to resume after `RETURN` (`usize::MAX` for the top-level frame).
    return_ip: usize,
    /// Positional arguments `$1..$N` for this frame.
    args: Vec<Value>,
    /// Variables saved by `LOCAL` in this frame, restored on return: `(folded
    /// name, previous value or None if it was unset)`.
    locals: Vec<(String, Option<Value>)>,
}

struct Vm<'a, 'h> {
    code: &'a [Instr],
    subs: &'a std::collections::HashMap<String, usize>,
    ctx: &'a mut ExecCtx<'h>,
    frames: Vec<Frame>,
    rv: Value,
    ip: usize,
    /// Set by a `READKEY`/`INPUT`/`WAIT` command to pause the run; consumed by `finish`.
    suspend: Option<SuspendReason>,
    /// Remaining instruction budget for this slice (`None` = unlimited, the normal
    /// foreground run). When it reaches 0 the loop yields (`SuspendReason::Yield`) so a
    /// background (`RUN/BG`) task can't freeze the editor with a tight CPU loop.
    budget: Option<u32>,
}

/// How a `Command` dispatch affects control flow.
enum Flow {
    /// Advance to the next instruction.
    Next,
    /// `ip` was already set (e.g. `CALL`).
    Jumped,
    /// End the script.
    Halt,
    /// Pause the script for user input (`READKEY`/`INPUT`). `self.suspend` carries
    /// the request; the run loop advances `ip` past this command and returns so the
    /// continuation resumes *after* it.
    Suspend,
}

/// What a parked script is waiting for — surfaced to the editor/GUI so it can
/// capture the next keystroke (`Key`) or a typed line (`Line`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// `READKEY`/`RK` — one keystroke.
    Key,
    /// `INPUT` — a line of text (entered on the command line).
    Line,
}

/// A request raised when a script suspends on `READKEY`/`INPUT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    pub kind: InputKind,
    /// The `INPUT` prompt (always `None` for `READKEY`).
    pub prompt: Option<String>,
    /// `READKEY/D` — the destination variable should get a readable key DESCRIPTION (`Ctrl+Y`,
    /// `Shift+F3`, `Enter`, `a`) rather than the raw character. Always `false` for `INPUT`.
    pub describe: bool,
}

/// What a `WAIT` is parked on — surfaced to the editor, which resumes the script
/// (with no value) once the condition holds. Non-freezing: the GUI keeps rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitTarget {
    /// `WAIT` — until all background (`X/BG`) jobs have finished.
    AllJobs,
    /// `WAIT/J <id>` — until that one background job has finished.
    Job(u64),
}

/// Why a script is parked mid-run (drives how the editor resumes it).
enum SuspendReason {
    /// `READKEY <var> [message]` — the keystroke char is written into `var` (`$SC` gets the
    /// scancode). `message`, if given, is shown in a modal box while awaiting the key. `describe`
    /// (`READKEY/D`) writes a readable key description into `var` instead of the raw character.
    ReadKey { var: String, message: Option<String>, describe: bool },
    /// `INPUT` — the typed line is written into `var`; `prompt` is shown.
    InputLine { var: String, prompt: Option<String> },
    /// `WAIT`/`WAIT/J` — resumed (no value) when the awaited background jobs finish.
    Wait(WaitTarget),
    /// `WAIT <ms>` — a NON-freezing timed pause: the editor resumes (no value) once the
    /// interval elapses, so the GUI renders the current state and stays live meanwhile.
    Sleep(u64),
    /// `TXT [/T:"title"] <text>` — a MODAL scrollable text dialog: the script pauses until
    /// the user dismisses it (Esc / ×), then resumes (no value). `title` `None` = default.
    /// `columns` is an optional render hint: when `Some`, the GUI shows a two-column
    /// (name, description) grid that wraps within each column instead of plain lines.
    /// `text` always carries the flat form too (used by CAPTURE, copy, and tests).
    ShowText {
        title: Option<String>,
        text: String,
        columns: Option<Vec<(String, String)>>,
        /// Optional per-line label colours (one per line of `text`; `None` = default) — `HLKINDS`.
        line_colors: Option<Vec<Option<(u8, u8, u8)>>>,
    },
    /// Bare `MSG <text>` — a MODAL message box: the script pauses until the user dismisses it
    /// (OK / Esc / ×), then resumes (no value). `MSG/T:<secs>` does NOT use this — it's a
    /// non-blocking timed toast (handled by `EditorOp::ShowMessage`).
    ShowMessage { text: String },
    /// `PICK [/I] <var> ["title"] <list-expr>` — a MODAL list chooser. The script pauses; the editor
    /// shows the `items` (filterable) and resumes via [`resume_pick`](Interp::resume_pick): the chosen
    /// item's string (or its 1-based index when `index_mode`) lands in `var`, `$ERROR` clears; on
    /// cancel `var` gets `""` (or `0`) and `$ERROR` is set.
    Pick { var: String, title: Option<String>, items: Vec<String>, index_mode: bool },
    /// Enhanced `CP` with no local selection: a MODAL chooser (sharing the PICK dialog) of the
    /// selections in OTHER tabs. `sources[i]` is the buffer index behind row `i`; on resume the chosen
    /// buffer's selection is inserted at the active caret. Cancel sets `$ERROR`.
    CpPick { title: Option<String>, items: Vec<String>, sources: Vec<usize> },
    /// `REOPEN` — a MODAL chooser (sharing the PICK dialog) of recently-closed files. `entries[i]` is
    /// the `(path, caret)` behind row `i`; on resume the chosen file is reopened with its caret
    /// restored. Cancel sets `$ERROR`.
    ReopenPick { title: Option<String>, items: Vec<String>, entries: Vec<(String, usize)> },
    /// `LISTTABS` — a MODAL chooser (sharing the PICK dialog) of the open tabs. `targets[i]` is the
    /// buffer index behind row `i`; on resume the chosen tab is switched to. Cancel leaves the active
    /// tab unchanged (and sets `$ERROR`).
    TabPick { title: Option<String>, items: Vec<String>, targets: Vec<usize> },
    /// `MARKLIST` — a MODAL chooser (sharing the PICK dialog) of the active document's bookmarks.
    /// `targets[i]` is the byte offset behind row `i`; on resume the caret jumps there (centered).
    /// Cancel leaves the caret unchanged (and sets `$ERROR`).
    MarkPick { title: Option<String>, items: Vec<String>, targets: Vec<usize> },
    /// `LINTFIX` — a MODAL chooser (sharing the PICK dialog) of a language issue's corrections. On
    /// resume the chosen `replacements[i]` replaces the document `span`. Cancel leaves it unchanged.
    LintFixPick {
        title: Option<String>,
        items: Vec<String>,
        span: (usize, usize),
        replacements: Vec<String>,
    },
    /// `BK on` / `BK debug` — an inline breakpoint. The script pauses; the editor shows a prompt
    /// (`debug` = a variable-dump dialog, else a simple Y/N stop prompt) and either RESUMES the
    /// script (continue) or ABORTS it. `BK off` doesn't suspend at all. `line`/`col` locate the `BK`
    /// in the script (1-based; `col` 0 = unknown) for the dialog header.
    Breakpoint { debug: bool, line: usize, col: usize },
    /// PREEMPTION (not a verb): the instruction budget for a sliced run elapsed. The continuation
    /// resumes at the SAME `ip` with a fresh budget — used by background (`RUN/BG`) tasks so they
    /// run cooperatively without freezing the editor.
    Yield,
    /// PROPAGATION (not a verb): a nested `RUN`/`RUNCODE` script suspended. This (outer) frame is
    /// parked only so the resume cascade can finish the nested run and then continue here after the
    /// `RUN`. The editor never sees this reason — the innermost frame carries the real one.
    NestedRun,
}

/// A parked continuation: everything needed to resume ONE suspended script (one frame of the
/// interpreter's continuation stack). The owned [`ir::Program`] keeps the borrowed `code`/`subs`
/// alive across the pause; `ip`/`frames`/`rv` are the complete VM state. A pause inside a nested
/// `RUN`/`RUNCODE` parks one of these per nesting level (the outer ones carry `NestedRun`), and the
/// resume cascade drives them inner-to-outer — see `run_source_sync`/`drive_resume`.
struct SuspendedScript {
    program: ir::Program,
    ip: usize,
    frames: Vec<Frame>,
    rv: Value,
    reason: SuspendReason,
}

/// How one `execute`/`resume` of a (sub)program settled.
enum ExecOutcome {
    /// Ran to completion (EXIT/end-of-code/top-level RETURN). Carries the final `RETURN` value so a
    /// routine invoked as a command (the nested-run path) can propagate it to the caller's `$RV`.
    Done { rv: Value },
    /// Paused on `READKEY`/`INPUT`/`WAIT`; carries the state to park.
    Suspended { ip: usize, frames: Vec<Frame>, rv: Value, reason: SuspendReason },
}

impl Vm<'_, '_> {
    /// Run to completion (or suspension) and report how it settled. Consumes the VM
    /// so its `frames`/`rv` can move into a parked [`SuspendedScript`].
    fn finish(mut self) -> VmResult<ExecOutcome> {
        self.run_loop()?;
        Ok(match self.suspend.take() {
            Some(reason) => {
                ExecOutcome::Suspended { ip: self.ip, frames: self.frames, rv: self.rv, reason }
            }
            None => ExecOutcome::Done { rv: self.rv },
        })
    }

    fn run_loop(&mut self) -> VmResult<()> {
        while self.ip < self.code.len() {
            // Preemption: a sliced run (a `RUN/BG` background task) yields after a fixed number of
            // instructions, so a tight CPU loop with no `WAIT` still interleaves with the editor.
            // Yield BEFORE fetching the instruction, so the continuation resumes at this same `ip`.
            if let Some(b) = self.budget.as_mut() {
                if *b == 0 {
                    self.suspend = Some(SuspendReason::Yield);
                    return Ok(());
                }
                *b -= 1;
            }
            // Clone the current instruction so the `&self.code` borrow ends
            // before we mutate VM state. Programs are small; this is fine.
            let instr = self.code[self.ip].clone();
            match instr {
                Instr::Exit => break,
                // EXITALL: end this script AND signal every enclosing/parked script to abort.
                Instr::ExitAll => {
                    *self.ctx.exit_all = true;
                    break;
                }
                Instr::Jump(t) => {
                    self.ip = t;
                }
                Instr::JumpUnless { cond, target, .. } => {
                    // A trappable condition error (e.g. a missing `READFILE`) sets `$ERROR` and counts
                    // as false, so the block is skipped rather than the script halting.
                    let truthy =
                        self.eval_expr_trap(&cond)?.map(|v| v.is_truthy()).unwrap_or(false);
                    if truthy {
                        self.ip += 1;
                    } else {
                        self.ip = target;
                    }
                }
                Instr::SetVar { name, expr, .. } => {
                    if is_special_name(&name) {
                        return Err(XplError::fatal(format!(
                            "cannot assign to read-only special ${name}"
                        )));
                    }
                    // A trappable error leaves the variable unchanged (and `$ERROR` set).
                    if let Some(v) = self.eval_expr_trap(&expr)? {
                        self.ctx.env.vars.insert(name, v);
                    }
                    self.ip += 1;
                }
                Instr::SetIndexVar { name, index, expr, .. } => {
                    if is_special_name(&name) {
                        return Err(XplError::fatal(format!(
                            "cannot assign to read-only special ${name}"
                        )));
                    }
                    if let (Some(idx), Some(val)) =
                        (self.eval_expr_trap(&index)?, self.eval_expr_trap(&expr)?)
                    {
                        let updated = set_index(self.ctx.env.vars.remove(&name), &idx, val, &name)?;
                        self.ctx.env.vars.insert(name, updated);
                    }
                    self.ip += 1;
                }
                Instr::Local { names, .. } => {
                    let mut saved = Vec::with_capacity(names.len());
                    for n in &names {
                        saved.push((n.clone(), self.ctx.env.vars.remove(n)));
                    }
                    self.frames.last_mut().unwrap().locals.extend(saved);
                    self.ip += 1;
                }
                Instr::Clear { names, .. } => {
                    for n in &names {
                        self.ctx.env.vars.remove(n);
                    }
                    self.ip += 1;
                }
                Instr::Return { value, .. } => {
                    if let Some(e) = value {
                        // A trappable error returns an empty value (and sets `$ERROR`).
                        self.rv = self.eval_expr_trap(&e)?.unwrap_or_else(|| Value::str(""));
                    }
                    if self.frames.len() <= 1 {
                        break; // RETURN at top level ends the script
                    }
                    let frame = self.frames.pop().unwrap();
                    self.restore_locals(frame.locals);
                    self.ip = frame.return_ip;
                }
                Instr::CaptureBegin { var, .. } => {
                    self.ctx.env.capture.push((var, String::new()));
                    self.ip += 1;
                }
                Instr::CaptureEnd => {
                    // Pop the buffer and assign it; tolerant of an empty stack (e.g. a capture
                    // whose buffer was lost when a mid-block suspend reconstructed the run).
                    if let Some((var, buf)) = self.ctx.env.capture.pop() {
                        self.ctx.env.vars.insert(var, Value::str(buf));
                    }
                    self.ip += 1;
                }
                Instr::Command { verb, raw_args, span } => {
                    self.ctx.err_line = span.line; // for `exec_run`'s `<file>:<line>` on failure
                    // Remember the triggering command for the error log (verb + raw args).
                    self.ctx.cur_command =
                        if raw_args.is_empty() { verb.clone() } else { format!("{verb} {raw_args}") };
                    match self.exec_command(&verb, &raw_args, span.line, span.col) {
                        Ok(Flow::Next) => self.ip += 1,
                        Ok(Flow::Jumped) => {}
                        Ok(Flow::Halt) => break,
                        // Pause: advance past the READKEY/INPUT so the resumed run
                        // continues *after* it, then stop the loop.
                        Ok(Flow::Suspend) => {
                            self.ip += 1;
                            break;
                        }
                        Err(e) => {
                            // Log a fatal once, at its origin, before it unwinds through outer frames.
                            if e.is_fatal() && !self.ctx.err_logged {
                                self.ctx.err_logged = true;
                                self.log_error_entry(true, 0, e.message());
                            }
                            return Err(e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_locals(&mut self, locals: Vec<(String, Option<Value>)>) {
        for (name, prev) in locals.into_iter().rev() {
            match prev {
                Some(v) => {
                    self.ctx.env.vars.insert(name, v);
                }
                None => {
                    self.ctx.env.vars.remove(&name);
                }
            }
        }
    }

    /// Evaluate an expression against the current scope. Any expression error is
    /// fatal (divide-by-zero, unknown function, bad arity — §8).
    fn eval_expr(&self, e: &Expr) -> VmResult<Value> {
        let frame = self.frames.last().expect("a frame is always present");
        let ctx = Ctx { env: self.ctx.env, host: &*self.ctx.host, args: &frame.args, rv: &self.rv };
        // Most expression errors are fatal programming bugs; a few (a `READFILE` of a missing file)
        // are marked trappable so a script can recover via `$ERROR` instead of halting.
        expr::eval(e, &ctx).map_err(|err| {
            if err.trappable {
                XplError::trappable(err.msg)
            } else {
                XplError::fatal(err.msg)
            }
        })
    }

    /// Like [`eval_expr`], but a TRAPPABLE error (e.g. `READFILE` of a missing file) sets `$ERROR` and
    /// yields `None`, so the calling instruction takes its empty/false path and the script keeps
    /// running. A FATAL error still propagates (halting the script). Mirrors `host_op`'s soft-fail.
    fn eval_expr_trap(&mut self, e: &Expr) -> VmResult<Option<Value>> {
        match self.eval_expr(e) {
            Ok(v) => Ok(Some(v)),
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                Ok(None)
            }
            Err(fatal) => Err(fatal),
        }
    }

    /// `CALC`: parse + evaluate an expression STRING against the current scope. Returns the value or a
    /// short error message (parse or eval failure) to show on the status line — never halts the script.
    fn calc_eval(&self, src: &str) -> Result<Value, String> {
        let src = src.trim();
        if src.is_empty() {
            return Err("empty expression".to_string());
        }
        let expr = crate::expr::parse(src).map_err(|e| e.msg)?;
        let frame = self.frames.last().expect("a frame is always present");
        let ctx = Ctx { env: self.ctx.env, host: &*self.ctx.host, args: &frame.args, rv: &self.rv };
        crate::expr::eval(&expr, &ctx).map_err(|e| e.msg)
    }

    /// The `DECIMALS` setting for formatting a `CALC` result (via the expression context).
    fn calc_decimals(&self) -> Option<u8> {
        let frame = self.frames.last().expect("a frame is always present");
        let ctx = Ctx { env: self.ctx.env, host: &*self.ctx.host, args: &frame.args, rv: &self.rv };
        ctx.float_decimals()
    }

    /// Substitute `$`-variables in raw verb-argument text.
    fn subst(&self, raw: &str) -> String {
        let frame = self.frames.last().expect("a frame is always present");
        let ctx = Ctx { env: self.ctx.env, host: &*self.ctx.host, args: &frame.args, rv: &self.rv };
        substitute(raw, &ctx)
    }

    /// `PRINTEX [<file>]`: expand the active document's `|expr|` soft-fields, then — with a `<file>`
    /// — write it (OS newlines) and open it in a new tab; with no file, drop the expanded text into
    /// a new, unnamed tab. Either way the result ends up in a tab (caret at the top).
    fn print_expand(&mut self, args: &str) -> VmResult<Flow> {
        let path = args.trim().to_string();
        let src = self.ctx.host.active_text();
        // Template-local variable scope: snapshot the user vars, run, then restore — so vars set in
        // live-code fields persist ACROSS fields during this run but are cleared when PRINTEX ends
        // (existing editor vars stay visible to the template and untouched afterward). Restored even
        // on error so a failed expansion can't leak the template's vars into the editor.
        let saved_vars = self.ctx.env.vars.clone();
        // Counter snapshot: `|Cn|` fields advance the document's `C1..C9` during expansion; snapshot
        // them now and restore below so the run renumbers from the template's seed and the template's
        // live counters aren't permanently advanced (the counter twin of the var save/restore).
        self.host_op(EditorOp::SaveCounters)?;
        let result = self.expand_fields(&src);
        self.host_op(EditorOp::RestoreCounters)?; // restore even if a field failed (no counter leak)
        self.ctx.env.vars = saved_vars;
        let (expanded, count) = result?;
        // OS-appropriate newlines (CRLF on Windows, LF elsewhere), regardless of the source's
        // endings or any mix introduced by expansion.
        let expanded = to_os_newlines(&expanded);
        let plural = if count == 1 { "" } else { "s" };
        if path.is_empty() {
            // No file: put the expanded text in a new, unnamed tab (caret at the top).
            self.host_op(EditorOp::NewDoc)?;
            self.host_op(EditorOp::InsertText(expanded))?;
            self.host_op(EditorOp::Move { motion: Motion::DocStart, extend: false })?;
            *self.ctx.status =
                Some(format!("PRINTEX: expanded {count} field{plural} to a new tab"));
            return Ok(Flow::Next);
        }
        // With a file: write it, then open it in a new tab.
        match self.ctx.host.write_file(&path, &expanded) {
            Ok(()) => {}
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                return Ok(Flow::Next); // don't try to open a file we failed to write
            }
            Err(fatal) => return Err(fatal),
        }
        self.host_op(EditorOp::Open { path: path.clone(), force: true, hex: false })?; // open the result, no prompt
        if self.ctx.env.error == 0 {
            *self.ctx.status =
                Some(format!("PRINTEX: expanded {count} field{plural} to {path} (opened)"));
        }
        Ok(Flow::Next)
    }

    /// `PRINTEX` expansion: copy `src` verbatim except for `|…|` soft-fields. A *substitution* field
    /// (the default) is run through substitution (`$vars`, `$(…)`, functions, binders) — so e.g.
    /// `|$YEAR-$MON-$MDAY|` becomes the date. A *live-code* field (top-level `;`, or a leading
    /// statement keyword) runs its XPL statements against the persistent template scope, the last
    /// `;`-segment supplying the output (expression → its value; statement → nothing, deleting the
    /// field) — these require `TRUST 1`. `\|` is a literal pipe; an unclosed `|` is emitted literally.
    /// Returns `(expanded, field_count)`. The caller snapshots/restores `env.vars` around this.
    fn expand_fields(&mut self, src: &str) -> VmResult<(String, usize)> {
        let trusted = self.trusted();
        let b = src.as_bytes();
        let n = b.len();
        let mut out = String::with_capacity(n);
        let mut i = 0;
        let mut count = 0;
        while i < n {
            match b[i] {
                b'\\' if i + 1 < n && b[i + 1] == b'|' => {
                    out.push('|'); // escaped literal pipe
                    i += 2;
                }
                b'|' => {
                    // Find the closing unescaped `|`.
                    let mut j = i + 1;
                    let mut close = None;
                    while j < n {
                        match b[j] {
                            b'\\' if j + 1 < n && b[j + 1] == b'|' => j += 2,
                            b'|' => {
                                close = Some(j);
                                break;
                            }
                            _ => j += 1,
                        }
                    }
                    if let Some(j) = close {
                        let inner = src[i + 1..j].replace("\\|", "|");
                        out.push_str(&self.expand_one_fence(&inner, trusted)?);
                        count += 1;
                        i = j + 1;
                    } else {
                        out.push('|'); // unclosed → literal
                        i += 1;
                    }
                }
                b'\\' => {
                    out.push('\\'); // lone backslash (not `\|`) → literal
                    i += 1;
                }
                _ => {
                    // Copy a verbatim run up to the next `|` or `\` (ASCII, so this stays on a
                    // char boundary — multibyte text is preserved).
                    let start = i;
                    while i < n && b[i] != b'|' && b[i] != b'\\' {
                        i += 1;
                    }
                    out.push_str(&src[start..i]);
                }
            }
        }
        Ok((out, count))
    }

    /// If a `PRINTEX` field's first word is a counter verb `C1`..`C9`, return its 0-based index and
    /// the rest of the field as a `$`-substituted counter argument (empty for a bare `|C1|`). The
    /// word match is case-insensitive (`|c1|` works like `|C1|`). `None` for any non-counter field.
    fn counter_fence(&self, inner: &str) -> Option<(u8, String)> {
        let t = inner.trim_start();
        let end = t.find(char::is_whitespace).unwrap_or(t.len());
        let word = t[..end].as_bytes();
        if word.len() == 2 && word[0].eq_ignore_ascii_case(&b'C') && (b'1'..=b'9').contains(&word[1]) {
            return Some((word[1] - b'1', self.subst(t[end..].trim())));
        }
        None
    }

    /// Whether `TRUST 1` is in effect (read via `$TR`) — gates live-code `PRINTEX` fields.
    fn trusted(&mut self) -> bool {
        matches!(self.ctx.host.special("TR"), Some(v) if v.to_str() == "1")
    }

    /// Expand one `|…|` field body (see `expand_fields`). A substitution field runs through `subst`;
    /// a live-code field runs its statements (trust-gated) and yields the last `;`-segment's value.
    fn expand_one_fence(&mut self, inner: &str, trusted: bool) -> VmResult<String> {
        // A third field kind: a *counter field* `|Cn …|` advances and renders a `C1..C9` register
        // (full `Cn` grammar). Bounded and safe — unlike live code it needs no TRUST — so check it
        // before the live/substitution split. The arg is already `$`-substituted by `counter_fence`.
        if let Some((idx, arg)) = self.counter_fence(inner) {
            return self.host_op_value(EditorOp::CounterField { idx, arg }).map(|v| v.to_str());
        }
        if !fence_is_live(inner) {
            return Ok(self.subst(inner)); // plain substitution — no side effects, no trust needed
        }
        if !trusted {
            return Err(XplError::trappable(
                "PRINTEX: this document has live code in a |…| field; enable it with TRUST 1",
            ));
        }
        let segs = crate::splitter::split_commands_indexed(inner);
        let Some((last_start, last_chunk)) = segs.last() else {
            return Ok(String::new()); // whitespace-only field
        };
        let last = last_chunk.trim();
        // A trailing block-closer / statement keyword (`ef`, `endif`, `set …`, …) parses as a
        // bare-word EXPRESSION, which would make us peel it off and run an unbalanced block. Only
        // treat the last segment as a value when it ISN'T a statement keyword.
        let last_kw = last.split([' ', '\t']).next().unwrap_or("").to_ascii_uppercase();
        let last_is_stmt = crate::library::CONTROL_KEYWORDS.contains(&last_kw.as_str());
        if !last_is_stmt && crate::expr::parse(last).is_ok() {
            // Statements (if any) before the final expression; then the expression supplies the value.
            let before = &inner[..*last_start];
            if !before.trim().is_empty() {
                self.run_source_sync(before, &[], false)?;
            }
            Ok(self.subst(&format!("$({last})")))
        } else {
            // The whole field is statements — run it (full parse, so a single-line IF pairs up);
            // the last statement has no value, so the field is deleted.
            self.run_source_sync(inner, &[], false)?;
            Ok(String::new())
        }
    }

    /// `LDTX`/`LDSC`/`LDPM`: parse `<slot> [<source>]` and load a binder slot. The source
    /// is chosen by the first non-blank char of the remainder: absent → the current
    /// selection; `"…"` → a quoted literal (escapes processed, like `INSERT`); otherwise →
    /// a file path (read by the host, resolved against the cwd).
    fn exec_load_binder(&mut self, script: bool, args: &str) -> VmResult<()> {
        let t = args.trim_start();
        let end = t.find(char::is_whitespace).unwrap_or(t.len());
        let slot_str = &t[..end];
        let rest = t[end..].trim();
        let mut sc = slot_str.chars();
        let slot = match (sc.next(), sc.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase(),
            _ => {
                // Bad slot is trappable (set $ERROR + status, continue) — mirror `host_op`'s
                // trappable handling rather than returning a fatal error.
                let verb = if script { "LDSC" } else { "LDTX" };
                *self.ctx.status = Some(format!(
                    "{verb}: slot must be a single letter A-Z or digit 0-9 (got {slot_str:?})"
                ));
                self.ctx.env.error = 1;
                return Ok(());
            }
        };
        let source = if rest.is_empty() {
            BinderSource::Selection
        } else if rest.starts_with('"') {
            BinderSource::Literal(literal_text(rest))
        } else {
            BinderSource::File(rest.to_string())
        };
        self.host_op(EditorOp::SetBinder { script, slot, source })
    }

    /// Dispatch a `Command`. `verb` is already case-folded.
    fn exec_command(&mut self, verb: &str, raw_args: &str, line: usize, col: usize) -> VmResult<Flow> {
        // Peel any switches attached to the verb (`COPY/NV`, `SE/W`, `CD-d`) — the
        // uniform XyScript switch syntax. `base` drives dispatch; `vswitches` are
        // the attached switch names.
        let (base, vswitches) = crate::switches::peel_verb(verb);

        // Substitute `$`-variables in the args *before* clearing `$ERROR`, so a
        // command that reads `$ERROR` (e.g. `PR $ERROR`) sees the value as it was
        // on entry. (Search parses from `raw_args` instead — see below.)
        let mut args = self.subst(raw_args);

        // Point-and-shoot: a file-argument command with NO argument, issued while the caret sits on a
        // file in a DIR listing, operates on THAT file (XyWrite-style). The host returns a path only
        // when the active buffer is a listing and the caret is on a real entry; the file commands all
        // take the whole trimmed args as the path, so the raw (unquoted) path drops straight in.
        if args.trim().is_empty() && POINT_AND_SHOOT_VERBS.iter().any(|v| v.eq_ignore_ascii_case(base)) {
            if let Some(path) = self.ctx.host.point_and_shoot_file() {
                args = path;
            }
        }

        // Single-path file verbs accept an optional surrounding pair of quotes around the path (so a
        // path with spaces can be quoted, and a habitual `OPEN "…"` just works). Strip it here, once,
        // for the whole class — WITHOUT escape-processing, so `"C:\Users\me"` keeps its backslashes.
        // (Two-path/tokenizing verbs handle their own quoting; point-and-shoot paths carry no quotes.)
        if SINGLE_PATH_VERBS.iter().any(|v| v.eq_ignore_ascii_case(base)) {
            let unq = unquote_path(&args);
            if unq.len() != args.len() {
                args = unq.to_string();
            }
        }

        // Action verbs clear $ERROR on entry; exempt verbs manage it themselves. A bare library-
        // routine invocation (`foo` ≡ `CALL foo`) is exempt too, so it matches `CALL` (which is in
        // NO_ERROR_TOUCH) — the routine's own first action verb decides $ERROR.
        if !NO_ERROR_TOUCH.contains(&base) && !self.subs.contains_key(base) {
            self.ctx.env.error = 0;
        }

        // Search / replace. Switches are flags-first: attached to the verb (`SE/W`)
        // or leading before the self-delimiting pattern (`SE /W "x"`, `SE -w "x"`).
        // Parsed from the RAW args so a regex/perl replacement keeps its `$1` capture
        // references — `$`-substitution applies to the find pattern, and to the
        // replacement only when it isn't a regex or `/P` pattern (where `$1` is
        // literal text).
        if is_search_base(base) {
            let mut spec = parse_search(base, &vswitches, raw_args)?;
            spec.find = self.subst(&spec.find);
            if !spec.regex && !spec.perl {
                if let Some(r) = spec.replace.take() {
                    spec.replace = Some(self.subst(&r));
                }
            }
            // A replace (`CH`/`CI` family) returns a "N changes" tally, which `host_op` surfaces on the
            // status line (the caret stayed put, so the count is the only feedback). A plain search
            // returns "" and shows nothing.
            self.host_op(EditorOp::Search(spec))?;
            return Ok(Flow::Next);
        }

        // SORT over the selection. Its switches carry values (`/F:n`, `/N:n`), so it
        // parses them itself before the generic `validate_switches` (which only knows
        // valueless switch names) would reject them.
        if base == "SORT" {
            let spec = parse_sort(&vswitches, &args)?;
            self.host_op(EditorOp::Sort(spec))?;
            return Ok(Flow::Next);
        }

        // DIR/LS: parse its own (multi-letter, order/attr) switches before the generic
        // validation, then take the remaining token as an optional path/filespec.
        if base == "DIR" || base == "LS" {
            // `/W` opens the listing in a separate window; `/H` `/V` open it in a new split pane.
            // Neither is a view option, so peel them off before parsing the DirOpts switches.
            let window = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
            let split = pane_split_switch(&vswitches);
            let vsw: Vec<String> = vswitches
                .iter()
                .filter(|s| {
                    !s.eq_ignore_ascii_case("W")
                        && !s.eq_ignore_ascii_case("H")
                        && !s.eq_ignore_ascii_case("V")
                })
                .cloned()
                .collect();
            // Fold in the `DIRSET` defaults for the switch categories the command didn't specify.
            // Applies to every open path (tab / `DIR/W` window / split pane) since it happens before
            // the branch below, and only touches sort/filter/view — `/W`/`/H`/`/V` were peeled off.
            let vsw = merge_dir_defaults(&vsw, &self.ctx.host.dir_default_switches());
            let (opts, rest) = parse_dir(&vsw, &args)?;
            // The path after DIR's switches may be quoted (spaces, or habit) — strip one surrounding
            // pair, raw (no escape-processing), like the other single-path file verbs.
            let p = unquote_path(&rest);
            let path = if p.is_empty() { None } else { Some(p.to_string()) };
            // Under CAPTURE, render the listing to text into the buffer instead of opening it.
            if self.is_capturing() {
                let v = self.host_op_value(EditorOp::DirToString { path, opts })?;
                self.capture_push(&v.to_str());
            } else if window {
                self.host_op(EditorOp::DirWindow { path, opts })?; // a separate window; split N/A
            } else {
                // `/H` `/V` — put the listing in a NEW split pane (split first, then list into it).
                if let Some(stacked) = split {
                    self.host_op(EditorOp::PaneSplitNew { stacked })?;
                }
                self.host_op(EditorOp::Dir { path, opts })?;
            }
            return Ok(Flow::Next);
        }

        // GREP: ripgrep-style recursive search into a results tab. Custom argument parsing (a pattern
        // plus an optional search path/glob, the pattern quotable for spaces), like DIR, so it's
        // handled before the generic switch validation. `/R` regex, `/I` case-insensitive, `/W`
        // whole-word — the same flag letters as the FIND family.
        if base == "GREP" {
            let regex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("R"));
            let case = vswitches.iter().any(|s| s.eq_ignore_ascii_case("I"));
            let whole = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
            let perl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("P"));
            let (pattern, path) = parse_grep_args(&args);
            if pattern.is_empty() {
                // Bare `GREP` (no pattern) opens the GREP dialog — the interactive companion to FIND.
                self.host_op(EditorOp::OpenGrepDialog)?;
                return Ok(Flow::Next);
            }
            self.host_op(EditorOp::Grep { pattern, path, regex, case, whole, perl })?;
            return Ok(Flow::Next);
        }
        // GJ: point-and-shoot on a GREP results tab — open + jump to the hit under the caret.
        if base == "GJ" {
            self.host_op(EditorOp::GrepJump)?;
            return Ok(Flow::Next);
        }

        // Binder refs in command position: `@X` inserts its stored text, `&X` runs its
        // stored XPL (X = A-Z/0-9; `base` is already upper-cased). Any trailing args are
        // ignored. (As a *value* — `PR @A`, `SE @A` — binder-refs are handled by the arg/
        // expr layer, a later phase, not here.)
        if let Some((script, slot)) = binder_ref(base) {
            self.host_op(EditorOp::UseBinder { script, slot })?;
            return Ok(Flow::Next);
        }

        // Reject switches a command doesn't declare (keeps `INSERT/X` from silently
        // doing nothing). Commands that accept switches validate/consume them below.
        validate_switches(base, &vswitches)?;

        // Editing/navigation verbs that should act on the document: if the CMline is focused, shift
        // focus to the document first (a no-op otherwise — and `XC`/SUBMIT's transient document
        // target is left alone, since the host checks the live focus). A library routine invoked by
        // name is a script too, so it gets the same treatment.
        if FOCUS_DOC_VERBS.contains(&base) || self.subs.contains_key(base) {
            self.host_op(EditorOp::EnsureDocumentFocus)?;
        }

        match base {
            "LOADKBD" => {
                let path = args.trim().to_string();
                self.host_call(|h| h.load_kbd(&path))?;
                // Confirm on success ($ERROR is cleared on entry and set by a failed load). A FAILURE
                // sets the transient status here AND is recorded on the host (`kbd_load_error`) so it
                // survives to the end of a `startup.xpl` — where later commands (and nested `LOADCFG`/
                // `THEME` runs) would otherwise overwrite/clear the status, hiding why there's no keyboard.
                if self.ctx.env.error == 0 {
                    *self.ctx.status = Some(format!("loaded keyboard from {path}"));
                }
                Ok(Flow::Next)
            }
            // Reset the keyboard to the built-in baseline.
            "CLEARKBD" => {
                self.host_op(EditorOp::ClearKbd)?;
                Ok(Flow::Next)
            }
            // `KBDDOC [<file>]` — annotate a KBD file with a GETKEY key-name column (`unmod Esc :1=KF`),
            // so its scan codes read at a glance; `LOADKBD` ignores the annotations. Runs the embedded
            // script (kbddoc.xpl): no arg annotates the current tab; `<file>` opens/blind-edits + saves.
            "KBDDOC" => {
                let arg = args.trim();
                let arg = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')).unwrap_or(arg);
                let sub_args: Vec<Value> =
                    if arg.is_empty() { Vec::new() } else { vec![Value::str(arg)] };
                self.run_source_sync(KBDDOC_SRC, &sub_args, false)
            }
            // Dump the currently-loaded keymap to a KBD file (inverse of LOADKBD); no path = default.
            "SAVEKBD" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveKbdToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let p = args.trim();
                    let arg = if p.is_empty() { None } else { Some(p.to_string()) };
                    self.host_op(EditorOp::SaveKbd(arg))?;
                }
                Ok(Flow::Next)
            }
            // `WRITE`/`WR` are synonyms for `INSERT` (insert the literal text at the caret,
            // processing `\n`/`\t` escapes). `WRITELN`/`WRL` do the same but add a trailing newline
            // in the document's line-ending style (`Newline`), not a bare LF.
            "INSERT" | "WRITE" | "WR" => {
                self.host_op(EditorOp::InsertText(literal_text(&args)))?;
                Ok(Flow::Next)
            }
            "WRITELN" | "WRL" => {
                self.host_op(EditorOp::InsertText(literal_text(&args)))?;
                self.host_op(EditorOp::Newline)?;
                Ok(Flow::Next)
            }
            // `PUT <text>` — the cursor-destination twin of `PR`: instead of printing the
            // (already `$`-substituted) argument to the status line, insert it at the caret of
            // whichever has focus (the CMline or the active document — `InsertText` routes through
            // the focus target). Args are taken RAW like `PR` (no quote-stripping or `\n` escape
            // processing — that's what `INSERT` does), so `PUT $var` inserts the variable's value.
            "PUT" => {
                self.host_op(EditorOp::InsertText(args))?;
                Ok(Flow::Next)
            }
            // `WRITEFILE <path> <content>` / `WF` — write `content` (taken RAW after `$`-substitution,
            // like `PUT` — so `WF out.txt $x` writes the variable's value verbatim) to a file,
            // OVERWRITING it. The path is the first token (quote it for spaces); paths resolve against
            // `$CWD`. Reads back with the `READFILE()` function.
            "WRITEFILE" | "WF" => {
                let (path, content) = split_path_and_rest(&args)?;
                self.ctx.host.write_file(&path, &content)?;
                Ok(Flow::Next)
            }
            // Counters `C1`..`C9` (XyWrite-style, per-document). `Cn <seed>` configures (number /
            // letter / Roman+`r` + optional decorator); bare `Cn` puts the value and advances;
            // `Cn +n` / `Cn -[n]` / `Cn =` put relative to the last value. `$C1`..`$C9` peek.
            "C1" | "C2" | "C3" | "C4" | "C5" | "C6" | "C7" | "C8" | "C9" => {
                let idx = base.as_bytes()[1] - b'1'; // '1'→0 .. '9'→8
                self.host_op(EditorOp::Counter { idx, arg: args })?;
                Ok(Flow::Next)
            }
            // `PRINTEX [<output_file>]` — "print + expand": copy the active document, expanding each
            // `|expr|` soft-field through substitution (`$vars`, `$(…)`, functions) — so a doc can
            // carry self-updating values like `|$YEAR-$MON-$MDAY|`. With a file, write it (OS
            // newlines) and open it in a new tab; with no file, drop the result in a new unnamed tab.
            "PRINTEX" => self.print_expand(&args),
            // `MERGE`/`ME <file>` — insert the file's contents at the caret (XyWrite "merge").
            "MERGE" | "ME" => {
                self.host_op(EditorOp::Merge(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            // Auto-replace (typo-fix / snippet expansion). `AR` replaces the word behind the caret
            // when it's a loaded key (typically bound, e.g. `57=AR;" "` for Shift+Space). `LoadAR`/
            // `SAVEAR`/`CLEARAR` manage the in-memory `key=replacement` map. `$AR` = pair count.
            "AR" => {
                self.host_op(EditorOp::AutoReplace)?;
                // A replacement value beginning with `#` is an XPL ACTION: the host deleted the
                // matched key and queued the XPL here. Run it now, then HALT — so a bound chain like
                // `AR;" "` stops after the action (no trailing space). A plain text replacement (or
                // no match) leaves nothing queued and the chain continues.
                if let Some(xpl) = self.ctx.host.take_ar_xpl() {
                    self.run_source_sync(&xpl, &[], false)?;
                    return Ok(Flow::Halt);
                }
                Ok(Flow::Next)
            }
            "LOADAR" => {
                self.host_op(EditorOp::LoadAutoReplace(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "SAVEAR" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveAutoReplaceToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let a = args.trim();
                    let arg = if a.is_empty() { None } else { Some(a.to_string()) };
                    self.host_op(EditorOp::SaveAutoReplace(arg))?;
                }
                Ok(Flow::Next)
            }
            "CLEARAR" => {
                self.host_op(EditorOp::ClearAutoReplace)?;
                Ok(Flow::Next)
            }
            // Binder loaders: `LDTX <slot> [<source>]` → text binder `@slot`; `LDSC`/`LDPM`
            // → script binder `&slot`. Source = selection (no arg) / quoted literal / file.
            // NB: use the RAW (un-substituted) args so the content is stored LITERALLY — a
            // stored `$x`/`@A`/`&B` is kept verbatim (expanded/run only when the binder is
            // used), which is also what the persistence round-trip needs.
            "LDTX" => {
                self.exec_load_binder(false, raw_args)?;
                Ok(Flow::Next)
            }
            "LDSC" | "LDPM" => {
                self.exec_load_binder(true, raw_args)?;
                Ok(Flow::Next)
            }
            // Deletion.
            // Character delete: `BD` (backspace) / `RC` (rub-out forward) are the QN
            // names. (`BS` is bottom-of-screen motion, not backspace; `DEL` forward.)
            "BD" => {
                self.host_op(EditorOp::Delete(DeleteKind::Back))?;
                Ok(Flow::Next)
            }
            "RC" => {
                self.host_op(EditorOp::Delete(DeleteKind::Forward))?;
                Ok(Flow::Next)
            }
            // `DEL <path>` confirms before deleting; `DEL/NV <path>` (and the synonym `ERNV`) delete
            // without confirmation. It's a file-delete verb requiring a path (the host errors on an
            // empty one). The editor's forward character-delete is `RC`, not bare DEL.
            "DEL" => {
                let path = args.trim().to_string();
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV")) {
                    self.host_op(EditorOp::FileDelete(path))?;
                } else {
                    self.host_op(EditorOp::FileDeleteConfirm(path))?;
                }
                Ok(Flow::Next)
            }
            "ERNV" => {
                // XyWrite "erase, no verify" — same as `DEL/NV`.
                self.host_op(EditorOp::FileDelete(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "RENAME" | "REN" => {
                let (from, to) = parse_two_paths(base, &args)?;
                self.host_op(EditorOp::FileRename { from, to })?;
                Ok(Flow::Next)
            }
            // Shell-out (trust-gated in the host). `X`/`XY`/`XV` consume the rest of the
            // line (self-delimiting in the splitter) and substitute `$`-vars.
            "RUNHERE" => {
                self.host_op(EditorOp::RunHere)?;
                Ok(Flow::Next)
            }
            // Date / time / stopwatch (B12). The component fields live in `$YEAR`/`$MON`/…;
            // these verbs insert/report via those specials.
            "DA" => {
                let d = self.special_str("DA");
                let t = self.special_str("TM");
                self.host_op(EditorOp::InsertText(format!("{d} {t}")))?;
                Ok(Flow::Next)
            }
            "ET" => {
                let e = self.special_str("ET");
                self.host_op(EditorOp::InsertText(e))?;
                Ok(Flow::Next)
            }
            "ZT" => {
                self.host_op(EditorOp::ResetStopwatch)?;
                Ok(Flow::Next)
            }
            "WAIT" => {
                // Three forms:
                //   `WAIT/J <id>` (or `WAIT /J <id>`) — suspend until that background job ends;
                //   `WAIT` (bare)                     — suspend until ALL background jobs end;
                //   `WAIT <ms>`                       — a NON-freezing timed pause.
                // All three park the script (Flow::Suspend); the editor resumes it when the
                // condition holds, so the GUI keeps rendering throughout.
                let (leading, rest) = crate::switches::take_leading(&args, |n| n == "J");
                let job_form = vswitches.iter().any(|s| s == "J") || !leading.is_empty();
                let a = rest.trim();
                if job_form {
                    let id: u64 = a.parse().map_err(|_| {
                        XplError::trappable(format!("WAIT/J: expected a job id, got {a:?}"))
                    })?;
                    self.suspend = Some(SuspendReason::Wait(WaitTarget::Job(id)));
                } else if a.is_empty() {
                    self.suspend = Some(SuspendReason::Wait(WaitTarget::AllJobs));
                } else {
                    let ms: u64 = a.parse().map_err(|_| {
                        XplError::trappable(format!("WAIT: expected milliseconds, got {a:?}"))
                    })?;
                    self.suspend = Some(SuspendReason::Sleep(ms));
                }
                Ok(Flow::Suspend)
            }
            "TXT" => {
                // Optional leading `/T:"title"` (quoted, may contain spaces) sets the window
                // title; the rest is the body (`literal_text`: quoted → `\n` escapes; bare →
                // verbatim). MODAL: park the script until the GUI dialog is dismissed.
                let (title, body) = parse_txt(&args);
                let text = literal_text(body);
                // Under CAPTURE, redirect the body into the buffer and keep running — the modal
                // must NOT park the script.
                if self.is_capturing() {
                    self.capture_push(&text);
                    return Ok(Flow::Next);
                }
                self.suspend = Some(SuspendReason::ShowText {
                    title,
                    text,
                    columns: None,
                    line_colors: None,
                });
                Ok(Flow::Suspend)
            }
            "TRUST" => {
                let on = match args.trim() {
                    "1" => true,
                    "0" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "TRUST: expected 0 or 1, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetTrust(on))?;
                Ok(Flow::Next)
            }
            "X" => {
                // `X/BG` runs in the background (worker thread; $XO/$RC arrive later); `X/NV`
                // fires-and-forgets; bare `X` runs to completion ($RC/$XO set).
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("BG")) {
                    let return_var = return_var_switch(&vswitches);
                    self.host_op(EditorOp::ShellBg { cmd: args.clone(), return_var })?;
                } else if vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV")) {
                    match self.ctx.host.shell_spawn(&args) {
                        Ok(()) => {}
                        Err(XplError::Trappable { code, msg }) => self.fail_trap(code, msg),
                        Err(fatal) => return Err(fatal),
                    }
                } else if let Some((_out, code)) = self.shell_run(&args)? {
                    *self.ctx.status = Some(format!("exit code {code}"));
                }
                Ok(Flow::Next)
            }
            // List the running background jobs (`X/BG`); count via `$JOBS`.
            "JOBS" => {
                for line in self.ctx.host.job_list() {
                    self.ctx.output.push(line);
                }
                Ok(Flow::Next)
            }
            // Terminate a running background job by id (`KILL <id>`).
            "KILL" => {
                let id: u64 = args.trim().parse().map_err(|_| {
                    XplError::trappable(format!("KILL: expected a job id, got {:?}", args.trim()))
                })?;
                self.host_op(EditorOp::Kill(id))?;
                Ok(Flow::Next)
            }
            "XY" => {
                // Insert the command's stdout into the active buffer (recorded/undoable).
                if let Some((out, _)) = self.shell_run(&args)? {
                    self.host_op(EditorOp::InsertText(out))?;
                }
                Ok(Flow::Next)
            }
            "XV" => {
                // `XV <var> <cmd>` captures stdout into a user variable.
                let (var, cmd) = args
                    .trim_start()
                    .split_once(char::is_whitespace)
                    .filter(|(_, c)| !c.trim().is_empty())
                    .ok_or_else(|| XplError::trappable("XV: usage is XV <var> <command>"))?;
                let var = var.to_string();
                if let Some((out, _)) = self.shell_run(cmd)? {
                    self.ctx.env.set(&var, Value::str(out));
                }
                Ok(Flow::Next)
            }
            "DELSEL" => {
                self.host_op(EditorOp::Delete(DeleteKind::Selection))?;
                Ok(Flow::Next)
            }
            // Whole-unit deletes: word / line / sentence / paragraph (each removes
            // the unit the cursor is in, with its trailing break/space). `RE` is the
            // partial form — delete from the cursor to the end of the line.
            "RW" => {
                self.host_op(EditorOp::Delete(DeleteKind::Word))?;
                Ok(Flow::Next)
            }
            // `RL` — delete the current VISUAL (wrapped) line, for XyWrite compatibility. `RLL`
            // ("rubout line logical") deletes the whole LOGICAL line (the pre-XyWrite `RL` behavior).
            // Without a GUI to shape the layout (headless / `WRAP off`), `RL` falls back to `RLL`.
            "RL" => {
                self.host_op(EditorOp::Delete(DeleteKind::VisualLine))?;
                Ok(Flow::Next)
            }
            "RLL" => {
                self.host_op(EditorOp::Delete(DeleteKind::Line))?;
                Ok(Flow::Next)
            }
            "RS" => {
                self.host_op(EditorOp::Delete(DeleteKind::Sentence))?;
                Ok(Flow::Next)
            }
            "RP" => {
                self.host_op(EditorOp::Delete(DeleteKind::Paragraph))?;
                Ok(Flow::Next)
            }
            "RE" => {
                self.host_op(EditorOp::Delete(DeleteKind::ToEol))?;
                Ok(Flow::Next)
            }
            "UNDO" => {
                // Optional arg: `<number>` = how many levels to undo (default 1, à la XyWrite);
                // `ALL` = undo every level.
                let a = args.trim();
                let levels: usize = if a.is_empty() {
                    1
                } else if a.eq_ignore_ascii_case("all") {
                    usize::MAX
                } else {
                    a.parse().map_err(|_| {
                        XplError::trappable(format!("UNDO: expected a level count or ALL, got {a:?}"))
                    })?
                };
                self.host_op(EditorOp::Undo(levels))?;
                Ok(Flow::Next)
            }
            "REDO" => {
                // Optional arg: `<number>` = how many levels to redo (default 1); `ALL` = redo all.
                let a = args.trim();
                let levels: usize = if a.is_empty() {
                    1
                } else if a.eq_ignore_ascii_case("all") {
                    usize::MAX
                } else {
                    a.parse().map_err(|_| {
                        XplError::trappable(format!("REDO: expected a level count or ALL, got {a:?}"))
                    })?
                };
                self.host_op(EditorOp::Redo(levels))?;
                Ok(Flow::Next)
            }
            // File verbs (unguarded primitives; safety policy is composable XPL).
            // `NEW [<path>]` — bare opens a fresh unnamed buffer; with a path, a fresh buffer named
            // for it (ready to SAVE). Forgiving: an existing file is opened instead (see NewNamed).
            "NEW" | "NE" => {
                // `NEW/H` / `NEW/V` — open a fresh empty buffer in a NEW split pane (H = Stacked
                // top/bottom, V = SideBySide left/right) rather than a new tab.
                let stacked = vswitches.iter().any(|s| s.eq_ignore_ascii_case("H"));
                let side = vswitches.iter().any(|s| s.eq_ignore_ascii_case("V"));
                if stacked || side {
                    self.host_op(EditorOp::PaneSplitNew { stacked })?;
                    return Ok(Flow::Next);
                }
                let p = args.trim();
                if p.is_empty() {
                    self.host_op(EditorOp::NewDoc)?;
                } else {
                    self.host_op(EditorOp::NewNamed { path: p.to_string() })?;
                }
                Ok(Flow::Next)
            }
            // `BEDIT [file]` — blind edit: open into an invisible buffer for scripted editing
            // (bare = a new unnamed buffer). Edits/`$`-vars retarget to it; `SA` writes, `AB` exits,
            // else it auto-exits at script end.
            "BEDIT" => {
                let p = args.trim();
                let path = if p.is_empty() { None } else { Some(p.to_string()) };
                self.host_op(EditorOp::BlindEdit(path))?;
                Ok(Flow::Next)
            }
            // `BCLOSE [file]` — close (discard) the blind buffer a `BEDIT` opened. An explicit,
            // readable alternative to `AB` while blind. With a `<file>`, it also asserts that's the
            // buffer being closed (errors on a mismatch, or if no blind edit is open).
            "BCLOSE" => {
                let p = args.trim();
                let file = if p.is_empty() { None } else { Some(p.to_string()) };
                self.host_op(EditorOp::BlindClose(file))?;
                Ok(Flow::Next)
            }
            // `OPEN <path>` opens that file; bare `OPEN` pops the GUI's open-file dialog.
            // `CA`/`ED`/`EDIT` are XyWrite-compat synonyms.
            "OPEN" | "CA" | "ED" | "EDIT" => {
                let p = args.trim();
                // `/H` / `/V` open the file in a NEW split pane: split the focused pane first (a fresh
                // empty pane takes focus), then open into it. A fresh open (force) so it reuses that
                // empty pane cleanly rather than surfacing the "already open?" prompt.
                let split = pane_split_switch(&vswitches);
                if let Some(stacked) = split {
                    self.host_op(EditorOp::PaneSplitNew { stacked })?;
                }
                let force = split.is_some() || vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                // `/HEX` — open the file straight into the hex + text view.
                let hex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HEX"));
                if p.is_empty() {
                    self.host_op(EditorOp::OpenPrompt { hex })?;
                } else {
                    self.host_op(EditorOp::Open { path: p.to_string(), force, hex })?;
                }
                Ok(Flow::Next)
            }
            // `FIND` — open the GUI's non-modal Find/Replace dialog (the dialog composes SE/CH
            // searches; headless hosts just record the request). Search itself is `SE`/`CH`.
            "FIND" => {
                self.host_op(EditorOp::OpenFind)?;
                Ok(Flow::Next)
            }
            // `SCRATCH` — open a new floating scratchpad window (plain read/write text box for parking
            // text; no file, no save). Each call opens another. `SCRATCHCLOSE` closes the most-recently
            // focused one (its text lands in `$SCRATCHTEXT`). GUI-only; a no-op on a headless host.
            "SCRATCH" => {
                // `/V` prefills the new scratchpad with the clipboard contents.
                let paste = vswitches.iter().any(|s| s.eq_ignore_ascii_case("V"));
                self.host_op(EditorOp::OpenScratch { paste })?;
                Ok(Flow::Next)
            }
            "SCRATCHCLOSE" => {
                // `/V` also inserts the closing scratchpad's text into the active tab at the caret.
                let insert = vswitches.iter().any(|s| s.eq_ignore_ascii_case("V"));
                self.host_op(EditorOp::CloseScratch { insert })?;
                Ok(Flow::Next)
            }
            // `READ <file>` — open an EXISTING file in a read-only tab (locked against edits);
            // toggle the lock off later with `RO`. A missing file is a trappable error. `/NV` skips
            // the "already open?" prompt and opens a fresh read-only tab.
            "READ" => {
                // `/H` / `/V` — read the file into a new split pane (split first, then read into the
                // fresh pane). READ always opens a fresh read-only copy, so no prompt to worry about.
                if let Some(stacked) = pane_split_switch(&vswitches) {
                    self.host_op(EditorOp::PaneSplitNew { stacked })?;
                }
                let force = vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                let hex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HEX"));
                self.host_op(EditorOp::OpenReadOnly { path: args.trim().to_string(), force, hex })?;
                Ok(Flow::Next)
            }
            // `SAVE`/`SA` (no arg) saves the active file in place. `SAVE <filename>` writes a
            // COPY to <filename> and keeps the current file open in its tab (the doc's own path
            // is untouched) — confirming before overwriting an existing file unless `/NV` is given.
            // (To save-AND-adopt a new name, use `SAAS`/`SAVEAS`.)
            "SAVE" | "SA" => {
                let p = args.trim();
                if p.is_empty() {
                    self.host_op(EditorOp::Save(None))?;
                } else {
                    let force = vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                    self.host_op(EditorOp::SaveCopy { path: p.to_string(), force })?;
                }
                Ok(Flow::Next)
            }
            // `ST` ("store", XyWrite) — save the active file IN PLACE, then close its tab. Like
            // `SA;AB`, but the tab is closed ONLY if the save SUCCEEDED, so an untitled/unwritable
            // document isn't silently discarded (a bare `SA` on an untitled doc is a trappable error
            // → `$ERROR` set → we skip the close and leave the tab open with the message).
            "ST" => {
                let p = args.trim();
                let path = (!p.is_empty()).then(|| p.to_string());
                let force = vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                self.host_op(EditorOp::Store { path, force })?;
                Ok(Flow::Next)
            }
            // Save-as: `SAAS`/`SAVEAS <path>` saves to + ADOPTS the path; with no path it asks
            // the GUI to pop its native Save-As dialog. `/NV` overwrites an existing target without asking.
            "SAAS" | "SAVEAS" => {
                let p = args.trim();
                if p.is_empty() {
                    self.host_op(EditorOp::SaveAsPrompt)?;
                } else {
                    let force = vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                    self.host_op(EditorOp::SaveAs { path: p.to_string(), force })?;
                }
                Ok(Flow::Next)
            }
            // Save the current selection to a new file, byte-exact (the doc is untouched). `/NV`
            // overwrites an existing target without asking.
            "SAD" => {
                let force = vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                self.host_op(EditorOp::SaveSelection { path: args.trim().to_string(), force })?;
                Ok(Flow::Next)
            }
            // Revert the active document to its last saved state (re-read from disk).
            "REC" | "RECALL" => {
                if !args.trim().is_empty() {
                    return Err(XplError::trappable("REC: takes no arguments"));
                }
                self.host_op(EditorOp::Revert)?;
                Ok(Flow::Next)
            }
            // Read-only lock: bare `RO` toggles; `RO on|1` locks, `RO off|0` unlocks. `$RO`.
            "RO" => {
                let kind = match args.trim().to_ascii_lowercase().as_str() {
                    "" => ViewToggle::Toggle,
                    "on" | "1" | "true" | "yes" => ViewToggle::On,
                    "off" | "0" | "false" | "no" => ViewToggle::Off,
                    other => {
                        return Err(XplError::trappable(format!(
                            "RO: expected on|off (or nothing to toggle), got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetReadOnly(kind))?;
                Ok(Flow::Next)
            }
            // `DUPVIEW` — open a second view of the current file at the current line. `/W` opens it in a
            // separate free-standing window (a second editor instance); bare opens an in-process tab.
            // Both sync on save via AUTOREOPEN. Not TRUST-gated.
            "DUPVIEW" => {
                let window = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                self.host_op(EditorOp::DupView { window })?;
                Ok(Flow::Next)
            }
            // Lock keys — Set/Clear/Toggle: Caps (`SCL`/`CCL`/`TCL`), Num (`SNL`/`CNL`/`TNL`),
            // Scroll (`SSL`/`CSL`/`TSL`). S=on, C=off, T=toggle. Windows-only effect; read via
            // `$CAPSLOCK`/`$NUMLOCK`/`$SCROLLLOCK`.
            "SCL" | "CCL" | "TCL" | "SNL" | "CNL" | "TNL" | "SSL" | "CSL" | "TSL" => {
                let key = match base {
                    "SCL" | "CCL" | "TCL" => LockKey::Caps,
                    "SNL" | "CNL" | "TNL" => LockKey::Num,
                    _ => LockKey::Scroll,
                };
                let mode = if base.starts_with('S') {
                    ViewToggle::On
                } else if base.starts_with('C') {
                    ViewToggle::Off
                } else {
                    ViewToggle::Toggle
                };
                self.host_op(EditorOp::SetLock { key, mode })?;
                Ok(Flow::Next)
            }
            // Close the active buffer (`AB` = discard unsaved; `AB/V` = ask to save first).
            "AB" => {
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("V")) {
                    self.host_op(EditorOp::CloseBufferVerify)?;
                } else {
                    self.host_op(EditorOp::CloseBuffer)?;
                }
                Ok(Flow::Next)
            }
            // Delete the active document's file from disk and close it. `ABD` does so immediately;
            // `ABD/V` asks to confirm first. A never-saved buffer closes either way.
            "ABD" => {
                let verify = vswitches.iter().any(|s| s.eq_ignore_ascii_case("V"));
                self.host_op(EditorOp::DeleteNote { verify })?;
                Ok(Flow::Next)
            }
            // Config snapshots: `SAVECFG [path]` writes current settings as an XPL script;
            // `LOADCFG [path]` runs it back. No-arg uses the default config file.
            "SAVECFG" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveCfgToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let p = args.trim();
                    let arg = if p.is_empty() { None } else { Some(p.to_string()) };
                    self.host_op(EditorOp::SaveCfg(arg))?;
                }
                Ok(Flow::Next)
            }
            "LOADCFG" => {
                let p = args.trim();
                let arg = if p.is_empty() { None } else { Some(p.to_string()) };
                self.host_op(EditorOp::LoadCfg(arg))?;
                Ok(Flow::Next)
            }
            // `LANG <path>` loads a UI translation table. Point-and-shoot: bare `LANG` with the caret
            // on a strings file in a DIR listing loads THAT file (the path is substituted in above).
            // Bare `LANG` elsewhere, or `LANG off`, clears it (back to built-in English). i18n phase 1.
            "LANG" => {
                let p = args.trim();
                let arg = if p.is_empty() || p.eq_ignore_ascii_case("off") {
                    None
                } else {
                    Some(p.to_string())
                };
                self.host_op(EditorOp::LoadLang(arg))?;
                Ok(Flow::Next)
            }
            // `AUTOCFG [path]` starts live config tracking (default file if no path); `AUTOCFG off`
            // stops it. Self-updating SAVECFG (settings only). Read back via `$ACF`.
            "AUTOCFG" => {
                let a = args.trim();
                if a.eq_ignore_ascii_case("off") {
                    self.host_op(EditorOp::ConfigTrack { on: false, path: None })?;
                } else {
                    let path = if a.is_empty() { None } else { Some(a.to_string()) };
                    self.host_op(EditorOp::ConfigTrack { on: true, path })?;
                }
                Ok(Flow::Next)
            }
            // Command history: `HISTORY`/`HI` opens the picker; `LoadCM "a\nb\n…"` loads it.
            "HISTORY" | "HI" => {
                self.host_op(EditorOp::ShowHistory(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            // `MENU` — toggle keyboard navigation of the menu bar (GUI-only; a no-op headless).
            "MENU" => {
                // Bare `MENU` toggles; `MENU "f"` / `MENU f` opens the top menu whose accelerator
                // letter is `f` (first ASCII-alnum char of the unquoted arg).
                let letter = literal_text(&args)
                    .trim()
                    .chars()
                    .find(|c| c.is_ascii_alphanumeric())
                    .map(|c| c.to_string());
                self.host_op(EditorOp::MenuKbd(letter))?;
                Ok(Flow::Next)
            }
            "LOADCM" => {
                // The arg is a quoted, `\n`-separated list (escapes decoded like `insert`).
                self.host_op(EditorOp::LoadHistory(literal_text(&args)))?;
                Ok(Flow::Next)
            }
            // `SaveCM [path]` — write the command history to a runnable `LoadCM` script (inverse of
            // LoadCM; like SAVEKBD/SAVECFG). Bare arg → the default history-file path.
            "SAVECM" => {
                // Under CAPTURE, redirect the snapshot text into the variable instead of writing a
                // file (the path arg is then irrelevant) — mirrors DIR/HLKINDS capture behavior.
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveHistoryToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { None } else { Some(arg.to_string()) };
                    self.host_op(EditorOp::SaveHistory(path))?;
                }
                Ok(Flow::Next)
            }
            // `SAVELDTX [path]` — write all non-empty text registers as a runnable program of
            // `LDTX <slot> "<literal>"` lines (`RUN` it to restore). Under CAPTURE, into a var.
            "SAVELDTX" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveLdtxToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { None } else { Some(arg.to_string()) };
                    self.host_op(EditorOp::SaveLdtx(path))?;
                }
                Ok(Flow::Next)
            }
            // `SAVELDSC`/`SAVELDPM [path]` — the script-binder counterpart: write all non-empty
            // script registers as `LDSC <slot> "<xpl>"` lines.
            "SAVELDSC" | "SAVELDPM" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveLdscToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { None } else { Some(arg.to_string()) };
                    self.host_op(EditorOp::SaveLdsc(path))?;
                }
                Ok(Flow::Next)
            }
            // Reopen list (recently-closed files). `LoadClosed "…"` restores it (used by SAVECFG);
            // `SaveClosed [path]` writes a runnable `LoadClosed` script (CAPTURE → into a var).
            "LOADCLOSED" => {
                self.host_op(EditorOp::LoadClosed(literal_text(&args)))?;
                Ok(Flow::Next)
            }
            "SAVECLOSED" => {
                if self.is_capturing() {
                    let v = self.host_op_value(EditorOp::SaveClosedToString)?;
                    self.capture_push(&v.to_str());
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { None } else { Some(arg.to_string()) };
                    self.host_op(EditorOp::SaveClosed(path))?;
                }
                Ok(Flow::Next)
            }
            // Global-variable persistence. The file is `name<TAB>literal` lines (a DATA file, not a
            // runnable script): each literal is the value's `to_literal()` form, and `LoadVars`
            // evaluates it directly through the expression engine — so it never runs the
            // `$`-substitution pass, and a `$` in a saved string needs no escaping. Bare path → the
            // default `xyscript-vars.txt` (resolved against `$CWD`).
            "SAVEVARS" => {
                let mut pairs: Vec<(&String, &Value)> = self.ctx.env.vars.iter().collect();
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                let mut text = String::from("# XyScript saved variables — restore with LoadVars.\n");
                for (name, val) in pairs {
                    text.push_str(name);
                    text.push('\t');
                    text.push_str(&val.to_literal());
                    text.push('\n');
                }
                if self.is_capturing() {
                    // Under CAPTURE, redirect the text into the variable instead of a file.
                    self.capture_push(&text);
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { "xyscript-vars.txt" } else { arg };
                    if let Err(e) = self.ctx.host.write_file(path, &text) {
                        match e {
                            XplError::Trappable { code, msg } => self.fail_trap(code, msg),
                            other => return Err(other),
                        }
                    }
                }
                Ok(Flow::Next)
            }
            "LOADVARS" => {
                let arg = args.trim();
                let path = if arg.is_empty() { "xyscript-vars.txt".to_string() } else { arg.to_string() };
                let text = match self.ctx.host.read_file(&path) {
                    Ok(s) => s,
                    Err(XplError::Trappable { code, msg }) => {
                        self.fail_trap(code, msg);
                        return Ok(Flow::Next);
                    }
                    Err(fatal) => return Err(fatal),
                };
                let (mut loaded, mut bad) = (0usize, 0usize);
                for line in text.lines() {
                    let line = line.trim_end_matches('\r');
                    if line.trim().is_empty() || line.trim_start().starts_with('#') {
                        continue;
                    }
                    let (name, lit) = match line.split_once('\t') {
                        Some((n, l)) => (n.trim(), l),
                        None => { bad += 1; continue; }
                    };
                    if name.is_empty() || is_special_name(name) { bad += 1; continue; }
                    match expr::parse(lit).map_err(|e| XplError::fatal(e.msg)).and_then(|e| self.eval_expr(&e)) {
                        Ok(v) => { self.ctx.env.vars.insert(fold_ident(name), v); loaded += 1; }
                        Err(_) => { bad += 1; }
                    }
                }
                if bad > 0 {
                    self.ctx.env.error = 1;
                    *self.ctx.status = Some(format!("LoadVars: loaded {loaded}, skipped {bad} bad line(s)"));
                } else {
                    *self.ctx.status = Some(format!("LoadVars: loaded {loaded} variable(s)"));
                }
                Ok(Flow::Next)
            }
            // `ListVars` — list every global variable and its current value (`$name: value`, sorted),
            // a la LISTSPECIALS. Into a CAPTURE variable if one is open, else inserted at the caret.
            "LISTVARS" | "LISTVAR" => {
                let mut pairs: Vec<(&String, &Value)> = self.ctx.env.vars.iter().collect();
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                let text: String = pairs.iter().map(|(n, v)| format!("${n}: {}\n", v.to_str())).collect();
                if self.is_capturing() {
                    self.capture_push(&text);
                } else if pairs.is_empty() {
                    *self.ctx.status = Some("ListVars: no global variables set".to_string());
                } else {
                    self.host_op(EditorOp::InsertText(text))?;
                }
                Ok(Flow::Next)
            }
            // `CMH ["command"]` — add a command to the top of the history (dedup), without running it.
            // Bare `CMH` stashes the current command line (KBD-binding friendly); `CMH "command"`
            // stashes that QUOTED string instead, with `$vars` expanded NOW (at stash time) — `args`
            // is already substituted, then `literal_text` strips the quotes and processes escapes
            // (same convention as `INSERT "…"`).
            "CMH" => {
                let a = args.trim();
                let cmd = if a.is_empty() { None } else { Some(literal_text(a)) };
                self.host_op(EditorOp::PushHistory(cmd))?;
                Ok(Flow::Next)
            }
            // Autosave interval: `AUTOSAVE <seconds>` enables; `AUTOSAVE off` / `AUTOSAVE 0` disables.
            "AUTOSAVE" => {
                let a = args.trim();
                let secs = match a.to_ascii_lowercase().as_str() {
                    "" => return Err(XplError::trappable("AUTOSAVE: <seconds> or off")),
                    "off" => 0,
                    _ => a.parse::<u64>().map_err(|_| {
                        XplError::trappable(format!("AUTOSAVE: expected seconds or off, got {a:?}"))
                    })?,
                };
                self.host_op(EditorOp::SetAutosave(secs))?;
                Ok(Flow::Next)
            }
            // `DECIMALS <n>` / `DECIMALS off` — fixed decimal places when displaying floats; read
            // back via `$DECIMALS`, persisted by SaveCfg. `off` restores default precision.
            "DECIMALS" => {
                let a = args.trim();
                let dp = match a.to_ascii_lowercase().as_str() {
                    "" => return Err(XplError::trappable("DECIMALS: <places> or off")),
                    "off" => None,
                    _ => Some(a.parse::<u8>().map_err(|_| {
                        XplError::trappable(format!(
                            "DECIMALS: expected a place count or off, got {a:?}"
                        ))
                    })?),
                };
                self.host_op(EditorOp::SetDecimals(dp))?;
                Ok(Flow::Next)
            }
            // `DOCTYPE [typst|gfm|html|text|auto]` — how the active doc renders to PDF/preview. Bare =
            // report the effective kind; `auto` clears the override (decide by extension). `$DOCTYPE`
            // reads it.
            "DOCTYPE" => {
                let req = match args.trim().to_ascii_lowercase().as_str() {
                    "" => DocTypeReq::Query,
                    "auto" => DocTypeReq::Auto,
                    "gfm" => DocTypeReq::Gfm,
                    "typst" => DocTypeReq::Typst,
                    "html" => DocTypeReq::Html,
                    "text" => DocTypeReq::Text,
                    other => {
                        return Err(XplError::trappable(format!(
                            "DOCTYPE: expected typst, gfm, html, text, or auto, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::DocType(req))?;
                Ok(Flow::Next)
            }
            // `TB <n>` — set the tab width in spaces (>= 1); read back via `$TB`.
            "TB" => {
                let a = args.trim();
                let n: usize = a.parse().ok().filter(|&n| n >= 1).ok_or_else(|| {
                    XplError::trappable(format!("TB: expected a tab width (>= 1), got {a:?}"))
                })?;
                self.host_op(EditorOp::SetTabWidth(n))?;
                Ok(Flow::Next)
            }
            // Scroll the viewport one visual (wrap-aware) line, caret unmoved: `MD` = text moves
            // DOWN (viewport toward the start), `MU` = text moves UP (viewport toward the end).
            "MD" => {
                self.host_op(EditorOp::ScrollView(-1))?;
                Ok(Flow::Next)
            }
            "MU" => {
                self.host_op(EditorOp::ScrollView(1))?;
                Ok(Flow::Next)
            }
            // Save all named+modified buffers (`SL`); save+close all (`SF`).
            "SL" => {
                self.host_op(EditorOp::SaveAll)?;
                Ok(Flow::Next)
            }
            "SF" => {
                self.host_op(EditorOp::SaveCloseAll)?;
                Ok(Flow::Next)
            }
            "CD" => {
                // `CD` is synonymous with `CD/D`: naming a drive switches to it (the cwd is a
                // single in-process path, and `resolve_path` normalizes drive-qualified paths).
                // The `/D` switch is accepted as a harmless no-op for muscle memory.
                self.host_op(EditorOp::ChangeDir(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            // `DIRNW on|off` ("DIR NeW tab"): on = opening a file from a listing lands in a new tab
            // (the listing stays); off = it replaces the listing in place. `DIRRT on|off` ("DIR
            // Reuse Tab"): on = a `DIR` reuses an open listing tab instead of spawning a new one.
            "DIRNW" | "DIRWNW" | "DIRWCLOSE" | "DIRRT" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" => true,
                    "off" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "{base}: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(match base {
                    "DIRNW" => EditorOp::SetDirNewTab(on),
                    "DIRWNW" => EditorOp::SetDirWindowNewTab(on),
                    "DIRWCLOSE" => EditorOp::SetDirWindowClose(on),
                    _ => EditorOp::SetDirReuse(on),
                })?;
                Ok(Flow::Next)
            }
            // `DIRSET <switches>` — default sort/filter/view switches for `DIR`/`LS`. `DIRSET off`
            // (or `none`/`clear`) clears; bare `DIRSET` re-reports the current value. Each switch is
            // validated against `DirOpts`; `/W`/`/H`/`/V` (where the listing opens) are rejected —
            // `DIRSET` governs only HOW a listing is sorted/filtered/formatted.
            "DIRSET" => {
                let a = args.trim();
                let tokens = if a.eq_ignore_ascii_case("off")
                    || a.eq_ignore_ascii_case("none")
                    || a.eq_ignore_ascii_case("clear")
                {
                    Vec::new()
                } else if a.is_empty() {
                    self.ctx.host.dir_default_switches() // bare: report the current value
                } else {
                    let toks = split_switch_names(a);
                    for t in &toks {
                        if matches!(t.as_str(), "W" | "H" | "V") {
                            return Err(XplError::trappable(format!(
                                "DIRSET: /{t} controls WHERE the listing opens, not how it's sorted — not a default"
                            )));
                        }
                        let mut probe = DirOpts::default();
                        if !apply_dir_switch(&mut probe, t) {
                            return Err(XplError::trappable(format!("DIRSET: unknown switch /{t}")));
                        }
                    }
                    toks
                };
                self.host_op(EditorOp::SetDirDefaults(tokens))?;
                Ok(Flow::Next)
            }
            // `ENCODING <name>` switches the DISPLAY view (re-decodes the same bytes). `ENCODING/C
            // <name> [outfile]` CONVERTS: re-encode the text into `<name>` (lossy → `?`), changing
            // the bytes in place, or writing a converted copy to `outfile` and opening it.
            "ENCODING" => {
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("C")) {
                    let a = args.trim();
                    let (name, out_file) = match a.split_once(char::is_whitespace) {
                        Some((n, rest)) if !rest.trim().is_empty() => {
                            (n.to_string(), Some(rest.trim().to_string()))
                        }
                        _ => (a.to_string(), None),
                    };
                    self.host_op(EditorOp::ConvertEncoding { name, out_file })?;
                } else {
                    self.host_op(EditorOp::SetEncoding(args.trim().to_string()))?;
                }
                Ok(Flow::Next)
            }
            // `TITLEBAR on|off` (bare = toggle) — show/hide the OS window title bar. `$TITLEBAR`
            // reads it; the runtime applies the change. A draggable strip replaces the bar when off.
            "TITLEBAR" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None, // toggle
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "TITLEBAR: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetTitlebar(want))?;
                Ok(Flow::Next)
            }
            // `WV on|off` (bare = toggle) — Writer's View: hide ALL chrome (title/menu/tabs/cmline/
            // status), leaving just the text window. Transient; `Esc` exits. `$WV` reads it.
            "WV" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None, // toggle
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!("WV: expected on or off, got {other:?}")))
                    }
                };
                self.host_op(EditorOp::SetWriterView(want))?;
                Ok(Flow::Next)
            }
            // `EMPH <phrase>` — highlight every whole-word occurrence of <phrase> in the active tab
            // (case-insensitive); `EMPH/R <regex>` matches a regular expression instead. Bare `EMPH`
            // clears it. Per-tab, session-only; colours from `EMPHFGCOLOR`/`EMPHBGCOLOR` (default: the
            // selection colours). Works in read-only tabs.
            "EMPH" => {
                let regex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("R"));
                let t = args.trim();
                let phrase = if t.is_empty() { None } else { Some(t.to_string()) };
                self.host_op(EditorOp::SetEmph { phrase, regex })?;
                Ok(Flow::Next)
            }
            // `WINGEO on|off` (bare = toggle) — whether the GUI remembers the main window's size +
            // position across runs. `$WINGEO` reads it; off is handy on mixed-DPI multi-monitor setups.
            "WINGEO" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None, // toggle
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "WINGEO: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetRememberGeometry(want))?;
                Ok(Flow::Next)
            }
            // `SESSION on|off` (bare = toggle) — auto-save-and-restore the open tabs across runs.
            "SESSION" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "SESSION: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetSessionRestore(want))?;
                Ok(Flow::Next)
            }
            // `NEWLINE lf|crlf` — convert every line ending in the document to LF or CRLF and adopt
            // it as the document's style (what `ENTER` inserts). Lossless. (`$NEWLINE` reads it.)
            "NEWLINE" => {
                let crlf = match args.trim().to_ascii_lowercase().as_str() {
                    "crlf" => true,
                    "lf" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "NEWLINE: expected lf or crlf, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetNewlineStyle(crlf))?;
                Ok(Flow::Next)
            }
            "TOPDF" | "PRINT" => {
                let p = args.trim();
                let arg = if p.is_empty() { None } else { Some(p.to_string()) };
                self.host_op(EditorOp::ToPdf(arg))?;
                Ok(Flow::Next)
            }
            // `FPRINT <source> [<output>]` — TOPDF a file that isn't open, by its own type (extension),
            // with the current Preview settings. Paths with spaces are quoted (like COPY/RENAME).
            "FPRINT" => {
                let toks = split_path_tokens(&args);
                let (source, output) = match toks.as_slice() {
                    [src] => (src.clone(), None),
                    [src, out] => (src.clone(), Some(out.clone())),
                    _ => {
                        return Err(XplError::trappable(
                            "FPRINT: expected <source> [<output>] (quote paths with spaces)".to_string(),
                        ))
                    }
                };
                self.host_op(EditorOp::FilePrint { source, output })?;
                Ok(Flow::Next)
            }
            "WORDCOUNT" => {
                if !args.trim().is_empty() {
                    return Err(XplError::trappable("WORDCOUNT: takes no arguments".to_string()));
                }
                self.host_op(EditorOp::WordCount)?;
                Ok(Flow::Next)
            }
            "CHARCOUNT" => {
                if !args.trim().is_empty() {
                    return Err(XplError::trappable("CHARCOUNT: takes no arguments".to_string()));
                }
                self.host_op(EditorOp::CharCount)?;
                Ok(Flow::Next)
            }
            // `SRANDOM <seed>` — seed the generator behind `RANDOM(min, max)`, so the sequence is
            // reproducible. Accepts an integer (a float seed is truncated); negatives are allowed.
            "SRANDOM" => {
                let a = args.trim();
                if a.is_empty() {
                    return Err(XplError::trappable("SRANDOM: expected a seed".to_string()));
                }
                let seed = a
                    .parse::<i64>()
                    .or_else(|_| a.parse::<f64>().map(|f| f.trunc() as i64))
                    .map_err(|_| XplError::trappable(format!("SRANDOM: invalid seed {a:?}")))?;
                self.ctx.host.seed_random(seed as u64);
                *self.ctx.status = Some(format!("random seed {seed}"));
                Ok(Flow::Next)
            }
            "HIGHLIGHT" => {
                let a = args.trim();
                let on = match a.to_ascii_lowercase().as_str() {
                    "on" | "1" | "true" | "yes" => true,
                    "off" | "0" | "false" | "no" | "" => false,
                    _ => return Err(XplError::trappable(format!("HIGHLIGHT: expected on|off, got {a:?}"))),
                };
                self.host_op(EditorOp::SetHighlight(on))?;
                Ok(Flow::Next)
            }
            // `HEX on|off` (bare = on) — show the active document as a hex + text grid. A view mode;
            // the bytes are unchanged. `$HEX` reads it.
            "HEX" => {
                let a = args.trim();
                // Bare `HEX` toggles; `HEX on|off` sets explicitly.
                let want = match a.to_ascii_lowercase().as_str() {
                    "" => None, // toggle
                    "on" | "1" | "true" | "yes" => Some(true),
                    "off" | "0" | "false" | "no" => Some(false),
                    _ => return Err(XplError::trappable(format!("HEX: expected on|off, got {a:?}"))),
                };
                self.host_op(EditorOp::SetHexView(want))?;
                Ok(Flow::Next)
            }
            // Bookmarks. MARKSET/MARKCLEAR/MARKCLEARALL/MARKTOGGLE act at the caret; MARKNEXT/MARKPREV
            // move the caret to the next/previous bookmark (circular). Set/clear are trappable (already
            // set / nothing there); toggle never errors. The host op returns the status string.
            "MARKSET" | "MARKCLEAR" | "MARKCLEARALL" | "MARKTOGGLE" | "MARKNEXT" | "MARKPREV" => {
                if !args.trim().is_empty() {
                    return Err(XplError::trappable(format!("{base} takes no argument")));
                }
                let op = match base {
                    "MARKSET" => EditorOp::MarkSet,
                    "MARKCLEAR" => EditorOp::MarkClear,
                    "MARKCLEARALL" => EditorOp::MarkClearAll,
                    "MARKTOGGLE" => EditorOp::MarkToggle,
                    "MARKNEXT" => EditorOp::MarkNext,
                    _ => EditorOp::MarkPrev,
                };
                self.host_op(op)?;
                Ok(Flow::Next)
            }
            // `MARKLIST` — a MODAL chooser (sharing the PICK dialog) of the active document's
            // bookmarks; choosing one jumps the caret there (centered). Empty list = a status note.
            "MARKLIST" => {
                let marks = self.ctx.host.bookmark_list();
                if marks.is_empty() {
                    *self.ctx.status = Some("MARKLIST: no bookmarks".to_string());
                    return Ok(Flow::Next);
                }
                let (targets, items): (Vec<usize>, Vec<String>) = marks.into_iter().unzip();
                self.suspend = Some(SuspendReason::MarkPick {
                    title: Some("Go to bookmark".to_string()),
                    items,
                    targets,
                });
                Ok(Flow::Suspend)
            }
            "HLAUTO" => {
                let a = args.trim();
                let on = match a.to_ascii_lowercase().as_str() {
                    "on" | "1" | "true" | "yes" => true,
                    "off" | "0" | "false" | "no" | "" => false,
                    _ => return Err(XplError::trappable(format!("HLAUTO: expected on|off, got {a:?}"))),
                };
                self.host_op(EditorOp::SetHighlightAuto(on))?;
                Ok(Flow::Next)
            }
            // `TABBGCOLOR <state> <r g b>` — tab-title background per state (active|hover|inactive);
            // `/HSL` reads the triple as H S L; `TABBGCOLOR <state>` / `<state> default` clears it.
            "TABBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let (state, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
                if state.is_empty() {
                    return Err(XplError::trappable(
                        "TABBGCOLOR: <active|hover|inactive> [<r g b> | default]",
                    ));
                }
                let rest = rest.trim();
                let color = if rest.is_empty() || rest.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color("TABBGCOLOR", rest, hsl)?)
                };
                self.host_op(EditorOp::SetTabBgColor { state: state.to_string(), color })?;
                Ok(Flow::Next)
            }
            // `TABXCOLOR <r g b>` — the tab close-× glyph colour (`/HSL`); `TABXCOLOR` / `TABXCOLOR
            // default` resets it to the UI foreground. `TABXCOLORS` opens the picker. `$TABXCOLOR`.
            "TABXCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color("TABXCOLOR", t, hsl)?)
                };
                self.host_op(EditorOp::SetTabXColor(color))?;
                Ok(Flow::Next)
            }
            // `BALPAIRS on|off` — highlight matching brackets; `$BALPAIRS`.
            "BALPAIRS" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "BALPAIRS: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetBalPairs(on))?;
                Ok(Flow::Next)
            }
            "TYPEPAIRS" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "TYPEPAIRS: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetTypePairs(on))?;
                Ok(Flow::Next)
            }
            // `BALPAIRSCOLOR <r g b>` / `default` — the matched-bracket highlight colour. `$BALPAIRSCOLOR`.
            "BALPAIRSCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color("BALPAIRSCOLOR", t, hsl)?)
                };
                self.host_op(EditorOp::SetBalColor(color))?;
                Ok(Flow::Next)
            }
            // `EMPHFGCOLOR`/`EMPHBGCOLOR <r g b>` / `default` — the `EMPH` emphasis glyph / background
            // colour (`/HSL`); bare or `… default` follows the selection colours. `EMPHFGCOLORS`/
            // `EMPHBGCOLORS` open the picker. `$EMPHFGCOLOR`/`$EMPHBGCOLOR`.
            "EMPHFGCOLOR" | "EMPHBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(if base == "EMPHFGCOLOR" {
                    EditorOp::SetEmphFgColor(color)
                } else {
                    EditorOp::SetEmphBgColor(color)
                })?;
                Ok(Flow::Next)
            }
            // `PAFGCOLOR`/`PABGCOLOR <r g b>` — the `$CWD` path-display foreground/background colour
            // (`/HSL`); bare or `… default` clears it (fg → dimmed UI text, bg → transparent).
            // `PAFGCOLORS`/`PABGCOLORS` open the picker. `$PAFGCOLOR`/`$PABGCOLOR`.
            "PAFGCOLOR" | "PABGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetPathColor { fg: base == "PAFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "PAFGCOLORS" | "PABGCOLORS" => {
                self.host_op(EditorOp::OpenPathColorPicker { fg: base == "PAFGCOLORS" })?;
                Ok(Flow::Next)
            }
            // `SCRATCHFGCOLOR`/`SCRATCHBGCOLOR <r g b>` — the scratchpad window's text/background
            // colour (`/HSL`); bare or `… default` clears it (back to egui's theme). The `…S` forms
            // open the picker. `$SCRATCHFGCOLOR`/`$SCRATCHBGCOLOR`.
            "SCRATCHFGCOLOR" | "SCRATCHBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetScratchColor { fg: base == "SCRATCHFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "SCRATCHFGCOLORS" | "SCRATCHBGCOLORS" => {
                self.host_op(EditorOp::OpenScratchColorPicker { fg: base == "SCRATCHFGCOLORS" })?;
                Ok(Flow::Next)
            }
            // `TABBGCOLORS <state>` — pop the GUI colour-picker for that tab-title state.
            "TABBGCOLORS" => {
                let state = args.trim();
                if state.is_empty() {
                    return Err(XplError::trappable(
                        "TABBGCOLORS: a state is required (active|hover|inactive)",
                    ));
                }
                self.host_op(EditorOp::OpenTabColorPicker(state.to_string()))?;
                Ok(Flow::Next)
            }
            // `THEME <name>` — apply a built-in colour scheme (`light`|`dark`) across the whole
            // editor, OR the path to a theme file (a SAVECFG-format colour preset), which is run
            // to activate the theme (the LOADCFG path: resolved against `$CWD`, then executed).
            "THEME" => {
                // A bare THEME while the caret sits on a file in a DIR listing loaded that file (via
                // POINT_AND_SHOOT_VERBS filling `args` above); otherwise a name/path is required.
                let name = args.trim();
                if name.is_empty() {
                    return Err(XplError::trappable(
                        "THEME: a name is required (light|dark|<theme-file>, or point at one in a directory listing)",
                    ));
                }
                match name.to_ascii_lowercase().as_str() {
                    "light" | "dark" => {
                        self.host_op(EditorOp::ApplyTheme(name.to_string()))?;
                    }
                    // Anything else is a path to a theme file: load (run) it to activate the theme.
                    _ => {
                        self.host_op(EditorOp::LoadCfg(Some(name.to_string())))?;
                    }
                }
                Ok(Flow::Next)
            }
            // Multi-buffer switching.
            "BUFNEXT" => {
                self.host_op(EditorOp::BufNext)?;
                Ok(Flow::Next)
            }
            "BUFPREV" => {
                self.host_op(EditorOp::BufPrev)?;
                Ok(Flow::Next)
            }
            "BUFALT" => {
                self.host_op(EditorOp::BufAlt)?;
                Ok(Flow::Next)
            }
            // Multi-caret management verbs. `CARETADD` (bare) plants a caret at the current caret and
            // enters the keyboard building state (plain motion then moves the primary only, so you can
            // roam and plant more); `CARETADD <n>` adds/toggles a caret at byte offset n. `CARETUP`/
            // `CARETDOWN` add a caret one line above/below each caret. `CARETONE` collapses to one.
            "CARETADD" => {
                let a = args.trim();
                let pos = if a.is_empty() {
                    None
                } else {
                    match a.parse::<usize>() {
                        Ok(n) => Some(n),
                        Err(_) => {
                            return Err(XplError::trappable(format!(
                                "CARETADD: expected a byte offset, got {a:?}"
                            )))
                        }
                    }
                };
                self.host_op(EditorOp::CaretAdd(pos))?;
                Ok(Flow::Next)
            }
            "CARETUP" => {
                self.host_op(EditorOp::CaretVertical(false))?;
                Ok(Flow::Next)
            }
            "CARETDOWN" => {
                self.host_op(EditorOp::CaretVertical(true))?;
                Ok(Flow::Next)
            }
            "CARETCOL" => {
                self.host_op(EditorOp::CaretColumn)?;
                Ok(Flow::Next)
            }
            // `CARETALL [/R] [/W] [/A] [/S] [<pattern>]` — a selection-caret at EVERY match. Bare uses
            // the current selection (or the word under the caret) as the needle; `/S` confines the
            // search to the current selection. Switches mirror the FIND family: /R regex, /W whole
            // word, /A case-absolute (default insensitive). Capped with a status report.
            "CARETALL" => {
                let regex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("R"));
                let whole = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let case_sensitive = vswitches.iter().any(|s| s.eq_ignore_ascii_case("A"));
                let in_selection = vswitches.iter().any(|s| s.eq_ignore_ascii_case("S"));
                let t = args.trim();
                let pattern = if t.is_empty() { None } else { Some(t.to_string()) };
                self.host_op(EditorOp::CaretAll { pattern, regex, whole, case_sensitive, in_selection })?;
                Ok(Flow::Next)
            }
            // `CARETNEXT [/R] [/W] [/A] [<text>]` — add the next occurrence of the primary selection (or
            // <text>) after the last caret as a selection-caret, and make it primary; wraps at EOF. With
            // nothing selected and no <text>, it seeds by selecting the word under the caret.
            "CARETNEXT" => {
                let regex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("R"));
                let whole = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let case_sensitive = vswitches.iter().any(|s| s.eq_ignore_ascii_case("A"));
                let t = args.trim();
                let text = if t.is_empty() { None } else { Some(t.to_string()) };
                self.host_op(EditorOp::CaretNext { text, regex, whole, case_sensitive })?;
                Ok(Flow::Next)
            }
            // `CARETPOP` — undo the most recent caret-set change (remove the last-added caret).
            "CARETPOP" => {
                self.host_op(EditorOp::CaretPop)?;
                Ok(Flow::Next)
            }
            // `CARETPRIMARY <n>` — make the caret at document-order index n (0-based) the primary.
            "CARETPRIMARY" => {
                let a = args.trim();
                let n: usize = a
                    .parse()
                    .map_err(|_| XplError::trappable(format!("CARETPRIMARY: expected an index, got {a:?}")))?;
                self.host_op(EditorOp::CaretPrimary(n))?;
                Ok(Flow::Next)
            }
            // `CARETSKIP [/R] [/W] [/A]` — during a CARETNEXT run, move the primary selection to the
            // next occurrence (skip the current match) without adding a caret.
            "CARETSKIP" => {
                let regex = vswitches.iter().any(|s| s.eq_ignore_ascii_case("R"));
                let whole = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let case_sensitive = vswitches.iter().any(|s| s.eq_ignore_ascii_case("A"));
                self.host_op(EditorOp::CaretSkip { regex, whole, case_sensitive })?;
                Ok(Flow::Next)
            }
            // `CARETSPLIT` — split a multi-line selection into one bare caret per line (at each line's end).
            "CARETSPLIT" => {
                self.host_op(EditorOp::CaretSplit)?;
                Ok(Flow::Next)
            }
            "CARETONE" => {
                self.host_op(EditorOp::CaretOne)?;
                Ok(Flow::Next)
            }
            "BUFGO" => {
                self.host_op(EditorOp::BufGo(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "GOFILE" | "GOF" => {
                self.host_op(EditorOp::GoFile(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "BUFLIST" => {
                for line in self.ctx.host.buffer_list() {
                    self.ctx.output.push(line);
                }
                Ok(Flow::Next)
            }
            // Split panes. `NEW/H`/`NEW/V` open a fresh empty buffer in a new pane split from the
            // focused one (H = top/bottom Stacked, V = left/right SideBySide). `THS`/`TVS` do the same
            // but show a partner buffer (an arg names an open tab; else the alternate buffer; else a
            // fresh empty one). `TW` cycles focus, `TUS` un-splits, `TRV` swaps, `TZ` zooms.
            "THS" | "TVS" => {
                let stacked = base == "THS";
                let t = args.trim();
                let target = if t.is_empty() { None } else { Some(t.to_string()) };
                self.host_op(EditorOp::PaneSplitTile { stacked, target })?;
                Ok(Flow::Next)
            }
            "TW" => {
                self.host_op(EditorOp::PaneFocusNext)?;
                Ok(Flow::Next)
            }
            "TUS" => {
                self.host_op(EditorOp::PaneCollapse)?;
                Ok(Flow::Next)
            }
            // `FS` — release the focused pane to FULL SCREEN: collapse the whole split to just it.
            "FS" => {
                self.host_op(EditorOp::PaneFullscreen)?;
                Ok(Flow::Next)
            }
            "TRV" => {
                self.host_op(EditorOp::PaneSwap)?;
                Ok(Flow::Next)
            }
            "TZ" => {
                self.host_op(EditorOp::PaneZoom)?;
                Ok(Flow::Next)
            }
            // `LISTTABS` — a MODAL chooser of the open tabs (the interactive sibling of `BUFLIST`).
            // Each row is "N filename"; choosing one switches to it. The `targets` are the buffer
            // indices in tab order (row i → buffer i).
            "LISTTABS" => {
                let items = self.ctx.host.buffer_list();
                if items.is_empty() {
                    *self.ctx.status = Some("LISTTABS: no open tabs".to_string());
                    return Ok(Flow::Next);
                }
                let targets: Vec<usize> = (0..items.len()).collect();
                self.suspend = Some(SuspendReason::TabPick {
                    title: Some("Switch to tab".to_string()),
                    items,
                    targets,
                });
                Ok(Flow::Suspend)
            }
            // Cursor motion. A trailing `SEL` argument extends the selection
            // (e.g. `RIGHT SEL` is Shift+Right). `NC/PC` (char) and `LU/LD` (line)
            // are XyWrite synonyms for the arrow motions.
            "LEFT" | "RIGHT" | "UP" | "DOWN" | "HOME" | "END" | "EL" | "ER" | "LB" | "LE" | "TOP"
            | "BOTTOM" | "TF" | "BF" | "NC" | "PC" | "LU" | "LD" | "NW" | "PW" | "NP" | "PP"
            | "NS" | "PS" | "NL" | "PL" | "CL" | "CR" | "LL" | "LR" | "PD" | "PU" | "HM" | "BS" => {
                let motion = match base {
                    "LEFT" | "PC" | "CL" => Motion::Left,
                    "RIGHT" | "NC" | "CR" => Motion::Right,
                    "UP" | "LU" => Motion::Up,
                    "DOWN" | "LD" => Motion::Down,
                    // `EL`/`ER` (Express Left/Right) = start/end of the ACTUAL (logical) line, the
                    // same as `HOME`/`END`. `LB`/`LE` (Line Begin/End) = start/end of the VISUAL
                    // (wrapped) line — fulfilled with a logical fallback + GUI cosmic refine.
                    "HOME" | "EL" => Motion::LineStart,
                    "END" | "ER" => Motion::LineEnd,
                    "LB" => Motion::VisualLineStart,
                    "LE" => Motion::VisualLineEnd,
                    // `TF`/`BF` are the XyWrite "Top/Bottom of File" names for `TOP`/`BOTTOM`.
                    "TOP" | "TF" => Motion::DocStart,
                    "NW" => Motion::NextWord,
                    "PW" => Motion::PrevWord,
                    "NP" => Motion::NextPara,
                    "PP" => Motion::PrevPara,
                    "NS" => Motion::NextSentence,
                    "PS" => Motion::PrevSentence,
                    "NL" => Motion::NextLineStart,
                    "PL" => Motion::PrevLineStart,
                    "LL" => Motion::LeftSameLine,
                    "LR" => Motion::RightSameLine,
                    "PD" => Motion::PageDown,
                    "PU" => Motion::PageUp,
                    "HM" => Motion::ScreenTop,
                    "BS" => Motion::ScreenBottom,
                    _ => Motion::DocEnd, // BOTTOM / BF
                };
                let extend = args.trim().eq_ignore_ascii_case("SEL");
                self.host_op(EditorOp::Move { motion, extend })?;
                Ok(Flow::Next)
            }
            // `DB`/`DE` — move the cursor to the start/end of the selection, exactly like
            // `JMP $DS`/`JMP $DN`: a DEFINED block is kept (caret roams to the edge), a transient
            // selection collapses. Trappable "no text is selected" (sets `$ERROR`) when none.
            "DB" | "DE" => {
                self.host_op(EditorOp::MoveToSelectionEdge { end: base == "DE" })?;
                Ok(Flow::Next)
            }
            // `JMP n [SEL]` — absolute char offset; `JMP +n` / `JMP -n` — relative
            // (clamped at the edges, no error out of range).
            "JMP" => {
                let mut parts = args.split_whitespace();
                let tok = parts
                    .next()
                    .ok_or_else(|| XplError::trappable("JMP requires a character offset"))?;
                let relative = tok.starts_with('+') || tok.starts_with('-');
                let motion = if relative {
                    let d: i64 = tok
                        .parse()
                        .map_err(|_| XplError::trappable("JMP: invalid offset"))?;
                    Motion::ByChar(d)
                } else {
                    let n: usize = tok
                        .parse()
                        .map_err(|_| XplError::trappable("JMP: invalid offset"))?;
                    Motion::ToChar(n)
                };
                let extend = parts.next().is_some_and(|s| s.eq_ignore_ascii_case("SEL"));
                self.host_op(EditorOp::Move { motion, extend })?;
                Ok(Flow::Next)
            }
            // Defined-text ("DF") selection: DF toggles, DM marks to cursor, DZ
            // closes, DFA selects all, XD/YD cancel.
            // Define a selection: DF toggle, DM modify, DZ close, DFA all, XD/YD
            // cancel; DW/DL/DS/DP define the word/line/sentence/paragraph at the cursor.
            "DF" | "DM" | "DZ" | "DFA" | "XD" | "YD" | "DW" | "DL" | "DS" | "DP" | "DW+"
            | "DS+" | "DP+" => {
                let kind = match base {
                    "DF" => DefineKind::Toggle,
                    "DM" => DefineKind::Modify,
                    "DZ" => DefineKind::Close,
                    "DFA" => DefineKind::All,
                    "DW" => DefineKind::Word,
                    "DL" => DefineKind::Line,
                    "DS" => DefineKind::Sentence,
                    "DP" => DefineKind::Paragraph,
                    "DW+" => DefineKind::WordPlus,
                    "DS+" => DefineKind::SentencePlus,
                    "DP+" => DefineKind::ParagraphPlus,
                    _ => DefineKind::Cancel, // XD / YD
                };
                self.host_op(EditorOp::Define(kind))?;
                Ok(Flow::Next)
            }
            // Persistent selection: SPS on, CPS off, TPS toggle.
            "SPS" | "CPS" | "TPS" => {
                let kind = match base {
                    "SPS" => PersistKind::On,
                    "CPS" => PersistKind::Off,
                    _ => PersistKind::Toggle,
                };
                self.host_op(EditorOp::Persistent(kind))?;
                Ok(Flow::Next)
            }
            // GUI view toggles (S=on, C=off, T=toggle): MB menu bar, VR visible returns ¶,
            // WS visible whitespace, LN line-number gutter, GC pop-up command line.
            // Read via `$MB`/`$VR`/`$WS`/`$LNS`/`$GC`.
            "SMB" | "CMB" | "TMB" | "SVR" | "CVR" | "TVR" | "SWS" | "CWS" | "TWS" | "SLN"
            | "CLN" | "TLN" | "SGC" | "CGC" | "TGC" => {
                let flag = match &base[1..] {
                    "MB" => ViewFlag::MenuBar,
                    "VR" => ViewFlag::VisibleReturns,
                    "WS" => ViewFlag::VisibleWhitespace,
                    "GC" => ViewFlag::GuiCmline,
                    _ => ViewFlag::LineNumbers, // "LN"
                };
                let kind = match &base[..1] {
                    "S" => ViewToggle::On,
                    "C" => ViewToggle::Off,
                    _ => ViewToggle::Toggle, // "T"
                };
                self.host_op(EditorOp::SetView { flag, kind })?;
                Ok(Flow::Next)
            }
            // Show/hide the *Shell Output* tab (`SW`/`SHELLWIN`). No arg toggles; `on`/`off` set
            // it. `X/BG` follows this state. Read back via `$SW`.
            "SW" | "SHELLWIN" => {
                // `/H` `/V` — show the *Shell Output* buffer in a split pane instead of toggling its tab.
                if let Some(stacked) = pane_split_switch(&vswitches) {
                    self.host_op(EditorOp::PaneShell { stacked })?;
                    return Ok(Flow::Next);
                }
                let a = args.trim();
                let kind = if a.is_empty() {
                    ViewToggle::Toggle
                } else if a.eq_ignore_ascii_case("on") {
                    ViewToggle::On
                } else if a.eq_ignore_ascii_case("off") {
                    ViewToggle::Off
                } else {
                    return Err(XplError::trappable(format!(
                        "{base}: expected on|off (or no argument to toggle), got `{a}`"
                    )));
                };
                self.host_op(EditorOp::SetView { flag: ViewFlag::ShellWindow, kind })?;
                Ok(Flow::Next)
            }
            // Show/hide the interactive shell pane at the bottom of the editor (`SH`/`SHELL`). No
            // arg toggles; `on`/`off` set it. Read back via `$SH`.
            // `SH`/`SHELL` — open + focus the Console REPL tab (bare or `on`); `off` hides it.
            "SH" | "SHELL" => {
                let a = args.trim();
                let show = if a.is_empty() || a.eq_ignore_ascii_case("on") {
                    true
                } else if a.eq_ignore_ascii_case("off") {
                    false
                } else {
                    return Err(XplError::trappable(format!(
                        "{base}: expected on|off (or no argument to open), got `{a}`"
                    )));
                };
                self.host_op(EditorOp::OpenConsole { show })?;
                Ok(Flow::Next)
            }
            // `SHPROMPT [<template>]` — the Console prompt; bare restores the default. Uses `raw_args`
            // (NOT the `$`-substituted `args`) so `$CWD` survives VERBATIM in the stored template and is
            // expanded at render time (see `AppHost::expand_prompt`) — otherwise it would
            // freeze to the directory that was current when the prompt was set (or when a config loaded).
            // Quote the template to keep a leading/trailing space (`SHPROMPT "$CWD> "`), which most
            // prompts want; an unquoted value is trimmed.
            "SHPROMPT" => {
                let r = raw_args.trim();
                let tmpl = if r.is_empty() {
                    None
                } else if r.len() >= 2 && r.starts_with('"') && r.ends_with('"') {
                    Some(r[1..r.len() - 1].to_string()) // inner verbatim — spaces preserved
                } else {
                    Some(r.to_string())
                };
                self.host_op(EditorOp::SetConsolePrompt(tmpl))?;
                Ok(Flow::Next)
            }
            // `ANSICOLOR on|off` (bare = toggle) — colour Console output from ANSI codes, or show them raw.
            "ANSICOLOR" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "ANSICOLOR: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetAnsiColor(want))?;
                Ok(Flow::Next)
            }
            // `CLS` / `CLEAR` — clear the Console scrollback.
            "CLS" | "CLEAR" => {
                self.host_op(EditorOp::ConsoleClear)?;
                Ok(Flow::Next)
            }
            // `CALC` — a calculator over the `$(…)` expression engine. With an argument, evaluate it
            // (result → status line + clipboard). With none, act on the document: evaluate the caret's
            // line and insert the result below it, or (with a selection) sum the selected lines and
            // insert the total below. `$CALC` reads back the last result.
            "CALC" => {
                let dp = self.calc_decimals();
                let arg = args.trim().to_string();
                if !arg.is_empty() {
                    match self.calc_eval(&arg) {
                        Ok(v) => {
                            let s = v.to_str_dp(dp);
                            self.ctx.host.calc_set_clipboard(&s);
                            self.ctx.host.set_last_calc(&s);
                            *self.ctx.status = Some(format!("= {s}  (copied)"));
                        }
                        Err(e) => {
                            self.ctx.env.error = 1;
                            *self.ctx.status = Some(format!("CALC: {e}"));
                        }
                    }
                    return Ok(Flow::Next);
                }
                match self.ctx.host.calc_context() {
                    CalcContext::CmLine => {
                        *self.ctx.status =
                            Some("CALC: put the cursor on a line to evaluate".to_string());
                    }
                    CalcContext::Line(line) => match self.calc_eval(&line) {
                        Ok(v) => {
                            let s = v.to_str_dp(dp);
                            self.ctx.host.calc_insert_result(&s);
                            self.ctx.host.set_last_calc(&s);
                            *self.ctx.status = Some(format!("= {s}"));
                        }
                        Err(e) => {
                            self.ctx.env.error = 1;
                            *self.ctx.status = Some(format!("CALC: {e}"));
                        }
                    },
                    CalcContext::Lines(lines) => {
                        let mut sum = crate::value::Number::Int(0);
                        let mut err: Option<String> = None;
                        for (i, line) in lines.iter().enumerate() {
                            if line.trim().is_empty() {
                                continue; // blank lines are skipped
                            }
                            match self.calc_eval(line).and_then(|v| {
                                v.to_number().map_err(|_| "not a number".to_string())
                            }) {
                                Ok(n) => sum = crate::value::Number::add(&sum, &n),
                                Err(e) => {
                                    err = Some(format!("line {}: {e}", i + 1));
                                    break;
                                }
                            }
                        }
                        match err {
                            Some(m) => {
                                self.ctx.env.error = 1;
                                *self.ctx.status = Some(format!("CALC: {m}"));
                            }
                            None => {
                                let s = crate::value::Value::from_number(sum).to_str_dp(dp);
                                self.ctx.host.calc_insert_result(&s);
                                self.ctx.host.set_last_calc(&s);
                                *self.ctx.status = Some(format!("total = {s}"));
                            }
                        }
                    }
                }
                Ok(Flow::Next)
            }
            "CLRSUM" => {
                // Cursor addition: (re)set the running total to 0.
                self.ctx.host.sum_clear();
                *self.ctx.status = Some("running total cleared (sum = 0)".to_string());
                Ok(Flow::Next)
            }
            "ADDSUM" => {
                // Add the number under the caret to the running total.
                match self.ctx.host.sum_add_under_cursor() {
                    Ok((added, total)) => {
                        *self.ctx.status = Some(format!("+ {added}  (sum = {total})"));
                    }
                    Err(e) => {
                        self.ctx.env.error = 1;
                        *self.ctx.status = Some(format!("ADDSUM: {e}"));
                    }
                }
                Ok(Flow::Next)
            }
            "PUTSUM" => {
                // Put the running total at the caret.
                let total = self.ctx.host.sum_put();
                *self.ctx.status = Some(format!("sum = {total}"));
                Ok(Flow::Next)
            }
            "PV" | "PREVIEW" => {
                let a = args.trim();
                let kind = if a.is_empty() {
                    ViewToggle::Toggle
                } else if a.eq_ignore_ascii_case("on") {
                    ViewToggle::On
                } else if a.eq_ignore_ascii_case("off") {
                    ViewToggle::Off
                } else {
                    return Err(XplError::trappable(format!(
                        "{base}: expected on|off (or no argument to toggle), got `{a}`"
                    )));
                };
                self.host_op(EditorOp::SetView { flag: ViewFlag::Preview, kind })?;
                Ok(Flow::Next)
            }
            // GUI sizing knobs (take a number): UISCALE zoom, UIFONT chrome pt, EDFONT editor
            // pt, LEADING/LSP line-spacing %, OF left margin cols. `$UISCALE/$UIFONT/$EDFONT/
            // $LSP/$OF` read them.
            "UISCALE" | "UIFONT" | "EDFONT" | "LEADING" | "LSP" | "OF" | "OFL" | "OFR"
            | "TXTFONT" | "TXTSCALE" | "DWFONT" | "DWSCALE" => {
                let kind = match base {
                    "UISCALE" => UiSizeKind::Scale,
                    "UIFONT" => UiSizeKind::ChromeFont,
                    "EDFONT" => UiSizeKind::EditorFont,
                    "LEADING" | "LSP" => UiSizeKind::Leading,
                    "OFL" => UiSizeKind::MarginLeft,
                    "OFR" => UiSizeKind::MarginRight,
                    "TXTFONT" => UiSizeKind::TxtFont,
                    "TXTSCALE" => UiSizeKind::TxtScale,
                    "DWFONT" => UiSizeKind::DwFont,
                    "DWSCALE" => UiSizeKind::DwScale,
                    _ => UiSizeKind::MarginBoth, // OF
                };
                // A leading explicit `+`/`-` makes it RELATIVE (`EDFONT +2`, `UIFONT -1`): the host
                // adds `value` to the knob's current value. `-2` parses to `-2.0`, so the add still
                // works; a bare number (no sign) replaces the value as before.
                let arg = args.trim();
                let relative = arg.starts_with('+') || arg.starts_with('-');
                let value: f32 = arg.parse().map_err(|_| {
                    XplError::trappable(format!("{base}: expected a number, got {arg:?}"))
                })?;
                self.host_op(EditorOp::SetUiSize { kind, value, relative })?;
                Ok(Flow::Next)
            }
            "UIRESET" => {
                self.host_op(EditorOp::UiReset)?;
                Ok(Flow::Next)
            }
            // `EDFAMILY <name>` — set the editor font family by name ("" / "default" resets). The
            // `EDFONTS` picker dialog is GUI sugar over this. Read back via `$EDFAMILY`.
            "EDFAMILY" => {
                self.host_op(EditorOp::SetEditorFontFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "UIFAMILY" => {
                self.host_op(EditorOp::SetUiFontFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "TXTFAMILY" => {
                self.host_op(EditorOp::SetTxtFontFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "DWFAMILY" => {
                self.host_op(EditorOp::SetDwFontFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "EDSTYLE" => {
                self.host_op(EditorOp::SetEditorFontStyle(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "UISTYLE" => {
                self.host_op(EditorOp::SetUiFontStyle(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "TXTSTYLE" => {
                self.host_op(EditorOp::SetTxtFontStyle(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "DWSTYLE" => {
                self.host_op(EditorOp::SetDwFontStyle(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "DWRESET" => {
                self.host_op(EditorOp::DwReset)?;
                Ok(Flow::Next)
            }
            // Plural verbs: open the GUI font-picker dialog for the editor / UI chrome / TXT window
            // (family + size + live preview). The dialog is GUI sugar over the family/size setters.
            "EDFONTS" => {
                self.host_op(EditorOp::EditorFontPicker)?;
                Ok(Flow::Next)
            }
            "UIFONTS" => {
                self.host_op(EditorOp::UiFontPicker)?;
                Ok(Flow::Next)
            }
            "TXTFONTS" => {
                self.host_op(EditorOp::TxtFontPicker)?;
                Ok(Flow::Next)
            }
            "DWFONTS" => {
                self.host_op(EditorOp::DwFontPicker)?;
                Ok(Flow::Next)
            }
            // Markdown Preview / `TOPDF` fonts (one setting drives both). `PVFAMILY`/`PVCODE` set the
            // body / code family (`""`/`default` → Typst default); `PVSIZE <n|default>` the body size;
            // `PVFONTS`/`PVCODES` open the GUI pickers. Read via `$PVFAMILY`/`$PVSIZE`/`$PVCODE`.
            "PVFAMILY" => {
                self.host_op(EditorOp::SetPreviewFontFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "PVCODE" => {
                self.host_op(EditorOp::SetPreviewCodeFamily(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "PVSIZE" => {
                let a = args.trim();
                let value = if a.is_empty() || a.eq_ignore_ascii_case("default") {
                    0.0 // sentinel → clear to None (a 0pt body font is meaningless)
                } else {
                    a.parse::<f32>().map_err(|_| {
                        XplError::trappable(format!(
                            "PVSIZE: expected a number or 'default', got {a:?}"
                        ))
                    })?
                };
                self.host_op(EditorOp::SetUiSize {
                    kind: UiSizeKind::PreviewFont,
                    value,
                    relative: false,
                })?;
                Ok(Flow::Next)
            }
            // `PVCODESIZE <n|default>` — the code (monospace) size in points; `default`/empty makes code
            // inherit the body size again. `$PVCODESIZE`; SaveCfg.
            "PVCODESIZE" => {
                let a = args.trim();
                let value = if a.is_empty() || a.eq_ignore_ascii_case("default") {
                    0.0 // sentinel → clear to None (inherit the body size)
                } else {
                    a.parse::<f32>().map_err(|_| {
                        XplError::trappable(format!(
                            "PVCODESIZE: expected a number or 'default', got {a:?}"
                        ))
                    })?
                };
                self.host_op(EditorOp::SetUiSize {
                    kind: UiSizeKind::PreviewCodeFont,
                    value,
                    relative: false,
                })?;
                Ok(Flow::Next)
            }
            // `PVMARGIN <len>` sets all four Preview/`TOPDF` page margins; `PVMARGINL/R/T/B` set one
            // side. `<len>` = a number with an optional unit (`in` default, `cm`/`mm`/`pt`). Bare
            // reports the current value. `$PVMARGIN`/`$PVMARGINL/R/T/B`; SaveCfg.
            "PVMARGIN" | "PVMARGINL" | "PVMARGINR" | "PVMARGINT" | "PVMARGINB" => {
                let side = match base {
                    "PVMARGINL" => PvMarginSide::Left,
                    "PVMARGINR" => PvMarginSide::Right,
                    "PVMARGINT" => PvMarginSide::Top,
                    "PVMARGINB" => PvMarginSide::Bottom,
                    _ => PvMarginSide::All,
                };
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("{base}: {}", self.special_str(base)));
                    return Ok(Flow::Next);
                }
                self.host_op(EditorOp::SetPvMargin { side, value: args.trim().to_string() })?;
                Ok(Flow::Next)
            }
            // Preview/`TOPDF` page format, spacing, and page numbering. Each bare verb reports the
            // current value; with an argument it sets it. GFM/HTML only (authored Typst self-styles).
            // `GREPBEFORE <n>` / `GREPAFTER <n>` — leading / trailing context lines for GREP results;
            // `GREPBLANK 0|1` (or on|off) — a blank line between result groups. Bare reports the value.
            "GREPBEFORE" | "GREPAFTER" => {
                let a = args.trim();
                if a.is_empty() {
                    *self.ctx.status = Some(format!("{base}: {}", self.special_str(base)));
                    return Ok(Flow::Next);
                }
                let n: usize = a.parse().map_err(|_| {
                    XplError::trappable(format!("{base}: expected a line count (0 or more), got {a:?}"))
                })?;
                self.host_op(if base == "GREPBEFORE" {
                    EditorOp::SetGrepBefore(n)
                } else {
                    EditorOp::SetGrepAfter(n)
                })?;
                Ok(Flow::Next)
            }
            "GREPBLANK" => {
                let a = args.trim();
                if a.is_empty() {
                    *self.ctx.status = Some(format!("GREPBLANK: {}", self.special_str("GREPBLANK")));
                    return Ok(Flow::Next);
                }
                let on = match a.to_ascii_lowercase().as_str() {
                    "1" | "on" | "true" | "yes" => true,
                    "0" | "off" | "false" | "no" => false,
                    _ => {
                        return Err(XplError::trappable(format!(
                            "GREPBLANK: expected 0|1 (or on|off), got {a:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetGrepBlank(on))?;
                Ok(Flow::Next)
            }
            "PVPAPER" | "PVLEADING" | "PVSPACING" | "PVPAGENUM" | "PVPAGEPOS" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("{base}: {}", self.special_str(base)));
                    return Ok(Flow::Next);
                }
                let arg = args.trim().to_string();
                let op = match base {
                    "PVPAPER" => EditorOp::SetPreviewPaper(arg),
                    "PVLEADING" => EditorOp::SetPreviewLeading(arg),
                    "PVSPACING" => EditorOp::SetPreviewSpacing(arg),
                    "PVPAGENUM" => EditorOp::SetPreviewPageNumber(arg),
                    _ => EditorOp::SetPreviewPageAlign(arg),
                };
                self.host_op(op)?;
                Ok(Flow::Next)
            }
            // `PVJUSTIFY on|off` — justify Preview/`TOPDF` body text (bare reports).
            "PVJUSTIFY" => {
                match args.trim().to_ascii_lowercase().as_str() {
                    "" => *self.ctx.status = Some(format!("PVJUSTIFY: {}", self.special_str("PVJUSTIFY"))),
                    "on" => self.host_op(EditorOp::SetPreviewJustify(true))?,
                    "off" => self.host_op(EditorOp::SetPreviewJustify(false))?,
                    other => {
                        return Err(XplError::trappable(format!(
                            "PVJUSTIFY: expected on or off, got {other:?}"
                        )))
                    }
                }
                Ok(Flow::Next)
            }
            // First-line indent + page header/footer. Bare reports; an argument sets it (""/`off`
            // clears). Header/footer use `raw_args` so `{tokens}` and `$` survive verbatim.
            "PVINDENT" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("PVINDENT: {}", self.special_str("PVINDENT")));
                    return Ok(Flow::Next);
                }
                self.host_op(EditorOp::SetPreviewIndent(args.trim().to_string()))?;
                Ok(Flow::Next)
            }
            "PVHEADER" | "PVFOOTER" => {
                if raw_args.trim().is_empty() {
                    *self.ctx.status = Some(format!("{base}: {}", self.special_str(base)));
                    return Ok(Flow::Next);
                }
                let text = raw_args.trim().to_string();
                let op = if base == "PVHEADER" {
                    EditorOp::SetPreviewHeader(text)
                } else {
                    EditorOp::SetPreviewFooter(text)
                };
                self.host_op(op)?;
                Ok(Flow::Next)
            }
            "PVFONTS" => {
                self.host_op(EditorOp::PreviewFontPicker)?;
                Ok(Flow::Next)
            }
            "PVCODES" => {
                self.host_op(EditorOp::PreviewCodePicker)?;
                Ok(Flow::Next)
            }
            // Insert a newline in the active document's line-ending style (bound to the Enter
            // key in text mode; the CMline intercepts Enter→submit before the keymap). `$NEWLINE`.
            "ENTER" => {
                self.host_op(EditorOp::Newline)?;
                Ok(Flow::Next)
            }
            // Insert a tab character (bind to the Tab key, e.g. `15=TAB`). Use a COMMAND (not a
            // `"\t"` typing cell): a typing cell falls through to egui, which steals Tab for focus
            // navigation and inserts nothing; a command is captured by the keymap first. The canvas
            // renders `\t` at the `TB`/`$TB` width.
            // `TAB` (bare / `in`) indents; `TAB out` outdents. On a selection, every touched line is
            // indented/outdented; with no selection `TAB in` inserts one indent unit at the caret.
            // Soft tabs (`TABSPACES on`) make the unit `$TB` spaces. Bind Shift+Tab to `TAB out`.
            "TAB" => {
                let out = match args.trim().to_ascii_lowercase().as_str() {
                    "" | "in" => false,
                    "out" => true,
                    other => {
                        return Err(XplError::trappable(format!(
                            "TAB: expected in or out, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::Tab { out })?;
                Ok(Flow::Next)
            }
            // `TABSPACES on|off` — soft tabs (insert `$TB` spaces instead of a tab). `$TABSPACES`.
            "TABSPACES" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "TABSPACES: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetTabSpaces(on))?;
                Ok(Flow::Next)
            }
            // `TABINDENT on|off` — auto-indent: ENTER starts the new line at the previous line's
            // indentation. `$TABINDENT` reads it; `$INDENTED` reads the current line's indent (columns).
            "TABINDENT" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "TABINDENT: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetTabIndent(on))?;
                Ok(Flow::Next)
            }
            // `WRAPINDENT on|off` — hanging indent: soft-wrapped continuation rows align under the
            // line's own indentation (`$INDENTED`). `$WRAPINDENT` reads it.
            "WRAPINDENT" => {
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "WRAPINDENT: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetWrapIndent(on))?;
                Ok(Flow::Next)
            }
            // `BK off|on|debug` — inline breakpoint (a scriptable extension of Ctrl+Break). `off`
            // is a no-op (a disabled breakpoint); `on` pauses with a Y/N stop prompt; `debug` pauses
            // and shows the live variables. Bare `BK` = `on`. In a headless `--run` batch these
            // auto-continue (inert). The editor/GUI resolves the pause (continue or abort).
            "BK" => match args.trim().to_ascii_lowercase().as_str() {
                "off" => Ok(Flow::Next),
                "" | "on" => {
                    self.suspend = Some(SuspendReason::Breakpoint { debug: false, line, col });
                    Ok(Flow::Suspend)
                }
                "debug" => {
                    self.suspend = Some(SuspendReason::Breakpoint { debug: true, line, col });
                    Ok(Flow::Suspend)
                }
                other => {
                    Err(XplError::trappable(format!("BK: expected off|on|debug, got {other:?}")))
                }
            },
            // Colours: editor canvas (`FGCOLOR`/`BGCOLOR`) or UI chrome (`UIFGCOLOR`/`UIBGCOLOR`).
            // Each takes three 0-255 ints (`R G B`); with the `/HSL` switch the triple is read as
            // `H S L` on the same 0-255 scale and converted to RGB. Read back via the `$…` forms.
            // `FGCOLOR` is selection-aware: on a selection it colours just the selected text (a
            // transient span), else it sets the global editor foreground. `FGCOLOR default` clears
            // the selection's colour (or resets the global). Handled by the host via `active()`.
            "FGCOLOR" => {
                let arg = args.trim();
                let opt = if arg.eq_ignore_ascii_case("default") {
                    None
                } else {
                    let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                    Some(parse_color("FGCOLOR", &args, hsl)?)
                };
                self.host_op(EditorOp::FgColor(opt))?;
                Ok(Flow::Next)
            }
            "MDA" => {
                // XyWrite-style "mode attribute" on the SELECTION as a persistent block. Two kinds:
                //   • an ATTRIBUTE keyword — `MDA BO|IT|BI|UL|BU` (bold/italic/underline combos), `MDA NM`
                //     strips attributes;
                //   • a COLOUR — `MDA <color>` foreground, `MDA/B <color>` background, `MDA off` (or bare
                //     `MDA`) clears that colour layer.
                // Attributes and colours are INDEPENDENT layers; all persist to the `.xymeta` sidecar.
                let arg = args.trim();
                // An attribute keyword wins (colours never look like `BO`/`IT`/…); anything else is a colour.
                if let Some(attr_action) = crate::host::parse_mda_attr(arg) {
                    self.host_op(EditorOp::ModeAttr { attrs: attr_action })?;
                    return Ok(Flow::Next);
                }
                let background = vswitches.iter().any(|s| s.eq_ignore_ascii_case("B") || s.eq_ignore_ascii_case("BG"));
                let rgb = if arg.is_empty() || arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("default") {
                    None
                } else {
                    let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                    Some(parse_color("MDA", &args, hsl)?)
                };
                self.host_op(EditorOp::ModeColor { background, rgb })?;
                Ok(Flow::Next)
            }
            // `DWFGCOLOR`/`DWBGCOLOR <r g b>` — the DIR/W window's text/background colour (`/HSL`);
            // bare or `… default` clears it so the window inherits the editor's colour. `…S` opens the
            // picker. Kept separate from the RGB-only verbs below because it is an OPTION (unset =
            // inherit), which lets `DWFGCOLOR default` round-trip from a saved config.
            "DWFGCOLOR" | "DWBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetDwColor { fg: base == "DWFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "BGCOLOR" | "UIFGCOLOR" | "UIBGCOLOR" | "CRCOLOR" | "CMCRCOLOR"
            | "SEPCOLOR" | "TXTFGCOLOR" | "TXTBGCOLOR" | "SELBGCOLOR"
            | "SELFGCOLOR" | "FINDBGCOLOR" | "FINDFGCOLOR" | "SPELLCOLOR" | "GRAMMARCOLOR"
            | "PANECOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let rgb = parse_color(base, &args, hsl)?;
                let ground = match base {
                    "BGCOLOR" => ColorGround::Bg,
                    "PANECOLOR" => ColorGround::PaneBorder,
                    "UIFGCOLOR" => ColorGround::UiFg,
                    "UIBGCOLOR" => ColorGround::UiBg,
                    "SEPCOLOR" => ColorGround::Separator,
                    "TXTFGCOLOR" => ColorGround::TxtFg,
                    "TXTBGCOLOR" => ColorGround::TxtBg,
                    "SELBGCOLOR" => ColorGround::SelBg,
                    "SELFGCOLOR" => ColorGround::SelFg,
                    "FINDBGCOLOR" => ColorGround::FindBg,
                    "FINDFGCOLOR" => ColorGround::FindFg,
                    "SPELLCOLOR" => ColorGround::SpellUnderline,
                    "GRAMMARCOLOR" => ColorGround::GrammarUnderline,
                    "CMCRCOLOR" => ColorGround::CmlineCaret,
                    _ => ColorGround::Caret, // CRCOLOR
                };
                self.host_op(EditorOp::SetColor { ground, rgb })?;
                Ok(Flow::Next)
            }
            // Plural colour verbs: pop the GUI colour-picker dialog for that target (GUI sugar over
            // the singular `*COLOR` setters — the dialog applies the setter on OK).
            "FGCOLORS" | "BGCOLORS" | "UIFGCOLORS" | "UIBGCOLORS" | "CRCOLORS" | "CMCRCOLORS"
            | "SEPCOLORS" | "TXTFGCOLORS" | "TXTBGCOLORS" | "DWFGCOLORS" | "DWBGCOLORS"
            | "SELBGCOLORS" | "SELFGCOLORS" | "FINDBGCOLORS" | "FINDFGCOLORS"
            | "SPELLCOLORS" | "GRAMMARCOLORS" | "TABXCOLORS" | "PANECOLORS" | "BALPAIRSCOLORS"
            | "EMPHFGCOLORS" | "EMPHBGCOLORS" => {
                let ground = match base {
                    "FGCOLORS" => ColorGround::Fg,
                    "PANECOLORS" => ColorGround::PaneBorder,
                    "BALPAIRSCOLORS" => ColorGround::BalPairs,
                    "EMPHFGCOLORS" => ColorGround::EmphFg,
                    "EMPHBGCOLORS" => ColorGround::EmphBg,
                    "BGCOLORS" => ColorGround::Bg,
                    "UIFGCOLORS" => ColorGround::UiFg,
                    "UIBGCOLORS" => ColorGround::UiBg,
                    "SEPCOLORS" => ColorGround::Separator,
                    "TABXCOLORS" => ColorGround::TabClose,
                    "TXTFGCOLORS" => ColorGround::TxtFg,
                    "TXTBGCOLORS" => ColorGround::TxtBg,
                    "DWFGCOLORS" => ColorGround::DwFg,
                    "DWBGCOLORS" => ColorGround::DwBg,
                    "SELBGCOLORS" => ColorGround::SelBg,
                    "SELFGCOLORS" => ColorGround::SelFg,
                    "FINDBGCOLORS" => ColorGround::FindBg,
                    "FINDFGCOLORS" => ColorGround::FindFg,
                    "SPELLCOLORS" => ColorGround::SpellUnderline,
                    "GRAMMARCOLORS" => ColorGround::GrammarUnderline,
                    "CMCRCOLORS" => ColorGround::CmlineCaret,
                    _ => ColorGround::Caret, // CRCOLORS
                };
                self.host_op(EditorOp::OpenColorPicker(ground))?;
                Ok(Flow::Next)
            }
            // `MDAS` — colour-picker for the `MDA` block colour of the SELECTION: bare = foreground,
            // `/B` (or `/BG`) = background. On accept the picker runs `MDA`/`MDA/B <r g b>`.
            "MDAS" => {
                let background =
                    vswitches.iter().any(|s| s.eq_ignore_ascii_case("B") || s.eq_ignore_ascii_case("BG"));
                let ground = if background { ColorGround::MdaBg } else { ColorGround::MdaFg };
                self.host_op(EditorOp::OpenColorPicker(ground))?;
                Ok(Flow::Next)
            }
            // `HLCOLORS <kind>` — colour-picker dialog for a syntax-highlight token kind.
            "HLCOLORS" => {
                let kind = args.trim();
                if kind.is_empty() {
                    return Err(XplError::trappable("HLCOLORS: a token kind is required"));
                }
                self.host_op(EditorOp::OpenHighlightColorPicker(kind.to_string()))?;
                Ok(Flow::Next)
            }
            // Syntax-highlight theme: `HLCOLOR <kind> <r g b>` recolors a token kind (`/HSL` reads
            // the triple as H S L); `HLCOLOR <kind>` / `HLCOLOR <kind> default` clears it → FGCOLOR.
            // `HLRESET` restores the built-in defaults.
            "HLCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let (kind, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
                if kind.is_empty() {
                    return Err(XplError::trappable("HLCOLOR: <kind> [<r g b> | default]"));
                }
                let rest = rest.trim();
                let color = if rest.is_empty() || rest.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color("HLCOLOR", rest, hsl)?)
                };
                self.host_op(EditorOp::SetHighlightColor { kind: kind.to_string(), color })?;
                Ok(Flow::Next)
            }
            "HLRESET" => {
                self.host_op(EditorOp::ResetHighlightTheme)?;
                Ok(Flow::Next)
            }
            // Regex grammars: `HLLANG <name> <.ext1,.ext2>` declares (or resets) a grammar bound to
            // file extensions + selects it; `HLRULE <kind> <regex>` appends a per-line rule to it.
            // A grammar is just an XPL script of these — `RUN` it, then `HIGHLIGHT on` applies it by
            // extension. Rules apply first-match-wins; `<kind>` is any name (see `HLCOLOR`).
            "HLLANG" => {
                let t = args.trim();
                let (name, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
                if name.is_empty() {
                    return Err(XplError::trappable("HLLANG: <name> <.ext1,.ext2,...>"));
                }
                let exts: Vec<String> = rest
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .map(|e| e.trim())
                    .filter(|e| !e.is_empty())
                    .map(|e| e.to_string())
                    .collect();
                self.host_op(EditorOp::DefineHighlightLang { name: name.to_string(), exts })?;
                Ok(Flow::Next)
            }
            "HLRULE" => {
                // Use raw_args, NOT the substituted `args`: a regex legitimately contains `$`
                // (end-anchor / `\$` literal dollar), which `$`-substitution would otherwise eat.
                let t = raw_args.trim();
                let (kind, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
                let rest = rest.trim();
                if kind.is_empty() || rest.is_empty() {
                    // Fatal (like a bad regex below) so a grammar RUN stops and reports the broken line.
                    return Err(XplError::fatal("HLRULE: expected <kind> <regex>"));
                }
                // Allow an optional surrounding pair of double quotes (a regex may hold spaces).
                let regex = rest
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(rest);
                let perl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("P"));
                self.host_op(EditorOp::AddHighlightRule {
                    kind: kind.to_string(),
                    regex: regex.to_string(),
                    perl,
                })?;
                Ok(Flow::Next)
            }
            // List the token kinds `HLCOLOR` accepts, with each one's current colour — shown in a
            // scrollable modal `TXT` dialog (parks the script until dismissed, like `TXT`).
            "HLKINDS" => {
                let text = self.ctx.host.highlight_kinds().join("\n");
                if self.is_capturing() {
                    self.capture_push(&text);
                    return Ok(Flow::Next);
                }
                // Carry each kind's colour so the dialog paints its label in that colour.
                let line_colors = Some(self.ctx.host.highlight_kind_colors());
                self.suspend = Some(SuspendReason::ShowText {
                    title: Some("Highlight kinds".to_string()),
                    text,
                    columns: None,
                    line_colors,
                });
                Ok(Flow::Suspend)
            }
            // Caret shape: `CRSHAPE bar|block|underline` (aliases line/ibeam, underscore). `$CRSHAPE`.
            "CRSHAPE" => {
                let shape = CaretShape::parse(&args).ok_or_else(|| {
                    XplError::trappable(format!(
                        "CRSHAPE: expected bar|block|underline, got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetCaretShape(shape))?;
                Ok(Flow::Next)
            }
            // Command-line caret shape: `CMCRSHAPE bar|block|underline` (same values as CRSHAPE).
            // `$CMCRSHAPE`.
            "CMCRSHAPE" => {
                let shape = CaretShape::parse(&args).ok_or_else(|| {
                    XplError::trappable(format!(
                        "CMCRSHAPE: expected bar|block|underline, got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetCmlineCaretShape(shape))?;
                Ok(Flow::Next)
            }
            // `SPELLSTYLE`/`GRAMMARSTYLE dotted|solid|squiggle` — the underline SHAPE for live
            // spelling / grammar issues; bare reports it. `$SPELLSTYLE`/`$GRAMMARSTYLE`. SaveCfg.
            "SPELLSTYLE" | "GRAMMARSTYLE" => {
                let grammar = base == "GRAMMARSTYLE";
                let special = if grammar { "GRAMMARSTYLE" } else { "SPELLSTYLE" };
                if args.trim().is_empty() {
                    *self.ctx.status =
                        Some(format!("{special}: {}", self.special_str(special)));
                    return Ok(Flow::Next);
                }
                let style = UnderlineStyle::parse(&args).ok_or_else(|| {
                    XplError::trappable(format!(
                        "{base}: expected dotted|solid|squiggle (optionally -thick), got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetLintStyle { grammar, style })?;
                Ok(Flow::Next)
            }
            // `CMPLACE top|bottom|hidden` — where the command line is drawn; bare reports it. `$CMPLACE`.
            "CMPLACE" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("command line: {}", self.special_str("CMPLACE")));
                    return Ok(Flow::Next);
                }
                let place = CmlinePlacement::parse(&args).ok_or_else(|| {
                    XplError::trappable(format!(
                        "CMPLACE: expected top|bottom|hidden, got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetCmlinePlacement(place))?;
                Ok(Flow::Next)
            }
            // `STATUSLINE top|bottom` — where the status line is drawn; bare reports it. `top` puts it
            // just under the command line (XyWrite-style); `bottom` (default) keeps it at the bottom
            // edge. `$STATUSLINE`.
            "STATUSLINE" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("status line: {}", self.special_str("STATUSLINE")));
                    return Ok(Flow::Next);
                }
                let top = match args.trim().to_ascii_lowercase().as_str() {
                    "top" => true,
                    "bottom" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "STATUSLINE: expected top or bottom, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetStatusLineTop(top))?;
                Ok(Flow::Next)
            }
            // `CMBLANK on|off` (bare = toggle) — whether the command line is blanked after a command
            // runs from it. Off (default) keeps the command for re-running (B23). `$CMBLANK`.
            "CMBLANK" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "CMBLANK: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetCmlineBlank(want))?;
                Ok(Flow::Next)
            }
            // `SHOWONETAB on|off` (bare = toggle) — whether the tab strip shows when only one file is
            // open. Off (default) keeps a single doc uncluttered. `$SHOWONETAB`.
            // `CARETMOTION logical|visual` — the order Left/Right move the caret in. `logical` (default)
            // = storage order; `visual` = screen order (in RTL text the arrows follow the reading flow).
            "CARETMOTION" => {
                let visual = match args.trim().to_ascii_lowercase().as_str() {
                    "logical" => false,
                    "visual" => true,
                    other => {
                        return Err(XplError::trappable(format!(
                            "CARETMOTION: expected logical or visual, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetCaretMotion(visual))?;
                Ok(Flow::Next)
            }
            "SHOWONETAB" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "SHOWONETAB: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetShowOneTab(want))?;
                Ok(Flow::Next)
            }
            // `WRAP on|off` (bare = toggle) — word-wrap long lines to the window (on, default) or let
            // them run past the right edge with a horizontal scrollbar (off).
            "WRAP" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "WRAP: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetWrap(want))?;
                Ok(Flow::Next)
            }
            // `WRAPHANG <n>` — hang wrapped continuation rows `n` spaces (when WRAP is on and WRAPINDENT
            // is off). Bare reports the current value; 0 (default) = flush wrap.
            "WRAPHANG" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("wrap hang: {}", self.special_str("WRAPHANG")));
                    return Ok(Flow::Next);
                }
                let n: usize = args.trim().parse().map_err(|_| {
                    XplError::trappable(format!("WRAPHANG: expected a number, got {:?}", args.trim()))
                })?;
                self.host_op(EditorOp::SetWrapHang(n))?;
                Ok(Flow::Next)
            }
            // `REFLOWWIDTH <n>` — the default column width `REFLOW` uses when given no width (default
            // 65). Bare reports the current value. `$REFLOWWIDTH`; persisted by SAVECFG.
            "REFLOWWIDTH" => {
                if args.trim().is_empty() {
                    *self.ctx.status =
                        Some(format!("reflow width: {}", self.special_str("REFLOWWIDTH")));
                    return Ok(Flow::Next);
                }
                let n: usize = args.trim().parse().map_err(|_| {
                    XplError::trappable(format!(
                        "REFLOWWIDTH: expected a number, got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetReflowWidth(n))?;
                Ok(Flow::Next)
            }
            // `REFLOW [n]` — hard-wrap the selection (else the paragraph at the caret) to `n` columns,
            // or the `REFLOWWIDTH` default when omitted. Collapses internal whitespace, never splits a
            // word, keeps the first line's indent, and preserves blank-line paragraph breaks.
            "REFLOW" => {
                let width = if args.trim().is_empty() {
                    self.special_str("REFLOWWIDTH").parse().unwrap_or(65)
                } else {
                    args.trim().parse().map_err(|_| {
                        XplError::trappable(format!(
                            "REFLOW: expected a column width, got {:?}",
                            args.trim()
                        ))
                    })?
                };
                self.host_op(EditorOp::Reflow { width })?;
                *self.ctx.status = Some(format!("reflowed to {width} columns"));
                Ok(Flow::Next)
            }
            // `UNTITLEDTAB on|off` (bare = toggle) — whether the blank "Untitled" scratch tab is closed
            // when a document is opened (and reappears when the last document is closed). On (default).
            // `$UNTITLEDTAB`.
            "UNTITLEDTAB" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "UNTITLEDTAB: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetUntitledTab(want))?;
                Ok(Flow::Next)
            }
            // `GOTOOPEN on|off` (bare = toggle) — when on, opening a file that is already open skips
            // the "already open" prompt and switches straight to its tab. Off (default) prompts.
            // `$GOTOOPEN`.
            "GOTOOPEN" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "GOTOOPEN: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetGotoOpen(want))?;
                Ok(Flow::Next)
            }
            // `DIMONBLUR on|off` (bare = toggle) — dim the window slightly when it loses focus. Off by
            // default. SaveCfg. `$DIMONBLUR`.
            "DIMONBLUR" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "DIMONBLUR: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetDimOnBlur(want))?;
                Ok(Flow::Next)
            }
            // `CLOSEBACK on|off` (bare = toggle) — whether closing a tab returns to the LAST-VISITED
            // tab instead of the left neighbor. `$CLOSEBACK`.
            "CLOSEBACK" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "CLOSEBACK: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetCloseBack(want))?;
                Ok(Flow::Next)
            }
            // `AUTOREOPEN on|off` (bare = toggle) — reload a file automatically when another program
            // changes it on disk (clean buffers only; unsaved edits are kept). Off by default; NOT
            // gated by TRUST (no external code runs). SaveCfg. `$AUTOREOPEN`.
            "AUTOREOPEN" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "AUTOREOPEN: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetAutoReopen(want))?;
                Ok(Flow::Next)
            }
            // `ERRORLOG on|off` (bare = toggle) — append every trappable + fatal error to the log
            // file as a TSV row (timestamp, code, class, command, location, doc, message). Off by
            // default; NOT TRUST-gated (it only writes a local log). SaveCfg. `$ERRORLOG`.
            "ERRORLOG" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "ERRORLOG: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetErrorLog(want))?;
                Ok(Flow::Next)
            }
            // `ERRORLOGFILE <path>` — set the error-log file (bare resets to the default,
            // `xyscript-errors.log` in `$XSDIR`); a given path resolves against `$CWD`. SaveCfg.
            // `$ERRORLOGFILE`.
            "ERRORLOGFILE" => {
                let path = self.subst(&args).trim().to_string();
                self.host_op(EditorOp::SetErrorLogFile(path))?;
                Ok(Flow::Next)
            }
            // `MARKGUTTER off|on|persist` (bare = toggle off↔on) — whether bookmarks show a dot in the
            // left gutter, and whether the strip is reserved even without bookmarks (`persist`). `On` by
            // default. Persisted by SaveCfg. `$MARKGUTTER`.
            "MARKGUTTER" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(MarkGutterMode::On),
                    "off" => Some(MarkGutterMode::Off),
                    "persist" => Some(MarkGutterMode::Persist),
                    other => {
                        return Err(XplError::trappable(format!(
                            "MARKGUTTER: expected off, on, or persist, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetMarkGutter(want))?;
                Ok(Flow::Next)
            }
            // `MARKFGCOLOR`/`MARKBGCOLOR <r g b>` — the bookmark-gutter dot colour / strip background
            // (`/HSL`); bare or `… default` clears it (dot → bookmark orange, strip → transparent).
            // `MARKFGCOLORS`/`MARKBGCOLORS` open the picker. `$MARKFGCOLOR`/`$MARKBGCOLOR`.
            "MARKFGCOLOR" | "MARKBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetMarkColor { fg: base == "MARKFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "MARKFGCOLORS" | "MARKBGCOLORS" => {
                self.host_op(EditorOp::OpenMarkColorPicker { fg: base == "MARKFGCOLORS" })?;
                Ok(Flow::Next)
            }
            // `LNFGCOLOR <r g b>` / `LNBGCOLOR <r g b>` — the line-number gutter's text / background
            // colour (`/HSL`); bare or `… default` clears it (text → muted slate, gutter → transparent).
            // `LNFGCOLORS`/`LNBGCOLORS` open the picker. `$LNFGCOLOR`/`$LNBGCOLOR`.
            "LNFGCOLOR" | "LNBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetLineNumColor { fg: base == "LNFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "LNFGCOLORS" | "LNBGCOLORS" => {
                self.host_op(EditorOp::OpenLineNumColorPicker { fg: base == "LNFGCOLORS" })?;
                Ok(Flow::Next)
            }
            // The modal `MSG` box's colours: `MSGHFGCOLOR` (the "Message" title text), `MSGBGCOLOR`
            // (the box background), `MSGOKFGCOLOR`/`MSGOKBGCOLOR` (the OK button). `<r g b>` (`/HSL`);
            // bare or `… default` clears (follow the theme). The `…S` verbs open the picker.
            "MSGHFGCOLOR" | "MSGBGCOLOR" | "MSGOKFGCOLOR" | "MSGOKBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                let slot = match base {
                    "MSGHFGCOLOR" => MsgColorSlot::HeaderFg,
                    "MSGBGCOLOR" => MsgColorSlot::Bg,
                    "MSGOKFGCOLOR" => MsgColorSlot::OkFg,
                    _ => MsgColorSlot::OkBg,
                };
                self.host_op(EditorOp::SetMsgColor { slot, color })?;
                Ok(Flow::Next)
            }
            "MSGHFGCOLORS" | "MSGBGCOLORS" | "MSGOKFGCOLORS" | "MSGOKBGCOLORS" => {
                let slot = match base {
                    "MSGHFGCOLORS" => MsgColorSlot::HeaderFg,
                    "MSGBGCOLORS" => MsgColorSlot::Bg,
                    "MSGOKFGCOLORS" => MsgColorSlot::OkFg,
                    _ => MsgColorSlot::OkBg,
                };
                self.host_op(EditorOp::OpenMsgColorPicker { slot })?;
                Ok(Flow::Next)
            }
            // `URLHIGHLIGHT on|off` (bare = toggle) — highlight URLs in the document (coloured by
            // `URLFGCOLOR`/`URLBGCOLOR`) and make Ctrl+click open them. Off by default. SaveCfg.
            "URLHIGHLIGHT" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "URLHIGHLIGHT: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetUrlHighlight(want))?;
                Ok(Flow::Next)
            }
            // `PVMARKS on|off` (bare = toggle) — whether preview/`TOPDF` paints the EMPH matches and
            // URL highlights (like the editor). MDA styling renders regardless. `$PVMARKS`; SaveCfg.
            "PVMARKS" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "PVMARKS: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetPreviewMarks(want))?;
                Ok(Flow::Next)
            }
            // `URLFGCOLOR`/`URLBGCOLOR <r g b>` — the URL-highlight text colour / background (`/HSL`);
            // bare or `… default` clears it (fg → link blue, bg → transparent). `URLFGCOLORS`/
            // `URLBGCOLORS` open the picker. `$URLFGCOLOR`/`$URLBGCOLOR`.
            "URLFGCOLOR" | "URLBGCOLOR" => {
                let hsl = vswitches.iter().any(|s| s.eq_ignore_ascii_case("HSL"));
                let t = args.trim();
                let color = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_color(base, t, hsl)?)
                };
                self.host_op(EditorOp::SetUrlColor { fg: base == "URLFGCOLOR", color })?;
                Ok(Flow::Next)
            }
            "URLFGCOLORS" | "URLBGCOLORS" => {
                self.host_op(EditorOp::OpenUrlColorPicker { fg: base == "URLFGCOLORS" })?;
                Ok(Flow::Next)
            }
            // `ULSPACES on|off` (bare = toggle) — whether an `MDA` UNDERLINE runs through the spaces
            // between words (on, default) or breaks at them so only word glyphs are underlined (off).
            // `$ULSPACES`.
            "ULSPACES" => {
                let want = match args.trim().to_ascii_lowercase().as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    other => {
                        return Err(XplError::trappable(format!(
                            "ULSPACES: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::SetUlSpaces(want))?;
                Ok(Flow::Next)
            }
            // Email verbs are listed in BUILTIN_VERBS unconditionally (so they're documented + HELP-able),
            // but the implementation is behind the `email` cargo feature. A build without it reports this
            // rather than "unknown command".
            #[cfg(not(feature = "email"))]
            "SMTPURL" | "MAILUSER" | "MAILFROM" | "MAILPASS" | "MAILDEBUG" | "SENDMAIL" | "ATTACH"
            | "CLEARATTACH" | "IMAPURL" | "MAILBOX" | "LISTMAIL" | "SAVEMAIL" | "DELMAIL" => {
                Err(XplError::trappable(format!(
                    "{base}: email support is not compiled in this build (rebuild with --features email)"
                )))
            }
            // Email (implementation behind `--features email`). SMTP config setters — bare reports the
            // current value via the matching `$`-special. Session-only; the password is never persisted
            // (set it each session, or from $XYSCRIPT_MAILPASS).
            #[cfg(feature = "email")]
            "SMTPURL" | "MAILUSER" | "MAILFROM" | "MAILPASS" | "IMAPURL" | "MAILBOX" => {
                let field = match base {
                    "SMTPURL" => "url",
                    "MAILUSER" => "user",
                    "MAILFROM" => "from",
                    "IMAPURL" => "imapurl",
                    "MAILBOX" => "mailbox",
                    _ => "pass",
                };
                let v = args.trim();
                if v.is_empty() {
                    *self.ctx.status = Some(format!("{base}: {}", self.special_str(base)));
                    return Ok(Flow::Next);
                }
                self.host_op(EditorOp::MailSet { field: field.to_string(), value: v.to_string() })?;
                Ok(Flow::Next)
            }
            // `SENDMAIL [<to> ["subject"]]` — send the active document as an email via curl. With no
            // args, To/Subject/Cc come from a header block at the top of the buffer. `SENDMAIL/F <file>
            // [<to> ["subject"]]` reads the message text from <file> (optionally quoted, for spaces)
            // instead of the active document.
            #[cfg(feature = "email")]
            "SENDMAIL" => {
                // With /F, the FIRST argument is the body-source file (quote-aware); the rest is the
                // usual `[<to> ["subject"]]`.
                let (body_file, rest) = if vswitches.iter().any(|s| s.eq_ignore_ascii_case("F")) {
                    let (f, r) = take_first_arg(args.trim());
                    if f.is_empty() {
                        return Err(XplError::trappable("SENDMAIL/F: a file is required"));
                    }
                    (Some(f), r.to_string())
                } else {
                    (None, args.trim().to_string())
                };
                let a = rest.trim();
                let (to, subject) = if a.is_empty() {
                    (None, None)
                } else {
                    let mut it = a.splitn(2, char::is_whitespace);
                    let to = it.next().unwrap_or("").to_string();
                    let rest = it.next().unwrap_or("").trim();
                    let subject =
                        (!rest.is_empty()).then(|| rest.trim_matches('"').to_string());
                    (Some(to), subject)
                };
                let wait = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let msg = self
                    .host_op_value(EditorOp::SendMail { to, subject, body_file, wait })?
                    .to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `ATTACH <path>` stages a file for the next SENDMAIL; bare `ATTACH` lists staged files.
            #[cfg(feature = "email")]
            "ATTACH" => {
                let p = args.trim();
                if p.is_empty() {
                    let list = self.special_str("ATTACH");
                    *self.ctx.status = Some(if list.is_empty() {
                        "no files staged".to_string()
                    } else {
                        format!("staged: {list}")
                    });
                    return Ok(Flow::Next);
                }
                let msg = self.host_op_value(EditorOp::MailAttach(p.to_string()))?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `CLEARATTACH` — drop all staged attachments.
            #[cfg(feature = "email")]
            "CLEARATTACH" => {
                let msg = self.host_op_value(EditorOp::MailClearAttach)?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `LISTMAIL` — list message headers (Date/From/Subject) from the mailbox to *Shell Output*.
            #[cfg(feature = "email")]
            "LISTMAIL" => {
                let wait = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let msg = self.host_op_value(EditorOp::ListMail { wait })?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `SAVEMAIL <range> [<path>]` — download raw messages (`1-3,6-8,10`) to MSG<nnn>.TXT files
            // in <path> (default $CWD). The optional path may be quoted for spaces.
            #[cfg(feature = "email")]
            "SAVEMAIL" => {
                let a = args.trim();
                if a.is_empty() {
                    return Err(XplError::trappable(
                        "SAVEMAIL: a message range is required (e.g. 1-3,6-8)",
                    ));
                }
                let mut it = a.splitn(2, char::is_whitespace);
                let range = it.next().unwrap_or("").to_string();
                let path = it
                    .next()
                    .map(|p| p.trim().trim_matches('"').to_string())
                    .filter(|p| !p.is_empty());
                let wait = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let msg = self.host_op_value(EditorOp::SaveMail { range, path, wait })?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `DELMAIL <range>` — delete messages by number (`1-3,6-8,10` or `all`).
            #[cfg(feature = "email")]
            "DELMAIL" => {
                let range = args.trim().to_string();
                if range.is_empty() {
                    return Err(XplError::trappable(
                        "DELMAIL: a message range is required (e.g. 1-3,6-8 or all)",
                    ));
                }
                let wait = vswitches.iter().any(|s| s.eq_ignore_ascii_case("W"));
                let msg = self.host_op_value(EditorOp::DelMail { range, wait })?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            // `MAILDEBUG on|off` (bare toggles) — run curl with `-v` so the SMTP dialogue streams to
            // *Shell Output* (deliverability diagnosis). `$MAILDEBUG`.
            #[cfg(feature = "email")]
            "MAILDEBUG" => {
                let value = match args.trim().to_ascii_lowercase().as_str() {
                    "" => "toggle",
                    "on" => "on",
                    "off" => "off",
                    other => {
                        return Err(XplError::trappable(format!(
                            "MAILDEBUG: expected on or off, got {other:?}"
                        )))
                    }
                };
                self.host_op(EditorOp::MailSet { field: "debug".into(), value: value.into() })?;
                Ok(Flow::Next)
            }
            // `PAPLACE cmline|status` — where the `$CWD` path is shown; bare reports it. `$PAPLACE`.
            "PAPLACE" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("path shown on: {}", self.special_str("PAPLACE")));
                    return Ok(Flow::Next);
                }
                let place = crate::PathPlace::parse(&args).ok_or_else(|| {
                    XplError::trappable(format!(
                        "PAPLACE: expected cmline|status, got {:?}",
                        args.trim()
                    ))
                })?;
                self.host_op(EditorOp::SetPathPlace(place))?;
                Ok(Flow::Next)
            }
            // `PATHLEN n` — max width (chars) of the displayed `$CWD` path; bare reports it. `$PATHLEN`.
            "PATHLEN" => {
                if args.trim().is_empty() {
                    *self.ctx.status = Some(format!("path length: {}", self.special_str("PATHLEN")));
                    return Ok(Flow::Next);
                }
                let n: usize = args.trim().parse().map_err(|_| {
                    XplError::trappable(format!("PATHLEN: expected a number, got {:?}", args.trim()))
                })?;
                self.host_op(EditorOp::SetPathLen(n))?;
                Ok(Flow::Next)
            }
            // Deletion buffer + strict delete.
            "RD" => {
                self.host_op(EditorOp::RubDelete)?;
                Ok(Flow::Next)
            }
            "UD" => {
                self.host_op(EditorOp::PasteDeletion)?;
                Ok(Flow::Next)
            }
            "DN" => {
                self.host_op(EditorOp::DeleteSel)?;
                Ok(Flow::Next)
            }
            // Case of the selection (or the char under the cursor).
            "LC" | "UC" | "CC" => {
                let kind = match base {
                    "LC" => CaseKind::Lower,
                    "UC" => CaseKind::Upper,
                    _ => CaseKind::Swap,
                };
                self.host_op(EditorOp::Case(kind))?;
                Ok(Flow::Next)
            }
            // In-document copy / move the selection to the cursor.
            "CP" => {
                // With a selection in the current tab: the classic copy-selection-to-caret.
                let has_sel = matches!(self.ctx.host.special("DF"), Some(v) if v.to_str() == "1");
                if has_sel {
                    self.host_op(EditorOp::CopySelection)?;
                    return Ok(Flow::Next);
                }
                // No local selection: offer the selections in OTHER tabs in a chooser (shares the
                // PICK dialog); the chosen tab's selection is inserted at the caret on resume.
                let candidates = self.ctx.host.cross_tab_selection_candidates();
                if candidates.is_empty() {
                    *self.ctx.status = Some("CP: no selection here or in other tabs".to_string());
                    return Ok(Flow::Next);
                }
                let (sources, items): (Vec<usize>, Vec<String>) = candidates.into_iter().unzip();
                self.suspend = Some(SuspendReason::CpPick {
                    title: Some("Copy selection from tab".to_string()),
                    items,
                    sources,
                });
                Ok(Flow::Suspend)
            }
            "MV" => {
                self.host_op(EditorOp::MoveSelection)?;
                Ok(Flow::Next)
            }
            // Clipboard ($CB). CUT/COPY/PASTE = Ctrl+X/C/V; CLIP sets it; APPEND/
            // PREPEND extend it with the selection. (Bare COPY copies the selection;
            // `COPY <src> <dst>` is reserved for the future file-copy verb.)
            "CUT" => {
                self.host_op(EditorOp::Clip(ClipKind::Cut))?;
                Ok(Flow::Next)
            }
            "COPY" => {
                if args.trim().is_empty() {
                    self.host_op(EditorOp::Clip(ClipKind::Copy))?;
                } else {
                    let (src, dst) = parse_two_paths("COPY", &args)?;
                    self.host_op(EditorOp::FileCopy { src, dst })?;
                }
                Ok(Flow::Next)
            }
            "PASTE" => {
                self.host_op(EditorOp::Clip(ClipKind::Paste))?;
                Ok(Flow::Next)
            }
            "APPEND" => {
                self.host_op(EditorOp::Clip(ClipKind::Append))?;
                Ok(Flow::Next)
            }
            "PREPEND" => {
                self.host_op(EditorOp::Clip(ClipKind::Prepend))?;
                Ok(Flow::Next)
            }
            // Set the clipboard from a quoted-string argument (`CLIP "text"`, internal quotes as
            // `\"`); a bare unquoted argument is still taken verbatim. Because the argument is a
            // normal quoted string, `CLIP` chains with `;` like any other verb — a `;` inside the
            // quotes is part of the text, one after the closing quote separates commands.
            "CLIP" => {
                self.host_op(EditorOp::Clip(ClipKind::Set(literal_text(&args))))?;
                Ok(Flow::Next)
            }
            // `PR` writes to the script-output channel (the GUI surfaces it on the status line).
            "PR" => {
                if self.is_capturing() {
                    self.capture_push(&args);
                } else {
                    self.ctx.output.push(args);
                }
                Ok(Flow::Next)
            }
            // `MSG [/T:<seconds>] <text>` shows `text` in a GUI message box; `/T:` self-dismisses
            // it after that many seconds. Body is `literal_text` (quoted → `\n` etc.), like TXT.
            "MSG" => {
                let (timeout_secs, text) = parse_msg(&args)?;
                if self.is_capturing() {
                    self.capture_push(&text); // CAPTURE redirects the body; no modal, no park
                    Ok(Flow::Next)
                } else if timeout_secs.is_some() {
                    // `MSG/T:<secs>` — a non-blocking timed toast (self-dismisses; script continues).
                    self.host_op(EditorOp::ShowMessage { text, timeout_secs })?;
                    Ok(Flow::Next)
                } else {
                    // Bare `MSG` — a modal box: park the script until the user dismisses it (like TXT),
                    // so each `MSG` in a loop pauses instead of the last one overwriting the rest.
                    self.suspend = Some(SuspendReason::ShowMessage { text });
                    Ok(Flow::Suspend)
                }
            }
            "MESSAGE" => {
                // OS-native desktop message box — works headless (background `RUN/BG`) unlike `MSG`.
                // Fire-and-forget: the box is a separate process, so the script never parks on it.
                // Optional `/T:"title"` sets the window caption (parsed like TXT's title).
                let (title, rest) = parse_txt(&args);
                let text = literal_text(rest);
                if self.is_capturing() {
                    self.capture_push(&text); // CAPTURE redirects it to a var; no box
                } else {
                    self.host_op(EditorOp::OsMessage { title, text })?;
                }
                Ok(Flow::Next)
            }
            "LEARN" | "UNLEARN" => {
                // Add/remove word(s) from the user dictionary. The word list comes from one of three
                // sources: `/F <file>` (words listed in a file), `/S` (the current selection), or the
                // arguments themselves — each split on whitespace (so newline-delimited, one-per-line
                // text works), then stripped of clinging edge punctuation. Surfaces the host status.
                let is_learn = base == "LEARN";
                let from_file = vswitches.iter().any(|s| s.eq_ignore_ascii_case("F"));
                let from_sel = vswitches.iter().any(|s| s.eq_ignore_ascii_case("S"));
                let source: String = if from_file {
                    let path = args.trim();
                    if path.is_empty() {
                        return Err(XplError::trappable(format!("{base}/F: a file path is required")));
                    }
                    match self.ctx.host.read_file(path) {
                        Ok(t) => t,
                        Err(e) => {
                            return Err(XplError::trappable(format!("{base}/F: {}", e.message())));
                        }
                    }
                } else if from_sel {
                    self.ctx.host.special("SE").map(|v| v.to_str()).unwrap_or_default()
                } else {
                    args.to_string()
                };
                let words: Vec<String> = source
                    .split_whitespace()
                    .map(|w| {
                        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                    })
                    .filter(|w| !w.is_empty())
                    .map(|w| w.to_string())
                    .collect();
                if words.is_empty() {
                    let what = if from_file {
                        "no words found in the file"
                    } else if from_sel {
                        "no words in the selection"
                    } else {
                        "a word is required"
                    };
                    return Err(XplError::trappable(format!("{base}: {what}")));
                }
                // One word keeps the familiar single-word op (and its "learned: <w>" phrasing).
                let op = match (is_learn, words.len()) {
                    (true, 1) => EditorOp::Learn(words.into_iter().next().unwrap()),
                    (false, 1) => EditorOp::Unlearn(words.into_iter().next().unwrap()),
                    (true, _) => EditorOp::LearnWords(words),
                    (false, _) => EditorOp::UnlearnWords(words),
                };
                let msg = self.host_op_value(op)?.to_str();
                *self.ctx.status = Some(msg);
                Ok(Flow::Next)
            }
            "DIALECT" => {
                // Set the checking dialect (american|british|canadian|australian|indian). The host
                // validates the name; `$DIALECT` reads it back.
                let name = args.trim();
                if name.is_empty() {
                    return Err(XplError::trappable(
                        "DIALECT: a dialect is required (american|british|canadian|australian|indian)",
                    ));
                }
                self.host_op(EditorOp::SetDialect(name.to_string()))?;
                Ok(Flow::Next)
            }
            "SPELLENGINE" => {
                // Pick the spelling checker: `harper` (built-in) or `hunspell` (multilingual CLI).
                let name = args.trim();
                if name.is_empty() {
                    return Err(XplError::trappable("SPELLENGINE: harper | hunspell"));
                }
                self.host_op(EditorOp::SetSpellEngine(name.to_string()))?;
                Ok(Flow::Next)
            }
            "SPELLDICT" => {
                // Hunspell dictionaries for the `-d` flag, e.g. `SPELLDICT en_US,de_DE`.
                let names = args.trim();
                if names.is_empty() {
                    return Err(XplError::trappable("SPELLDICT: one or more dictionary names (e.g. en_US)"));
                }
                self.host_op(EditorOp::SetSpellDicts(names.to_string()))?;
                Ok(Flow::Next)
            }
            "SPELLBIN" => {
                // The hunspell executable (name on PATH or a full path).
                let path = args.trim();
                if path.is_empty() {
                    return Err(XplError::trappable("SPELLBIN: the hunspell executable (name or path)"));
                }
                self.host_op(EditorOp::SetSpellBin(path.to_string()))?;
                Ok(Flow::Next)
            }
            "SPELLNEXT" | "SPELLPREV" | "GRAMMARNEXT" | "GRAMMARPREV" => {
                // Jump the caret to the next/previous spelling or grammar issue (selecting it).
                let spelling = base.starts_with("SPELL");
                let forward = base.ends_with("NEXT");
                self.host_op(EditorOp::LintNav { spelling, forward })?;
                Ok(Flow::Next)
            }
            "LINTFIX" => {
                // Correct the language issue at the caret. 0 fixes → status; 1 → apply directly; more
                // → a modal chooser (sharing the PICK dialog) that applies the choice on resume.
                match self.ctx.host.lint_fix_options() {
                    None => {
                        *self.ctx.status =
                            Some("LINTFIX: no correction available at the caret".to_string());
                        self.ctx.env.error = 1;
                        Ok(Flow::Next)
                    }
                    Some(o) if o.labels.len() == 1 => {
                        self.ctx.host.apply_lint_fix(o.span, &o.replacements[0])?;
                        Ok(Flow::Next)
                    }
                    Some(o) => {
                        self.suspend = Some(SuspendReason::LintFixPick {
                            title: Some("Correct to".to_string()),
                            items: o.labels,
                            span: o.span,
                            replacements: o.replacements,
                        });
                        Ok(Flow::Suspend)
                    }
                }
            }
            "SPELLING" | "SP" | "GRAMMAR" | "GR" => {
                // Toggle live spelling / grammar underlines independently. `<verb> on|off` (also 1|0).
                // `$SPELLING` / `$GRAMMAR` read the states.
                let on = match args.trim().to_ascii_lowercase().as_str() {
                    "on" | "1" => true,
                    "off" | "0" | "" => false,
                    other => {
                        return Err(XplError::trappable(format!(
                            "{base}: expected on or off, got {other:?}"
                        )))
                    }
                };
                let op = if base == "SPELLING" || base == "SP" {
                    EditorOp::SetSpelling(on)
                } else {
                    EditorOp::SetGrammar(on)
                };
                self.host_op(op)?;
                Ok(Flow::Next)
            }
            "CHECK" => {
                // Run harper over the active doc via the host, which returns the report. Under CAPTURE
                // it goes to the variable; otherwise it opens in a scrollable `*Language Check*` tab
                // (the report can be long — the status line can't hold it).
                if self.is_capturing() {
                    // CAPTURE needs the report NOW → synchronous.
                    let report = self.host_op_value(EditorOp::RunLint)?.to_str();
                    self.capture_push(&report);
                } else {
                    // Interactive: run on a background thread (no UI freeze); it opens the results tab
                    // when done, with a "Working…" status meanwhile.
                    self.host_op(EditorOp::RequestFullCheck)?;
                }
                Ok(Flow::Next)
            }
            "SV" => {
                let name = args.trim();
                if name.is_empty() {
                    return Err(XplError::fatal("SV requires a variable name"));
                }
                if is_special_name(name) {
                    return Err(XplError::fatal(format!(
                        "cannot assign to read-only special ${name}"
                    )));
                }
                let sel = self.ctx.host.special("SE").map(|v| v.to_str()).unwrap_or_default();
                self.ctx.env.set(name, Value::str(sel));
                Ok(Flow::Next)
            }
            // `SETERR [n]` sets `$ERROR` to code `n` (default `1`, the generic code); the code
            // is otherwise arbitrary, so a script can raise its own numbered conditions.
            "SETERR" => {
                let a = self.subst(&args);
                let a = a.trim();
                let code: u32 = if a.is_empty() {
                    crate::error::ErrCode::Generic as u32
                } else {
                    a.parse::<u32>().map_err(|_| {
                        XplError::fatal(format!("SETERR: {a:?} is not a valid error code (0..)"))
                    })?
                };
                self.ctx.env.error = code;
                Ok(Flow::Next)
            }
            "CLERR" => {
                self.ctx.env.error = 0;
                Ok(Flow::Next)
            }
            // Suppress status messages for the rest of the script: `QUIET` hides automatic
            // op-result statuses (PR + errors still show); `QUIET/NV` also hides non-fatal error
            // text ($ERROR stays set); `QUIET OFF` re-enables. Auto-resets at script end.
            "QUIET" => {
                let level = if args.trim().eq_ignore_ascii_case("off") {
                    0
                } else if vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV")) {
                    2
                } else {
                    1
                };
                self.host_op(EditorOp::SetQuiet(level))?;
                Ok(Flow::Next)
            }
            "NOP" => Ok(Flow::Next),
            // Command-line (CMline) control — the editor reconciles these.
            "CMLINE" => {
                self.host_op(EditorOp::OpenCmdLine)?;
                Ok(Flow::Next)
            }
            "SUBMIT" => {
                self.host_op(EditorOp::SubmitCmdLine)?;
                Ok(Flow::Next)
            }
            "CANCEL" => {
                self.host_op(EditorOp::CancelCmdLine)?;
                Ok(Flow::Next)
            }
            // CMline focus & dual-cursor memory (B23). `GH`/the `cmline` verb both focus
            // the (persistent) cmline; `GT` focuses the text; `TH` toggles; `BC` blanks
            // the cmline; `XC` runs its shown contents.
            "GH" => {
                self.host_op(EditorOp::OpenCmdLine)?;
                Ok(Flow::Next)
            }
            "GT" => {
                self.host_op(EditorOp::FocusText)?;
                Ok(Flow::Next)
            }
            "TH" => {
                self.host_op(EditorOp::ToggleFocus)?;
                Ok(Flow::Next)
            }
            "BC" => {
                // Bare `BC` blanks the command line. `BC <text>` blanks it then puts the LITERAL
                // text on it WITHOUT `$`-expansion (uses `raw_args`, not the substituted `args`) —
                // typically followed by `XC`, at which point `$`-expansion happens. The `;`/`\`
                // chain escapes are already resolved in `raw_args`, so `\;` yields a literal `;`.
                if raw_args.trim().is_empty() {
                    self.host_op(EditorOp::BlankCmdLine)?;
                } else {
                    self.host_op(EditorOp::SetCmdLine(raw_args.to_string()))?;
                }
                Ok(Flow::Next)
            }
            "XC" => {
                self.host_op(EditorOp::ExecCmdLine)?;
                Ok(Flow::Next)
            }
            "QUIT" | "QQ" => {
                // `QUIT` prompts to save dirty/untitled tabs first; `QUIT/NV` quits immediately.
                let verify = !vswitches.iter().any(|s| s.eq_ignore_ascii_case("NV"));
                self.host_op(EditorOp::Quit { verify })?;
                Ok(Flow::Next)
            }
            "CALL" => self.exec_call(&args),
            "LOADLIB" => self.lib_load(&args, true),
            "ADDLIB" => self.lib_load(&args, false),
            "RELOADLIB" => self.lib_reload(),
            "UNLOADLIB" => self.lib_unload(),
            // `SaveLib [path]` — write all currently-loaded USER routines to one `.lib` file
            // (`SUB…ENDSUB` per routine), reloadable with `LoadLib`/`AddLib`. Bare path →
            // `xyscript.lib` (resolved against `$CWD`). Under `CAPTURE`, the text goes into the
            // variable instead of a file. The system library is never written.
            "SAVELIB" => {
                let text = format!(
                    "# XyScript user library — reload with LoadLib / AddLib.\n\n{}",
                    self.ctx.library.user_lib_text()
                );
                if self.is_capturing() {
                    self.capture_push(&text);
                } else {
                    let arg = args.trim();
                    let path = if arg.is_empty() { "xyscript.lib" } else { arg };
                    if let Err(e) = self.ctx.host.write_file(path, &text) {
                        match e {
                            XplError::Trappable { code, msg } => self.fail_trap(code, msg),
                            other => return Err(other),
                        }
                    }
                }
                Ok(Flow::Next)
            }
            // `HELP [name]` / `? [name]` — look up reference help for a command, `$special`, or
            // function and show it in a scrollable TXT dialog (or feed it to a CAPTURE variable).
            // A bare `HELP` shows a short overview; an unknown name reports suggestions on the
            // status line (and sets `$ERROR`) rather than opening an empty dialog.
            "HELP" | "?" | "REF" => {
                // `REF` is a synonym for `HELP` (the reference command, as `MAN` is for the manual).
                // Take the name RAW (not `$`-substituted), so `HELP $CWD` looks up the *special*
                // rather than substituting its value, and `HELP UPPER` works without a leading `$`.
                let q = raw_args.trim();
                // `HELP <term>` always searches now (exact matches promoted to the top); `/S` is kept as
                // an accepted no-op alias so old habits/scripts don't break.
                let resp = self.ctx.host.help((!q.is_empty()).then_some(q), false);
                if self.is_capturing() {
                    self.capture_push(&resp.body);
                    return Ok(Flow::Next);
                }
                if resp.not_found {
                    self.ctx.env.error = 1;
                    *self.ctx.status = Some(resp.body);
                    return Ok(Flow::Next);
                }
                // Show the results in the shared read-only Help tab (smooth scrolling), highlighting the term.
                self.host_op(EditorOp::ShowResults { text: resp.body, emphasis: resp.emphasis })?;
                Ok(Flow::Next)
            }
            // `MAN [topic]` — the manual command. Bare `MAN` opens the user manual (Help ▸ Manual);
            // `MAN <topic>` searches the manual by section and shows the matches in a TXT dialog (a
            // metacharacter makes the topic a regex, like `HELP`). Capture-aware.
            "MAN" => {
                let q = raw_args.trim();
                if q.is_empty() {
                    // Bare: open the manual (a GUI action; headless no-op).
                    self.host_op(EditorOp::ShowManual)?;
                    return Ok(Flow::Next);
                }
                let resp = self.ctx.host.manual_search(q);
                if self.is_capturing() {
                    self.capture_push(&resp.body);
                    return Ok(Flow::Next);
                }
                if resp.not_found {
                    self.ctx.env.error = 1;
                    *self.ctx.status = Some(resp.body);
                    return Ok(Flow::Next);
                }
                // Show the results in the shared read-only Help tab (smooth scrolling), highlighting the term.
                self.host_op(EditorOp::ShowResults { text: resp.body, emphasis: resp.emphasis })?;
                Ok(Flow::Next)
            }
            // `REFPDF` / `MANPDF` — open the reference / user-manual PDF in the OS default PDF app.
            // The file is looked up in `$XSDIR/docs` first, then `$XSDIR` (see `EditorOp::OpenDocPdf`).
            "REFPDF" | "MANPDF" => {
                self.host_op(EditorOp::OpenDocPdf { manual: base == "MANPDF" })?;
                Ok(Flow::Next)
            }
            "KBCHART" => {
                // Show the loaded keymap's chart (all modifier tables) in a scrollable TXT modal;
                // under CAPTURE, feed it to the captured variable instead.
                let text = self.ctx.host.keyboard_chart_text();
                if self.is_capturing() {
                    self.capture_push(&text);
                    return Ok(Flow::Next);
                }
                self.suspend = Some(SuspendReason::ShowText {
                    title: Some("Keyboard chart".to_string()),
                    text,
                    columns: None,
                    line_colors: None,
                });
                Ok(Flow::Suspend)
            }
            "LISTLIB" => {
                // Show the routine list in a scrollable TXT dialog (like HLKINDS); under CAPTURE,
                // feed it to the captured variable instead.
                let text = self.ctx.library.listing().join("\n");
                if self.is_capturing() {
                    self.capture_push(&text);
                    return Ok(Flow::Next);
                }
                self.suspend = Some(SuspendReason::ShowText {
                    title: Some("Library routines".to_string()),
                    text,
                    columns: Some(self.ctx.library.listing_columns()),
                    line_colors: None,
                });
                Ok(Flow::Suspend)
            }
            // `LISTSPECIALS` — generate one `$NAME: |$NAME|` line per special variable: a
            // PRINTEX-able template that stays in sync as specials are added. Inserts the lines at
            // the caret (so `NE` ; `LISTSPECIALS` ; `PRINTEX out` rebuilds the listing), or, under
            // CAPTURE, feeds the text to the captured variable instead.
            "LISTSPECIALS" => {
                let text: String = crate::env::SPECIAL_NAMES
                    .iter()
                    .map(|name| format!("${name}: |${name}|\n"))
                    .collect();
                if self.is_capturing() {
                    self.capture_push(&text);
                } else {
                    self.host_op(EditorOp::InsertText(text))?;
                }
                Ok(Flow::Next)
            }
            "RUN" => {
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("BG")) {
                    // `RUN/BG <script> [args…]` — spawn `$ED --run <script> [args…]` as a background
                    // child process (the X/BG job machinery): non-freezing, output streams to *Shell
                    // Output*, and `JOBS`/`KILL`/`$XO`/`$RC` all apply. The child has its own editor
                    // state; the positional args reach the script as `$1..$N` there, exactly as for a
                    // foreground `RUN`. `/R:<var>` additionally delivers the child's output into `<var>`.
                    let return_var = return_var_switch(&vswitches);
                    let (path, script_args) = self.resolve_run_target(&args);
                    self.host_op(EditorOp::RunBackgroundScript { path, args: script_args, return_var })?;
                    Ok(Flow::Next)
                } else {
                    self.exec_run(&args)
                }
            }
            // `RUNCODE <code>` — run XPL text directly (an `eval`). The args are already
            // `$`-substituted, so `RUNCODE $var` runs the code held in `$var` (single-pass, so the
            // code's OWN `$`-refs resolve when it runs). `/BG` backgrounds it (the code is piped to a
            // `$ED --run -` child via stdin); `/R:<var>` returns its output.
            "RUNCODE" => {
                if vswitches.iter().any(|s| s.eq_ignore_ascii_case("BG")) {
                    let return_var = return_var_switch(&vswitches);
                    self.host_op(EditorOp::RunBackgroundCode { code: args.clone(), return_var })?;
                    Ok(Flow::Next)
                } else {
                    self.run_source_sync(&args, &[], false)
                }
            }
            // Interactive input: pause the script and surface a request to the
            // editor/GUI. The resumed value lands in the named variable.
            "READKEY" | "RK" => {
                // `READKEY <var> [message]` — first token is the destination variable; the rest
                // (a `$`-substituted quoted/bare string) is an optional message shown in a modal
                // box while awaiting the keystroke.
                let mut it = args.trim().splitn(2, char::is_whitespace);
                let var = self.input_var(it.next().unwrap_or(""))?;
                let message =
                    it.next().map(str::trim).filter(|s| !s.is_empty()).map(literal_text);
                // `/D` — write a readable key description (`Ctrl+Y`, `Enter`, …) into `var`.
                let describe = vswitches.iter().any(|s| s.eq_ignore_ascii_case("D"));
                self.suspend = Some(SuspendReason::ReadKey { var, message, describe });
                Ok(Flow::Suspend)
            }
            "INPUT" => {
                // `INPUT <var> [prompt]` — first token is the destination variable, the
                // remainder (quoted or bare, `$`-substituted in `args`) is the prompt.
                let mut it = args.trim().splitn(2, char::is_whitespace);
                let var = self.input_var(it.next().unwrap_or(""))?;
                let prompt = it
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(literal_text);
                self.suspend = Some(SuspendReason::InputLine { var, prompt });
                Ok(Flow::Suspend)
            }
            // `PICK [/I] <var> ["title"] <list-expr>` — pause and show a list chooser. `<var>` is the
            // first token; an optional leading quoted string is the dialog title; the rest is an
            // EXPRESSION (evaluated from the raw args, so `$vars`/`[..]`/functions work) that must
            // yield an array. The chosen item (or 1-based index under `/I`) resumes into `<var>`.
            "PICK" | "CHOOSE" => {
                let index_mode = vswitches.iter().any(|s| s.eq_ignore_ascii_case("I"));
                let mut it = raw_args.trim_start().splitn(2, char::is_whitespace);
                let var = self.input_var(it.next().unwrap_or(""))?;
                let remainder = it.next().unwrap_or("").trim_start();
                let (title, expr_src) = match peel_quoted(remainder) {
                    Some((t, rest)) => (Some(t), rest),
                    None => (None, remainder),
                };
                if expr_src.trim().is_empty() {
                    return Err(XplError::fatal("PICK requires a list expression"));
                }
                // Evaluate the list expression in the same context as `$(…)` substitution.
                let eval_result = {
                    let frame = self.frames.last().expect("a frame is always present");
                    let ctx = Ctx {
                        env: self.ctx.env,
                        host: &*self.ctx.host,
                        args: &frame.args,
                        rv: &self.rv,
                    };
                    crate::expr::parse(expr_src).and_then(|e| crate::expr::eval(&e, &ctx))
                };
                let items: Vec<String> = match eval_result {
                    Ok(Value::Array(a)) => a.iter().map(|v| v.to_str()).collect(),
                    Ok(_) => {
                        self.ctx.env.error = 1;
                        *self.ctx.status = Some("PICK: not a list".into());
                        return Ok(Flow::Next);
                    }
                    Err(e) => {
                        self.ctx.env.error = 1;
                        *self.ctx.status = Some(format!("PICK: {}", e.msg));
                        return Ok(Flow::Next);
                    }
                };
                if items.is_empty() {
                    self.ctx.env.error = 1;
                    *self.ctx.status = Some("PICK: empty list".into());
                    return Ok(Flow::Next);
                }
                const PICK_CAP: usize = 5000;
                if items.len() > PICK_CAP {
                    self.ctx.env.error = 1;
                    *self.ctx.status = Some(format!("PICK: too many items (> {PICK_CAP})"));
                    return Ok(Flow::Next);
                }
                self.suspend = Some(SuspendReason::Pick { var, title, items, index_mode });
                Ok(Flow::Suspend)
            }
            // `REOPEN` — choose a recently-closed file to reopen (caret restored). Shares the PICK
            // dialog; the chosen entry's file is reopened on resume.
            // `REOPEN` — pop up the recently-closed picker. `REOPEN <n>` skips the picker and reopens
            // the first `n` closed files straight away (`REOPEN 1` replaces the old `REOPEN1`).
            "REOPEN" => {
                let arg = args.trim();
                if !arg.is_empty() {
                    let n: usize = arg.parse().ok().filter(|&n| n >= 1).ok_or_else(|| {
                        XplError::trappable(format!("REOPEN: expected a count >= 1, got {arg:?}"))
                    })?;
                    self.host_op(EditorOp::ReopenRecent(n))?;
                    return Ok(Flow::Next);
                }
                let candidates = self.ctx.host.reopen_candidates();
                if candidates.is_empty() {
                    *self.ctx.status = Some("REOPEN: no recently-closed files".to_string());
                    return Ok(Flow::Next);
                }
                let (entries, items): (Vec<(String, usize)>, Vec<String>) =
                    candidates.into_iter().map(|(path, caret, label)| ((path, caret), label)).unzip();
                self.suspend = Some(SuspendReason::ReopenPick {
                    title: Some("Reopen a recently-closed file".to_string()),
                    items,
                    entries,
                });
                Ok(Flow::Suspend)
            }
            "CLEARCLOSED" => {
                self.host_op(EditorOp::ClearClosed)?;
                Ok(Flow::Next)
            }
            "EXIT" | "EX" | "EXITALL" | "EX1" => Ok(Flow::Halt),
            // A loaded library routine is invokable directly, the `CALL` verb optional:
            // `puts $x 3` ≡ `CALL puts $x 3`. Routine names that shadow a built-in are rejected at
            // load, so only genuine routines reach this fallback — no ambiguity.
            other if self.subs.contains_key(other) => {
                self.exec_call(&format!("{other} {args}"))
            }
            // A routine that arrived AFTER this program was compiled — `LOADLIB` inside the very
            // script that then calls the routine (the classic `startup.xpl` shape). Library routines
            // are lowered INTO the program up front (see `ir::lower`'s `extra_subs`), so `self.subs`
            // is a snapshot from compile time and cannot see it; the call died as "unknown command",
            // which is FATAL and killed the rest of the script. Run it as a nested script instead:
            // that compiles fresh against the CURRENT library, exactly as `RUNCODE greet` already
            // did. Args go through `$1..$N` rather than being pasted back into source, so a value
            // that happens to contain a `$` isn't substituted a second time.
            other if self.ctx.library.get(other).is_some() => {
                let call_args: Vec<Value> =
                    args.split_whitespace().map(Value::str).collect();
                let refs: String = (1..=call_args.len())
                    .map(|i| format!("${i}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.run_source_sync(&format!("CALL {other} {refs}"), &call_args, true)
            }
            other => Err(XplError::fatal(unknown_command_message(other, &self.subs))),
        }
    }

    /// `CALL name [arg…]` — set up a frame and jump to the hoisted sub body.
    fn exec_call(&mut self, args: &str) -> VmResult<Flow> {
        let mut it = args.split_whitespace();
        let name = match it.next() {
            Some(n) => fold_ident(n),
            None => return Err(XplError::fatal("CALL requires a sub name")),
        };
        let entry = match self.subs.get(&name) {
            Some(&e) => e,
            None => return Err(XplError::fatal(format!("CALL unknown sub {name:?}"))),
        };
        let call_args: Vec<Value> = it.map(Value::str).collect();
        self.frames.push(Frame {
            return_ip: self.ip + 1,
            args: call_args,
            locals: Vec::new(),
        });
        self.rv = Value::str(""); // $RV resets at call entry
        self.ip = entry;
        Ok(Flow::Jumped)
    }

    /// Validate and fold the destination variable for `READKEY`/`INPUT`: the first
    /// whitespace-delimited token of `args`, which must be present and not a special.
    fn input_var(&self, args: &str) -> VmResult<String> {
        let var = fold_ident(args.split_whitespace().next().unwrap_or(""));
        if var.is_empty() {
            return Err(XplError::fatal("READKEY/INPUT requires a variable name"));
        }
        if is_special_name(&var) {
            return Err(XplError::fatal(format!(
                "cannot read input into read-only special ${var}"
            )));
        }
        Ok(var)
    }

    /// `RUN <path>` — read, compile (with the current library), and execute a
    /// script file, sharing this run's env/library/host.
    fn exec_run(&mut self, args: &str) -> VmResult<Flow> {
        let (path, arg_toks) = self.resolve_run_target(args);
        if path.is_empty() {
            return Err(XplError::trappable("RUN: a script path is required"));
        }
        let arg_vals: Vec<Value> = arg_toks.into_iter().map(Value::str).collect();
        let src = match self.ctx.host.read_file(&path) {
            Ok(s) => s,
            Err(XplError::Trappable { code, .. }) => {
                self.ctx.env.error = code;
                return Ok(Flow::Next);
            }
            Err(fatal) => return Err(fatal),
        };
        // Name the RUN-ed file as the error-log `location` while it runs; restore the outer file
        // after (so a later error in the parent script isn't attributed to the child).
        let saved_file = self.ctx.cur_file.replace(run_file_label(&path).to_string());
        // Run the file; on failure, stamp the error with `<file>:<line>` so a bad line in a RUN-ed
        // script (e.g. from `startup.xpl`) points AT it, instead of a bare message. `err_line` holds
        // the line the nested run failed on; `err_located` guards against an OUTER RUN re-stamping.
        let result = self.run_source_sync(&src, &arg_vals, false);
        self.ctx.cur_file = saved_file;
        result.map_err(|e| {
            if self.ctx.err_located {
                return e;
            }
            self.ctx.err_located = true;
            let loc = format!("{}:{}: ", run_file_label(&path), self.ctx.err_line);
            match e {
                XplError::Fatal(m) => XplError::fatal(format!("{loc}{m}")),
                XplError::Trappable { code, msg } => XplError::Trappable { code, msg: format!("{loc}{msg}") },
            }
        })
    }

    /// Resolve a `RUN`/`RUN/BG` operand into `(script_path, positional_args)`. The path is a
    /// `"quoted"` string (spaces allowed) or the first whitespace-delimited word; whatever follows
    /// is whitespace-split into positional args (`$1..$N`), mirroring a routine `CALL`.
    ///
    /// Backward compatibility: an UNQUOTED path may itself contain spaces (a common Windows path).
    /// If the split would produce args but the first token isn't a real file while the WHOLE operand
    /// is, it was a spaced path with no args — keep the pre-args behavior (whole string = path).
    /// Quote the path to force the split when a real spaced path should still take args.
    fn resolve_run_target(&self, args: &str) -> (String, Vec<String>) {
        let (path, rest) = split_run_target(args);
        let arg_toks: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        if !arg_toks.is_empty() && peel_quoted(args.trim_start()).is_none() {
            let whole = args.trim();
            if !self.ctx.host.path_exists(&path) && self.ctx.host.path_exists(whole) {
                return (whole.to_string(), Vec::new());
            }
        }
        (path, arg_toks)
    }

    /// Compile and synchronously run `src` against this run's env/library/host — shared by `RUN`
    /// (after reading the file) and `RUNCODE` (which takes the XPL text directly, an `eval`).
    ///
    /// If the sub-script PAUSES (`READKEY`/`INPUT`/`WAIT`/`TXT`), its continuation is parked onto
    /// `ctx.pending` and THIS (outer) frame suspends too (`Flow::Suspend` with `NestedRun`), so the
    /// interpreter's resume cascade finishes the sub-script and then continues here after the verb.
    ///
    /// `propagate_rv` copies the nested script's final `RETURN` value into THIS run's `$RV`. It's set
    /// only for a library routine invoked as a command that arrived after compile (the nested-run
    /// path below), so `foo 3` returns via `$RV` exactly like a hoisted `CALL foo 3` would; `RUN`/
    /// `RUNCODE`/X-chaining leave `$RV` alone (a script's last `RETURN` isn't the caller's result).
    fn run_source_sync(&mut self, src: &str, args: &[Value], propagate_rv: bool) -> VmResult<Flow> {
        let extra = self.ctx.library.effective();
        let program = ir::compile_with_library(src, &extra).map_err(XplError::fatal)?;
        drop(extra);
        // Inherit the caller's preemption budget so a nested RUN/RUNCODE script slices too —
        // otherwise a runaway loop inside a RUN-ed file would run unbudgeted and freeze the GUI.
        // A nested Yield propagates out as a parked Yield frame (below the outer NestedRun frame);
        // the editor re-budgets each slice via `resume_yield`. `None` (an unbudgeted `run`) passes
        // through unchanged.
        match execute(&program, &mut *self.ctx, args, self.budget)? {
            // The sub-script ran out (EXIT/end). If it was `EXITALL`, abort this frame too (the flag
            // keeps propagating up); otherwise continue after the verb.
            ExecOutcome::Done { rv } => {
                if propagate_rv {
                    self.rv = rv; // routine-as-command: its RETURN is the caller's $RV
                }
                if *self.ctx.exit_all {
                    Ok(Flow::Halt)
                } else {
                    Ok(Flow::Next)
                }
            }
            ExecOutcome::Suspended { ip, frames, rv, reason } => {
                self.ctx.pending.push(SuspendedScript { program, ip, frames, rv, reason });
                self.suspend = Some(SuspendReason::NestedRun);
                Ok(Flow::Suspend)
            }
        }
    }

    /// `LOADLIB`/`ADDLIB <path>` — read, parse, shadow-check, and install.
    fn lib_load(&mut self, args: &str, replace: bool) -> VmResult<Flow> {
        let path = args.trim().to_string();
        if path.is_empty() {
            self.ctx.env.error = 1;
            self.ctx.output.push("LOADLIB: a path is required".to_string());
            return Ok(Flow::Next);
        }
        let routines = match self.read_and_parse_lib(&path) {
            Ok(rs) => rs,
            Err(()) => return Ok(Flow::Next), // $ERROR already set, message pushed
        };
        let (accepted, rejected) = validate_names(routines);
        for msg in &rejected {
            self.ctx.output.push(msg.clone());
        }
        if accepted.is_empty() {
            self.ctx.env.error = 1;
            self.ctx.output.push(format!("{path}: no usable routines (all shadowed)"));
            return Ok(Flow::Next);
        }
        let n = accepted.len();
        let verb = if replace { "loaded" } else { "added" };
        let msg = format!("{verb} {n} routine{} from {path}", if n == 1 { "" } else { "s" });
        if replace {
            self.ctx.library.set_user(accepted);
            self.ctx.lib_loads.clear();
            self.ctx.lib_loads.push(path);
        } else {
            self.ctx.library.merge_user(accepted);
            self.ctx.lib_loads.push(path);
        }
        // Confirm on success (in the same output channel as this verb's error messages). Rejected-
        // name notes, if any, were already pushed above and precede this.
        self.ctx.output.push(msg);
        Ok(Flow::Next)
    }

    /// `RELOADLIB` — replay the recorded load sequence in order. On any error,
    /// the existing library is left unchanged (a syntax slip mid-iteration
    /// doesn't strand you).
    fn lib_reload(&mut self) -> VmResult<Flow> {
        let loads = self.ctx.lib_loads.clone();
        if loads.is_empty() {
            self.ctx.env.error = 1;
            self.ctx.output.push("RELOADLIB: nothing loaded; use LOADLIB".to_string());
            return Ok(Flow::Next);
        }
        let mut merged: BTreeMap<String, Routine> = BTreeMap::new();
        for path in &loads {
            let routines = match self.read_and_parse_lib(path) {
                Ok(rs) => rs,
                Err(()) => return Ok(Flow::Next), // preserve previous library
            };
            let (accepted, _rejected) = validate_names(routines);
            for r in accepted {
                merged.insert(r.name.clone(), r);
            }
        }
        self.ctx.library.set_user(merged.into_values().collect());
        Ok(Flow::Next)
    }

    /// `UNLOADLIB` — drop all user routines and the load sequence.
    fn lib_unload(&mut self) -> VmResult<Flow> {
        self.ctx.library.unload_user();
        self.ctx.lib_loads.clear();
        Ok(Flow::Next)
    }

    /// Read + parse a `.lib` file, folding failures into `$ERROR` + an output
    /// message. `Err(())` means "handled; abort the command".
    fn read_and_parse_lib(&mut self, path: &str) -> Result<Vec<Routine>, ()> {
        let text = match self.ctx.host.read_file(path) {
            Ok(t) => t,
            Err(e) => {
                self.ctx.env.error = 1;
                self.ctx.output.push(format!("{path}: {}", e.message()));
                return Err(());
            }
        };
        match parse_lib(&text, LibSource::User) {
            Ok(rs) => Ok(rs),
            Err(errs) => {
                self.ctx.env.error = 1;
                for e in errs {
                    self.ctx.output.push(format!("{path}: {e}"));
                }
                Err(())
            }
        }
    }

    /// Run a host call, folding a trappable error into `$ERROR` and propagating a
    /// fatal one.
    fn host_call<F>(&mut self, f: F) -> VmResult<()>
    where
        F: FnOnce(&mut dyn Host) -> VmResult<()>,
    {
        match f(self.ctx.host) {
            Ok(()) => Ok(()),
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                Ok(())
            }
            Err(fatal) => Err(fatal),
        }
    }

    /// Run an editor op. A non-empty returned value becomes the status line (op
    /// feedback like "opened X"); a trappable error sets `$ERROR` and the status;
    /// a fatal propagates.
    fn host_op(&mut self, op: EditorOp) -> VmResult<()> {
        match self.ctx.host.editor_op(op) {
            Ok(v) => {
                let s = v.to_str();
                if !s.is_empty() {
                    *self.ctx.status = Some(s);
                }
                Ok(())
            }
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                Ok(())
            }
            Err(fatal) => Err(fatal),
        }
    }

    /// Whether output is being redirected by an active `CAPTURE`.
    fn is_capturing(&self) -> bool {
        !self.ctx.env.capture.is_empty()
    }

    /// Append a chunk of redirected output to the innermost `CAPTURE` buffer, joining
    /// successive chunks with a newline (one logical line per emitted chunk).
    fn capture_push(&mut self, text: &str) {
        if let Some((_, buf)) = self.ctx.env.capture.last_mut() {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }

    /// Run an editor op and RETURN its value (vs. `host_op`, which routes the value to the
    /// status line). A trappable error folds into `$ERROR` + status and yields an empty value;
    /// a fatal propagates. Used where the value IS the output (e.g. `DIR` under `CAPTURE`).
    fn host_op_value(&mut self, op: EditorOp) -> VmResult<Value> {
        match self.ctx.host.editor_op(op) {
            Ok(v) => Ok(v),
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                Ok(Value::str(""))
            }
            Err(fatal) => Err(fatal),
        }
    }

    /// A host special's value as a string (empty if the host doesn't serve it).
    fn special_str(&self, name: &str) -> String {
        self.ctx.host.special(name).map(|v| v.to_str()).unwrap_or_default()
    }

    /// Record a trappable failure: set `$ERROR` and the status line (the soft-fail path
    /// shared by `host_op`/`host_call` and the shell verbs).
    fn fail_soft(&mut self, msg: String) {
        self.fail_trap(crate::error::ErrCode::Generic as u32, msg);
    }

    /// Fold a trappable error into `$ERROR` (using its own `code`) + the status line, and log it.
    fn fail_trap(&mut self, code: u32, msg: String) {
        self.ctx.env.error = code;
        self.log_error_entry(false, code, &msg);
        *self.ctx.status = Some(msg);
    }

    /// Write one error to the error log if `ERRORLOG` is on. Assembles the `command` (the current
    /// verb + args) and `location` (`file:line`, empty for an ad-hoc command line) from the run
    /// context; the host adds timestamp + document and does the (best-effort) file append.
    fn log_error_entry(&mut self, fatal: bool, code: u32, message: &str) {
        if !self.ctx.host.error_log_enabled() {
            return;
        }
        let location = match &self.ctx.cur_file {
            Some(f) => format!("{f}:{}", self.ctx.err_line),
            None => String::new(),
        };
        let command = self.ctx.cur_command.clone();
        self.ctx.host.log_error(fatal, code, &command, &location, message);
    }

    /// Run a shell command (`X`/`XY`/`XV`), folding a trappable error (e.g. shell-out
    /// disabled, or a launch failure) into `$ERROR`+status and returning `None`; a fatal
    /// propagates. `Some((stdout, exit_code))` on success.
    fn shell_run(&mut self, cmd: &str) -> VmResult<Option<(String, i64)>> {
        match self.ctx.host.shell(cmd) {
            Ok(r) => Ok(Some(r)),
            Err(XplError::Trappable { code, msg }) => {
                self.fail_trap(code, msg);
                Ok(None)
            }
            Err(fatal) => Err(fatal),
        }
    }
}

/// Interpret a verb's text argument, unwrapping a single surrounding double-
/// quoted string literal (with escape processing) so `insert "q"` → `q` and
/// `insert " "` → a space. Unquoted text is returned verbatim. **Provisional:**
/// the full editor typing model (terse self-inserting cells) is a Stage 2
/// concern; this keeps Stage-1 KBD authoring usable.
/// Parse an optional leading `/T:"title"` (or `-T:…`) switch off a `TXT` argument,
/// returning `(title, rest)`. The title may be quoted (kept verbatim, spaces and all) or a
/// bare whitespace-delimited word; `rest` is the body (leading whitespace trimmed). With no
/// `/T:` prefix the whole input is the body.
/// Parse `MSG [/T:<seconds>] <text>`: an optional leading `/T:`/`-t:` timeout in whole
/// seconds, then the message body (`literal_text` — quoted strings get `\n`/etc., bare is
/// verbatim, like INSERT/TXT). A non-numeric `/T:` value is a trappable error.
fn parse_msg(args: &str) -> VmResult<(Option<u64>, String)> {
    let s = args.trim_start();
    let after = match s.get(..3).map(str::to_ascii_lowercase).as_deref() {
        Some("/t:") | Some("-t:") => &s[3..],
        _ => return Ok((None, literal_text(s))),
    };
    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let num = &after[..end];
    let secs: u64 = num
        .parse()
        .map_err(|_| XplError::trappable(format!("MSG: /T: expects seconds, got {num:?}")))?;
    Ok((Some(secs), literal_text(after[end..].trim_start())))
}

fn parse_txt(args: &str) -> (Option<String>, &str) {
    let s = args.trim_start();
    let after = match s.get(..3).map(str::to_ascii_lowercase).as_deref() {
        Some("/t:") | Some("-t:") => &s[3..],
        _ => return (None, s),
    };
    let (title, rest) = match after.strip_prefix('"') {
        // Quoted: up to the closing quote (unterminated → the rest of the line).
        Some(q) => match q.find('"') {
            Some(end) => (&q[..end], &q[end + 1..]),
            None => (q, ""),
        },
        // Bare: up to the next whitespace.
        None => {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (&after[..end], &after[end..])
        }
    };
    (Some(title.to_string()), rest.trim_start())
}

/// `@X`/`&X` (X = A-Z/0-9) → `(is_script, slot)` in command position. `base` is already
/// upper-cased by the parser, so the slot is upper-cased. Returns `None` for anything else.
fn binder_ref(base: &str) -> Option<(bool, char)> {
    let bytes = base.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let script = match bytes[0] {
        b'@' => false,
        b'&' => true,
        _ => return None,
    };
    let slot = bytes[1] as char;
    slot.is_ascii_alphanumeric().then_some((script, slot))
}

/// Build the "unknown command" error, appending up to three nearest known verb/routine names as a
/// "did you mean" — so a typo on the CMline points the way instead of just failing. Candidates are
/// the built-in verbs plus the loaded routines; only close matches (edit distance ≤ 2) are offered.
fn unknown_command_message<V>(verb: &str, subs: &std::collections::HashMap<String, V>) -> String {
    let mut candidates: Vec<&str> = crate::library::BUILTIN_VERBS.to_vec();
    candidates.extend(subs.keys().map(String::as_str));
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .map(|c| (levenshtein(verb, c), c))
        .filter(|&(d, c)| d > 0 && d <= 2 && d < c.len().max(verb.len()))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.dedup_by_key(|s| s.1);
    let near: Vec<&str> = scored.into_iter().take(3).map(|(_, c)| c).collect();
    if near.is_empty() {
        format!("unknown command {verb:?}")
    } else {
        format!("unknown command {verb:?} — did you mean {}?", near.join(", "))
    }
}

/// Classic Levenshtein edit distance (two-row DP), for the "did you mean" suggestions.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Whether a `PRINTEX` `|…|` field is *live code* (vs a plain substitution): it has a top-level `;`
/// (a statement sequence) or its first word is a statement keyword. Live fields run XPL statements
/// (trust-gated); everything else (incl. `$var`/`$(expr)`) is pure substitution and needs no trust.
/// Note: misclassifying code as substitution is safe — `subst` can't run statements, it just
/// produces literal text — so this is a correctness/UX heuristic, not the security boundary.
fn fence_is_live(inner: &str) -> bool {
    if crate::splitter::split_commands(inner).len() > 1 {
        return true; // top-level `;` → a statement sequence
    }
    let first = inner.trim().split([' ', '\t']).next().unwrap_or("").to_ascii_uppercase();
    matches!(
        first.as_str(),
        "SET" | "IF" | "IFNOT" | "WHILE" | "FOR" | "SWITCH" | "SUB" | "CALL" | "GOTO" | "RETURN"
            | "LOCAL" | "CLEAR" | "BREAK" | "CONTINUE" | "EXIT" | "EX" | "EXITALL" | "EX1" | "CAPTURE"
            // Shell pass-throughs are side-effecting statements (they run a command), not text to
            // print — so a `|X …|` field is live code, trust-gated like the rest.
            | "X" | "XY" | "XV"
    )
}

/// If `s` (already left-trimmed by the caller is fine) starts with a quoted string, return its
/// unescaped contents plus the remaining text after the closing quote; otherwise `None`. Used by
/// `PICK` to peel an optional leading `"title"` off the operand before the list expression.
fn peel_quoted(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let b = s.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut i = 1;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => i += 2, // skip an escaped char (e.g. \")
            b'"' => return Some((literal_text(&s[..=i]), s[i + 1..].trim_start())),
            _ => i += 1,
        }
    }
    None // unterminated quote — treat the whole operand as the expression
}

/// Split a `WRITEFILE`-style argument into `(path, content)`: the path is the first token — a
/// `"quoted"` string (spaces allowed, quotes/escapes processed) or a whitespace-delimited word — and
/// the content is what follows (leading separator whitespace dropped, then taken verbatim). An empty
/// path is a trappable error.
fn split_path_and_rest(args: &str) -> VmResult<(String, String)> {
    let a = args.trim_start();
    let (path, rest) = match peel_quoted(a) {
        Some((p, rest)) => (p, rest),
        None => match a.find(char::is_whitespace) {
            Some(i) => (a[..i].to_string(), a[i..].trim_start()),
            None => (a.to_string(), ""),
        },
    };
    if path.is_empty() {
        return Err(crate::error::XplError::trappable("WRITEFILE: expected a path"));
    }
    Ok((path, rest.to_string()))
}

/// Split a `RUN`-style operand into `(path, rest)`: the path is a leading `"quoted"` string
/// (spaces allowed, escapes processed) or the first whitespace-delimited word; `rest` is whatever
/// follows (leading whitespace trimmed), to be split into positional args by the caller.
fn split_run_target(args: &str) -> (String, String) {
    let a = args.trim_start();
    match peel_quoted(a) {
        Some((p, rest)) => (p, rest.to_string()),
        None => match a.find(char::is_whitespace) {
            Some(i) => (a[..i].to_string(), a[i..].trim_start().to_string()),
            None => (a.to_string(), String::new()),
        },
    }
}

/// The trailing filename of a RUN path, for the `<file>:<line>` error label (`user-variables.xpl`
/// out of an absolute path). Falls back to the whole path if it has no trailing component.
fn run_file_label(path: &str) -> &str {
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path)
}

/// Stamp a TOP-LEVEL run's fatal error with `<file>:<line>: …` when a source `label` is known (a
/// startup script or a `--run` file) and a nested `RUN` hasn't already located it. `None` label (an
/// ad-hoc command line) or an already-located error passes through unchanged.
fn locate_top_level(e: XplError, label: Option<&str>, line: usize, located: bool) -> XplError {
    match label {
        Some(l) if !located => {
            let loc = format!("{}:{}: ", run_file_label(l), line);
            match e {
                XplError::Fatal(m) => XplError::fatal(format!("{loc}{m}")),
                XplError::Trappable { code, msg } => XplError::Trappable { code, msg: format!("{loc}{msg}") },
            }
        }
        _ => e,
    }
}

fn literal_text(args: &str) -> String {
    let t = args.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        if let Ok(toks) = crate::lexer::tokenize(t) {
            if toks.len() == 1 {
                if let crate::lexer::Tok::Str(s) = &toks[0].tok {
                    return s.clone();
                }
            }
        }
    }
    args.to_string()
}

/// Strip ONE surrounding pair of double-quotes from a command's raw path argument, if present,
/// WITHOUT running the string-escape lexer — so `CD "C:\Users\me"` keeps its literal backslashes
/// (a quoted *string literal* would escape-process `\U`, `\r`, etc.; a raw command path must not).
/// A bare, unquoted path is returned unchanged, spaces and all. This lets users quote a path that
/// contains spaces without the quotes leaking into the path itself.
fn unquote_path(args: &str) -> &str {
    let t = args.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

/// Split `s` on **unescaped** occurrences of `delim`. `\<delim>` always embeds a
/// literal delimiter. In literal mode the usual `\n`/`\t`/`\r` and `\<any>`→`<any>`
/// escapes apply; in `regex` mode every other backslash sequence is passed through
/// verbatim (so `\d`, `\s`, `\\` reach the regex engine intact).
fn scan_delimited(s: &str, delim: char, regex: bool) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n == delim => cur.push(delim), // escaped delimiter
                Some(n) if regex => {
                    cur.push('\\'); // keep the escape for the regex engine
                    cur.push(n);
                }
                Some('n') => cur.push('\n'),
                Some('t') => cur.push('\t'),
                Some('r') => cur.push('\r'),
                Some(other) => cur.push(other),
                None => cur.push('\\'),
            }
        } else if c == delim {
            segs.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    segs.push(cur);
    segs
}

/// The recognized order-free search flag letters (case-insensitive): `T` from-top,
/// `W` whole-word, `A` case-absolute, `B` backward, `R` regex (reserved).
const FLAG_LETTERS: &str = "TWABRSXPtwabrsxp";

/// Whether `base` is a search/replace verb.
/// Normalize every line ending in `s` (`\r\n` or bare `\n`) to the OS default — CRLF on Windows, LF
/// elsewhere. Used by file-output commands (`PRINTEX`) so the written file gets
/// OS-appropriate newlines no matter what the source document or expansion produced.
fn to_os_newlines(s: &str) -> String {
    let lf = s.replace("\r\n", "\n");
    if cfg!(windows) {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

fn is_search_base(base: &str) -> bool {
    matches!(
        base,
        "SE" | "SEB" | "SEA" | "SEBA" | "CH" | "CHA" | "CI" | "CIA" | "CV" | "CVA"
    )
}

/// Parse a colour command's three values into an RGB triple. `FGCOLOR`/`BGCOLOR` take
/// `R G B` (0-255); with `/HSL` the triple is `H S L` on the same 0-255 scale, converted
/// here to RGB. Wrong arity or out-of-range values are trappable usage errors.
fn parse_color(base: &str, args: &str, hsl: bool) -> VmResult<(u8, u8, u8)> {
    let nums: Vec<&str> = args.split_whitespace().collect();
    if nums.len() != 3 {
        return Err(XplError::trappable(format!(
            "{base}: expected three numbers ({}), got {}",
            if hsl { "H S L" } else { "R G B" },
            nums.len()
        )));
    }
    let mut v = [0u8; 3];
    for (i, tok) in nums.iter().enumerate() {
        v[i] = tok.parse::<u8>().map_err(|_| {
            XplError::trappable(format!("{base}: {tok:?} is not an integer 0-255"))
        })?;
    }
    Ok(if hsl { hsl_to_rgb(v[0], v[1], v[2]) } else { (v[0], v[1], v[2]) })
}

/// Convert an H S L triple (each 0-255) to an RGB triple (each 0-255). Hue wraps the
/// 0-255 range onto the colour wheel; saturation and lightness are 0-255 fractions.
fn hsl_to_rgb(h: u8, s: u8, l: u8) -> (u8, u8, u8) {
    let h = h as f32 / 255.0; // 0..1 around the wheel
    let s = s as f32 / 255.0;
    let l = l as f32 / 255.0;
    if s == 0.0 {
        let g = (l * 255.0).round() as u8;
        return (g, g, g);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let r = (hue(h + 1.0 / 3.0) * 255.0).round() as u8;
    let g = (hue(h) * 255.0).round() as u8;
    let b = (hue(h - 1.0 / 3.0) * 255.0).round() as u8;
    (r, g, b)
}

/// Reject switches a command does not declare. Commands that *do* take switches
/// (currently only `QUIET`, plus the search verbs handled separately) list them
/// here; anything else with an attached switch is a fatal usage error.
/// Editing/navigation verbs that operate on the DOCUMENT and so should pull focus out of the
/// CMline first (via `EditorOp::EnsureDocumentFocus`) when fired while the cmline is focused.
/// Covers the three document-scope op groups — with the MOTION group limited to "big jumps":
/// within-line moves (char Left/Right `CL/CR/NC/PC`, `HOME/END/EL/ER`, visual-line `LB/LE`,
/// same-line `LL/LR`, line up/down `UP/DOWN/LU/LD`) are intentionally EXCLUDED so the caret can
/// still be moved while editing a command in the cmline.
const FOCUS_DOC_VERBS: &[&str] = &[
    // Motions — big jumps only: word / sentence / paragraph / line-to-line / page / screen / file,
    // plus the selection-edge jumps `DB`/`DE` (they behave like `JMP $DS`/`JMP $DN`).
    "NW", "PW", "NS", "PS", "NP", "PP", "NL", "PL", "PD", "PU", "HM", "BS", "TF", "BF", "TOP",
    "BOTTOM", "JMP", "DB", "DE",
    // Define group (full): toggle / modify / close / all / cancel + word·line·sentence·paragraph.
    "DF", "DM", "DZ", "DFA", "XD", "YD", "DW", "DL", "DS", "DP", "DW+", "DS+", "DP+",
    // Delete group (full R-family): selection / word / line / sentence / paragraph / to-EOL.
    "DELSEL", "RW", "RL", "RLL", "RS", "RP", "RE",
    // Reflow edits the document at the caret/selection — land the caret in the text.
    "REFLOW",
    // Bookmark group: set/clear/toggle act on the doc at the caret; next/prev navigate; list chooses.
    "MARKSET", "MARKCLEAR", "MARKCLEARALL", "MARKTOGGLE", "MARKNEXT", "MARKPREV", "MARKLIST",
    // Script launchers: a script run from the CMline should act on the DOCUMENT (and so its
    // document-focusing commands leave the caret in the text, like a command typed on the CMline).
    "RUN", "RUNCODE", "RUNHERE",
    // `SH`/`SHELL` open the Console tab — land the caret at its prompt, not back on the CMline.
    "SH", "SHELL",
    // Cursor addition acts at the caret (read a number / insert the total) — keep focus in the text.
    "ADDSUM", "PUTSUM",
    // A second view opens a document tab — land focus in the text, like OPEN.
    "DUPVIEW",
    // `WV` (Writer's View) hides the chrome — INCLUDING the command line — so focus MUST land in the
    // text (there is nowhere else to type). `EnsureDocumentFocus` is document-focusing, so a
    // cmline-submitted `WV` also rests in the text rather than snapping back to the now-hidden cmline.
    "WV",
];

/// Extract the destination variable from a `/R:<var>` switch (background-job return pipe), if
/// present and non-empty. Used by `X/BG` and `RUN/BG`.
fn return_var_switch(vswitches: &[String]) -> Option<String> {
    vswitches.iter().find_map(|s| {
        let (k, v) = s.split_once(':')?;
        (k.eq_ignore_ascii_case("R") && !v.is_empty()).then(|| v.to_string())
    })
}

/// Split the first argument off `s`, honouring one pair of surrounding double quotes so a path with
/// spaces stays whole: `"my file.txt" rest` → (`my file.txt`, `rest`); `body.txt rest` → (`body.txt`,
/// `rest`). Used by `SENDMAIL/F`.
#[cfg(feature = "email")]
fn take_first_arg(s: &str) -> (String, &str) {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => (rest[..end].to_string(), rest[end + 1..].trim_start()),
            None => (rest.to_string(), ""), // unterminated quote: take the remainder
        };
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (s[..end].to_string(), s[end..].trim_start())
}

fn validate_switches(base: &str, switches: &[String]) -> VmResult<()> {
    if switches.is_empty() {
        return Ok(());
    }
    let valid: &[&str] = match base {
        "HELP" | "?" | "REF" => &["S"], // `/S` — accepted no-op alias (HELP/REF always search now)
        "QUIET" => &["NV"],
        "SAVE" | "SA" => &["NV"], // `/NV` — overwrite an existing copy target without confirming
        "ST" => &["NV"], // `/NV` — `ST/NV <path>` overwrites without confirming
        "SAAS" | "SAVEAS" => &["NV"], // `/NV` — `SAAS/NV <path>` overwrites without confirming
        "SAD" => &["NV"], // `/NV` — `SAD/NV <path>` overwrites without confirming
        "DEL" => &["NV"], // `/NV` — delete without the confirmation prompt
        "EMPH" => &["R"], // `/R` — treat the phrase as a regular expression (vs whole-word literal)
        "HLRULE" => &["P"], // `/P` — compile the rule with the Perl-compatible (fancy-regex) engine
        "CARETALL" => &["R", "W", "A", "S"], // FIND-family flags: regex / whole-word / case-absolute / in-selection
        "CARETNEXT" => &["R", "W", "A"], // regex / whole-word / case-absolute (searches the whole doc, wraps)
        "CARETSKIP" => &["R", "W", "A"], // same match flags as CARETNEXT
        // `/NV` — open again in a new tab without the "already open?" prompt.
        // `/NV` reopen without the prompt; `/H` `/V` open into a new split pane (Stacked/SideBySide).
        "OPEN" | "CA" | "ED" | "EDIT" | "READ" => &["NV", "H", "V", "HEX"],
        "DUPVIEW" => &["W"], // `/W` — a second view in a free-standing window
        "SW" | "SHELLWIN" => &["H", "V"], // `/H` `/V` — show *Shell Output* in a split pane

        "CD" => &["D"], // `/D` (change-drive) — accepted, harmless (cwd is in-process)
        "ENCODING" => &["C"], // `/C` — CONVERT the bytes to the charset (vs the default display switch)
        "X" => &["NV", "BG", "R"], // `/NV` fire-and-forget; `/BG` background job; `/R:<var>` return
        "RUN" => &["BG", "R"], // `/BG` — background child process; `/R:<var>` — return output to a var
        "RUNCODE" => &["BG", "R"], // run XPL text from a var; `/BG` backgrounds; `/R:<var>` returns
        "FGCOLOR" | "BGCOLOR" | "UIFGCOLOR" | "UIBGCOLOR" | "CRCOLOR" | "CMCRCOLOR" | "SEPCOLOR"
        | "TXTFGCOLOR" | "TXTBGCOLOR" | "DWFGCOLOR" | "DWBGCOLOR" | "SELBGCOLOR" | "SELFGCOLOR"
        | "HLCOLOR" | "TABBGCOLOR" | "TABXCOLOR" | "PAFGCOLOR" | "PABGCOLOR"
        | "PANECOLOR" | "BALPAIRSCOLOR" | "EMPHFGCOLOR" | "EMPHBGCOLOR"
        | "MARKFGCOLOR" | "MARKBGCOLOR" | "URLFGCOLOR" | "URLBGCOLOR"
        | "LNFGCOLOR" | "LNBGCOLOR" | "SCRATCHFGCOLOR" | "SCRATCHBGCOLOR"
        | "MSGHFGCOLOR" | "MSGBGCOLOR" | "MSGOKFGCOLOR" | "MSGOKBGCOLOR" => &["HSL"], // triple as H S L
        "MDA" => &["B", "BG", "HSL"], // `/B` (or `/BG`) — background layer; `/HSL` — the colour as H S L
        "MDAS" => &["B", "BG"], // `/B` (or `/BG`) — pick the background layer instead of the foreground
        "NEW" | "NE" => &["H", "V"], // `/H` `/V` — open the new buffer in a split pane (Stacked/SideBySide)
        "SCRATCH" => &["V"], // `/V` — prefill the new scratchpad with the clipboard
        "SCRATCHCLOSE" => &["V"], // `/V` — also insert the closing scratchpad's text at the caret
        "AB" => &["V"], // `/V` — verify (ask to save) before closing
        "ABD" => &["V"], // `/V` — verify (confirm) before deleting + closing
        "QUIT" | "QQ" => &["NV"], // `/NV` — quit without the save prompts
        "READKEY" | "RK" => &["NV", "D"], // `/NV` accepted; `/D` — write a key DESCRIPTION into var
        "PICK" | "CHOOSE" => &["I"], // `/I` — return the 1-based index instead of the item string
        "WAIT" => &["J"], // `/J <id>` — wait for a specific background job
        "LEARN" | "UNLEARN" => &["S", "F"], // `/S` — learn the selection; `/F <file>` — learn a word list
        #[cfg(feature = "email")]
        "SENDMAIL" => &["F", "W"], // `/F <file>` — body from a file; `/W` — run synchronously (sets $RC)
        #[cfg(feature = "email")]
        "LISTMAIL" | "SAVEMAIL" | "DELMAIL" => &["W"], // `/W` — run synchronously, so $RC is set on return

        _ => &[],
    };
    for s in switches {
        // Valued switches (`/R:<var>`, `/F:n`) validate by the name before the `:`.
        let name = s.split(':').next().unwrap_or(s);
        if !valid.iter().any(|v| v.eq_ignore_ascii_case(name)) {
            return Err(XplError::fatal(format!("{base}: unknown switch /{s}")));
        }
    }
    Ok(())
}

/// Parse a `SE`/`CH` command into a [`SearchSpec`]. Flags are order-free and come
/// first — attached to the verb (`SE/W`, already peeled into `vswitches`) or as
/// leading tokens before the pattern (`SE /W "x"`, `SE -w "x"`). A search switch
/// name is a *group* of single-letter flags (`/WA` → `W`,`A`). The pattern is
/// self-delimiting (the first char is the delimiter: `"x"`, `/x/`, …) with
/// `\<delim>`/`\n`/`\t` escapes; a bare alphanumeric pattern is allowed for `SE`.
/// An empty `SE` repeats the last search.
fn parse_search(base: &str, vswitches: &[String], args: &str) -> VmResult<SearchSpec> {
    // (backward, absolute, is_replace, verify)
    let (mut backward, mut absolute, is_replace, verify) = match base {
        "SE" => (false, false, false, false),
        "SEB" => (true, false, false, false),
        "SEA" => (false, true, false, false),
        "SEBA" => (true, true, false, false),
        // CI/CIA = XyWrite synonyms for CH/CHA (replace all).
        "CH" | "CI" => (false, false, true, false),
        "CHA" | "CIA" => (false, true, true, false),
        "CV" => (false, false, true, true),
        "CVA" => (false, true, true, true),
        _ => (false, false, false, false),
    };

    // Gather flag letters: verb-attached switches, then leading flag tokens. A
    // leading token is a flag group iff every char is a recognized flag letter.
    // An ALL-DIGIT verb-attached switch is the numeric `/n` count (`SE/3`, `CH/5`), not flag letters.
    let mut count: Option<usize> = None;
    let mut letters = String::new();
    for s in vswitches {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = s
                .parse()
                .map_err(|_| XplError::trappable(format!("{base}: /{s} is not a valid count")))?;
            if n == 0 {
                return Err(XplError::trappable(format!("{base}: /0 — the count must be at least 1")));
            }
            count = Some(n);
        } else {
            letters.push_str(s);
        }
    }
    let (leading, rest) =
        crate::switches::take_leading(args, |n| n.chars().all(|c| FLAG_LETTERS.contains(c)));
    for s in &leading {
        letters.push_str(s);
    }

    let mut whole_word = false;
    let mut from_top = false;
    let mut regex = false;
    let mut perl = false;
    let mut scope = false;
    let mut extend = false;
    let mut keep_case = false;
    for f in letters.chars() {
        match f.to_ascii_uppercase() {
            'W' => whole_word = true,
            'T' => from_top = true,
            'A' => absolute = true,
            'R' => regex = true,
            'P' => perl = true, // Perl-compatible regex (fancy-regex): lookaround/backrefs/atomic groups
            'S' => scope = true,
            'X' => extend = true,
            'K' => keep_case = true,
            'B' if !is_replace => backward = true,
            'B' => {} // backward is meaningless for replace-all; ignore
            other => return Err(XplError::trappable(format!("unknown search flag '{other}'"))),
        }
    }
    // `/K` (case-match the replacement) only makes sense for a replace — there's nothing to recase in
    // a plain find.
    if keep_case && !is_replace {
        return Err(XplError::trappable("/K (keep case) applies to CH, not SE"));
    }
    // `/X` (extend the selection to the match) is find-only and is the opposite of `/S` (which keeps
    // the selection fixed as a scope) — reject the nonsensical combinations rather than guess.
    if extend && is_replace {
        return Err(XplError::trappable("/X (extend selection) applies to SE, not replace"));
    }
    if extend && scope {
        return Err(XplError::trappable("/X (extend) and /S (search in selection) are mutually exclusive"));
    }
    // `/S` (scope to selection) and `/T` (start at the document edge) are mutually exclusive;
    // scoping wins (the selection is the explicit, narrower intent).
    if scope {
        from_top = false;
    }

    // The flags are known before the pattern, so a regex/Perl pattern keeps its backslash escapes
    // (`\d`, `\s`) instead of being unescaped as literal text.
    let (find, replace) = parse_pattern(base, rest, is_replace, regex || perl)?;

    Ok(SearchSpec {
        find,
        replace,
        backward,
        case_sensitive: absolute,
        whole_word,
        from_top,
        scope,
        regex,
        perl,
        keep_case,
        extend,
        verify,
        count,
    })
}

/// Apply one SORT switch token to `spec`. `/W /R /D` are valueless letter flags (and
/// may be grouped, `/WRD`); `/F:n` and `/N:n` carry a character count.
fn apply_sort_switch(spec: &mut SortSpec, tok: &str) -> VmResult<()> {
    let up = tok.to_ascii_uppercase();
    if let Some(v) = up.strip_prefix("F:") {
        spec.f_skip = Some(
            v.parse().map_err(|_| XplError::trappable(format!("SORT: /F:{v} expects a number")))?,
        );
    } else if let Some(v) = up.strip_prefix("N:") {
        spec.n_take = Some(
            v.parse().map_err(|_| XplError::trappable(format!("SORT: /N:{v} expects a number")))?,
        );
    } else if !up.is_empty() && up.chars().all(|c| matches!(c, 'W' | 'R' | 'D')) {
        for c in up.chars() {
            match c {
                'W' => spec.word = true,
                'R' => spec.reverse = true,
                'D' => spec.dedup = true,
                _ => unreachable!(),
            }
        }
    } else {
        return Err(XplError::trappable(format!("SORT: unknown switch /{tok}")));
    }
    Ok(())
}

/// Parse a `SORT` command's switches into a [`SortSpec`]. Switches are order-free and
/// come first — attached to the verb (`SORT/W/R`, already peeled into `vswitches`) or
/// as leading tokens (`SORT /W /F:6 /N:4`). SORT takes no positional argument, so any
/// non-switch text is an error.
fn parse_sort(vswitches: &[String], args: &str) -> VmResult<SortSpec> {
    let mut spec = SortSpec::default();
    for s in vswitches {
        apply_sort_switch(&mut spec, s)?;
    }
    let (leading, rest) = crate::switches::take_leading(args, |n| {
        let u = n.to_ascii_uppercase();
        u.starts_with("F:")
            || u.starts_with("N:")
            || (!u.is_empty() && u.chars().all(|c| matches!(c, 'W' | 'R' | 'D')))
    });
    for s in &leading {
        apply_sort_switch(&mut spec, s)?;
    }
    if !rest.trim().is_empty() {
        return Err(XplError::trappable(format!("SORT: unexpected argument '{}'", rest.trim())));
    }
    Ok(spec)
}

/// Apply one `DIR` switch token to `opts`. Recognizes `/B` (bare names-only), `/R`
/// (reverse), `/S` (recurse), `/O{N,S,D,E}` (order, optional `:`), and `/A…` attributes:
/// `D` dirs-only, `F` files-only, `H` show hidden, `S` show system (combinable, e.g.
/// `/ADH`), or bare `/A` = show everything. Returns `false` if the token isn't a known
/// DIR switch.
fn apply_dir_switch(opts: &mut DirOpts, tok: &str) -> bool {
    let up = tok.to_ascii_uppercase();
    match up.as_str() {
        "B" => {
            opts.bare = true;
            return true;
        }
        "R" => {
            opts.reverse = true;
            return true;
        }
        "S" => {
            opts.recurse = true;
            return true;
        }
        _ => {}
    }
    if let Some(rest) = up.strip_prefix('O') {
        opts.order = match rest.strip_prefix(':').unwrap_or(rest) {
            "N" => DirOrder::Name,
            "S" => DirOrder::Size,
            "D" => DirOrder::Date,
            "E" => DirOrder::Ext,
            _ => return false,
        };
        return true;
    }
    if let Some(rest) = up.strip_prefix('A') {
        let rest = rest.strip_prefix(':').unwrap_or(rest);
        // Bare `/A` shows everything (hidden + system); otherwise each letter constrains.
        if rest.is_empty() {
            opts.show_hidden = true;
            opts.show_system = true;
            return true;
        }
        for c in rest.chars() {
            match c {
                'D' => opts.only = DirOnly::Dirs,
                'F' => opts.only = DirOnly::Files,
                'H' => opts.show_hidden = true,
                'S' => opts.show_system = true,
                _ => return false,
            }
        }
        return true;
    }
    false
}

/// Parse a `DIR`/`LS` command's switches (verb-attached in `vswitches`, or leading tokens
/// in `args`) into [`DirOpts`], returning the leftover text as the optional path/filespec.
/// Read a split-pane switch off a verb: `/H` → `Some(true)` (Stacked, top/bottom), `/V` →
/// `Some(false)` (SideBySide, left/right), neither → `None`. Used by `OPEN`/`READ`/`DIR` etc. to open
/// into a new split pane instead of a new tab (`/H` wins if both are given). See `EditorOp::PaneSplitNew`.
fn pane_split_switch(vswitches: &[String]) -> Option<bool> {
    if vswitches.iter().any(|s| s.eq_ignore_ascii_case("H")) {
        Some(true)
    } else if vswitches.iter().any(|s| s.eq_ignore_ascii_case("V")) {
        Some(false)
    } else {
        None
    }
}

/// Split a `DIRSET` argument (`/OD/R`, `/OD /R`, or `OD R`) into uppercased switch NAMES. Sigils
/// (`/`, `-`) and whitespace all separate; empties dropped. Unlike [`switches::take_leading`], which
/// stops at the first non-switch token, this consumes the WHOLE string — a `DIRSET` operand is only
/// switches (no path), so `/OD/R` must split into `["OD", "R"]` rather than be read as one token.
fn split_switch_names(s: &str) -> Vec<String> {
    s.split(|c: char| c == '/' || c == '-' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_uppercase())
        .collect()
}

/// Merge `DIRSET` defaults into a `DIR` command's switch list. Keep every command switch, and add
/// each default whose LEADING LETTER isn't already present in the command — coarse per-letter
/// override: a command `/O…` supersedes a default `/O…`, while an unrelated command `/A…` leaves an
/// `/O…` default in place (the reported intent). Command switches come last so they win on any tie.
///
/// A sort order on the command line (`/O…`) additionally supersedes a default `/R` (reverse), not
/// just the default `/O…`. `/R` is a modifier on a sort order with no "off" form, so a `DIRSET /OD/R`
/// would otherwise force reverse onto every listing: typing `/ON` alone would silently stay reversed,
/// with no way back to ascending short of clearing `DIRSET`. Re-specifying the order is a complete
/// ordering directive; the caller can still say `/ON/R` to reverse explicitly (their own `/R` wins).
/// Split a `GREP` argument into `(pattern, path)`. The pattern is a `"double-quoted"` run (so it can
/// contain spaces) or the first whitespace-delimited token; whatever follows, trimmed, is the optional
/// search path/glob (a directory, a `*.rs` glob, or `dir/*.rs` — resolved like a `DIR` filespec). An
/// empty path is `None`.
fn parse_grep_args(args: &str) -> (String, Option<String>) {
    let s = args.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            let mask = rest[end + 1..].trim();
            return (rest[..end].to_string(), (!mask.is_empty()).then(|| mask.to_string()));
        }
        return (rest.to_string(), None); // no closing quote — take the remainder as the pattern
    }
    match s.split_once(char::is_whitespace) {
        Some((p, rest)) => {
            let mask = rest.trim();
            (p.to_string(), (!mask.is_empty()).then(|| mask.to_string()))
        }
        None => (s.to_string(), None),
    }
}

fn merge_dir_defaults(cmd: &[String], defaults: &[String]) -> Vec<String> {
    let letter = |s: &str| s.chars().next().map(|c| c.to_ascii_uppercase());
    let mut cmd_letters: std::collections::HashSet<Option<char>> = cmd.iter().map(|s| letter(s)).collect();
    if cmd_letters.contains(&Some('O')) {
        cmd_letters.insert(Some('R')); // an explicit order overrides a default reverse too
    }
    let mut out: Vec<String> =
        defaults.iter().filter(|d| !cmd_letters.contains(&letter(d))).cloned().collect();
    out.extend(cmd.iter().cloned());
    out
}

fn parse_dir(vswitches: &[String], args: &str) -> VmResult<(DirOpts, String)> {
    let mut opts = DirOpts::default();
    for s in vswitches {
        if !apply_dir_switch(&mut opts, s) {
            return Err(XplError::trappable(format!("DIR: unknown switch /{s}")));
        }
    }
    let (leading, rest) = crate::switches::take_leading(args, |n| {
        let mut probe = DirOpts::default();
        apply_dir_switch(&mut probe, n)
    });
    for s in &leading {
        apply_dir_switch(&mut opts, s);
    }
    Ok((opts, rest.to_string()))
}

/// Split `s` into whitespace-separated path tokens, honoring `"…"` so paths with spaces
/// stay intact (`COPY "a b.txt" c.txt` → two tokens). Quotes are removed; an empty `""`
/// is a valid (empty) token.
fn split_path_tokens(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut started = false; // distinguishes "" (a token) from plain whitespace
    for c in s.chars() {
        if c == '"' {
            in_quote = !in_quote;
            started = true;
        } else if c.is_whitespace() && !in_quote {
            if started {
                toks.push(std::mem::take(&mut cur));
                started = false;
            }
        } else {
            cur.push(c);
            started = true;
        }
    }
    if started {
        toks.push(cur);
    }
    toks
}

/// Parse exactly two path arguments for the two-path file verbs (`COPY`, `RENAME`).
fn parse_two_paths(verb: &str, args: &str) -> VmResult<(String, String)> {
    let toks = split_path_tokens(args);
    if toks.len() != 2 {
        return Err(XplError::trappable(format!(
            "{verb}: expected two paths <src> <dst> (quote paths with spaces)"
        )));
    }
    Ok((toks[0].clone(), toks[1].clone()))
}

/// Parse the self-delimiting pattern part of a search command into `(find, replace?)`.
fn parse_pattern(
    base: &str,
    rest: &str,
    is_replace: bool,
    regex: bool,
) -> VmResult<(String, Option<String>)> {
    match rest.chars().next() {
        // Empty SE → repeat the last search (the host fills the pattern).
        None if !is_replace => Ok((String::new(), None)),
        None => Err(XplError::trappable(format!("{base} requires a /find/replace/ pattern"))),
        // Bare alphanumeric pattern for SE (no delimiter).
        Some(c) if !is_replace && c.is_alphanumeric() => Ok((rest.to_string(), None)),
        Some(delim) => {
            let body = &rest[delim.len_utf8()..];
            // The find honors the regex mode (keep `\d` etc.); the replacement is
            // always literal text, so it uses literal escaping (`\n` → newline).
            let find = scan_delimited(body, delim, regex).first().cloned().unwrap_or_default();
            if is_replace {
                if find.is_empty() {
                    return Err(XplError::trappable(format!("{base}: empty search pattern")));
                }
                let replace = scan_delimited(body, delim, false).get(1).cloned().unwrap_or_default();
                Ok((find, Some(replace)))
            } else {
                Ok((find, None))
            }
        }
    }
}

/// Assign `value` into `current[index]`, returning the updated container.
///
/// - existing **Array** → numeric index; auto-extends with empty strings to fill
///   gaps (a negative index is fatal);
/// - existing **Map** → string-key entry;
/// - **undefined** → created as an Array if the index is a non-negative integer,
///   else a Map keyed by the index's string form;
/// - existing **scalar** → fatal (`name` is not indexable).
fn set_index(
    current: Option<Value>,
    index: &Value,
    value: Value,
    name: &str,
) -> VmResult<Value> {
    let as_int = match index.to_number() {
        Ok(crate::value::Number::Int(i)) => Some(i),
        Ok(crate::value::Number::Big(_)) => Some(i64::MAX), // absurd index → out of bounds
        Ok(crate::value::Number::Float(f)) => Some(f as i64),
        Err(_) => None,
    };
    match current {
        Some(Value::Array(mut items)) => {
            let n = as_int.ok_or_else(|| {
                XplError::fatal(format!("array index must be a number, got {:?}", index.to_str()))
            })?;
            if n < 0 {
                return Err(XplError::fatal(format!("negative array index {n} for ${name}")));
            }
            let u = n as usize;
            if u >= items.len() {
                items.resize(u + 1, Value::str(""));
            }
            items[u] = value;
            Ok(Value::Array(items))
        }
        Some(Value::Map(mut m)) => {
            m.insert(index.to_str(), value);
            Ok(Value::Map(m))
        }
        None => match as_int {
            Some(n) if n >= 0 => {
                let mut items = vec![Value::str(""); n as usize + 1];
                items[n as usize] = value;
                Ok(Value::Array(items))
            }
            _ => {
                let mut m = BTreeMap::new();
                m.insert(index.to_str(), value);
                Ok(Value::Map(m))
            }
        },
        Some(_) => Err(XplError::fatal(format!(
            "${name} is not an array or map; cannot assign to ${name}[…]"
        ))),
    }
}

/// The eval-time scope view handed to the expression evaluator and substituter.
/// Resolution order (specials win, per QN): built-in specials → host specials →
/// user vars.
struct Ctx<'a> {
    env: &'a Env,
    host: &'a dyn Host,
    args: &'a [Value],
    rv: &'a Value,
}

impl EvalContext for Ctx<'_> {
    fn get_var(&self, name: &str) -> Option<Value> {
        let f = fold_ident(name);
        match f.as_str() {
            "ERROR" => return Some(Value::Int(self.env.error as i64)),
            // `$ERRORMSG` is the generic-message catalog, indexed by code:
            // `$ERRORMSG[$ERROR]` names the last error's class. `[0]` = "[none]".
            "ERRORMSG" => {
                let catalog = (0..=crate::error::MAX_ERROR_CODE)
                    .map(|c| Value::str(crate::error::error_message(c)))
                    .collect();
                return Some(Value::Array(catalog));
            }
            "#" => return Some(Value::Int(self.args.len() as i64)),
            "*" => {
                let joined = self
                    .args
                    .iter()
                    .map(|v| v.to_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                return Some(Value::str(joined));
            }
            "RV" => return Some(self.rv.clone()),
            // $SC is served by the dedicated scan-code method, not the generic
            // special() map.
            "SC" => return Some(Value::Int(self.host.last_scan_code())),
            // $SCC ("scan code computed") = $SC + the bit value(s) of any held modifier key(s).
            "SCC" => {
                return Some(Value::Int(self.host.last_scan_code() + self.host.last_modifier_bits()))
            }
            _ => {}
        }
        // Positional args ($1 = args[0]).
        if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = f.parse::<usize>() {
                if n >= 1 {
                    return self.args.get(n - 1).cloned();
                }
            }
        }
        // Host-served specials, then user vars.
        if let Some(v) = self.host.special(&f) {
            return Some(v);
        }
        self.env.vars.get(&f).cloned()
    }

    fn sel(&self) -> String {
        self.host.special("SE").map(|v| v.to_str()).unwrap_or_default()
    }

    fn sel_at(&self, n: i64) -> String {
        self.host.selection_in(n)
    }

    fn read_file(&self, path: &str) -> Result<String, String> {
        self.host.read_file(path).map_err(|e| e.message().to_string())
    }

    fn random_int(&self, min: i64, max: i64) -> Option<i64> {
        self.host.random_int(min, max)
    }

    fn now(&self) -> Option<expr::DateParts> {
        // Reuse the host's clock specials (the same source as `$YEAR`/`$MON`/…) so `DATE()`/`TIME()`
        // and the date specials never disagree.
        let n = |k: &str| self.host.special(k)?.to_str().parse::<i64>().ok();
        Some(expr::DateParts {
            year: n("YEAR")?,
            month: n("MON")? as u32,
            day: n("MDAY")? as u32,
            hour: n("HOUR")? as u32,
            minute: n("MIN")? as u32,
            second: n("SEC")? as u32,
        })
    }

    fn binder(&self, script: bool, slot: char) -> Option<String> {
        self.host.binder(script, slot)
    }

    fn float_decimals(&self) -> Option<u8> {
        self.host.float_decimals()
    }

    fn key_binding(&self, scan: i64) -> Option<String> {
        self.host.key_binding(scan)
    }

    fn scc_key_name(&self, scc: i64) -> Option<String> {
        self.host.scc_key_name(scc)
    }

    fn path_is_dir(&self, path: &str) -> bool {
        self.host.path_is_dir(path)
    }

    fn path_exists(&self, path: &str) -> bool {
        self.host.path_exists(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::StubHost;

    #[test]
    fn dirset_splits_switch_strings_every_way() {
        assert_eq!(split_switch_names("/OD/R"), vec!["OD", "R"]);
        assert_eq!(split_switch_names("/OD /R"), vec!["OD", "R"]);
        assert_eq!(split_switch_names("od r"), vec!["OD", "R"]);
        assert_eq!(split_switch_names("/AH/AS"), vec!["AH", "AS"]);
        assert_eq!(split_switch_names(""), Vec::<String>::new());
    }

    #[test]
    fn dirset_merge_is_coarse_per_leading_letter() {
        let def = vec!["OD".to_string(), "R".to_string()];
        // No command switches → the defaults apply as-is.
        assert_eq!(merge_dir_defaults(&[], &def), vec!["OD", "R"]);
        // A command /O overrides BOTH the /O default AND the default /R: re-specifying the order is a
        // complete directive, so it can't leave the listing stuck in reverse.
        let m = merge_dir_defaults(&["ON".to_string()], &def);
        assert!(m.contains(&"ON".to_string()) && !m.contains(&"R".to_string()) && !m.contains(&"OD".to_string()));
        // …but the command's OWN /R still applies — an explicit reverse wins over the override.
        let m = merge_dir_defaults(&["ON".to_string(), "R".to_string()], &def);
        assert!(m.contains(&"ON".to_string()) && m.contains(&"R".to_string()) && !m.contains(&"OD".to_string()));
        // An explicit order cancels a default /R even when /R is the only default.
        let m = merge_dir_defaults(&["OS".to_string()], &vec!["R".to_string()]);
        assert!(m.contains(&"OS".to_string()) && !m.contains(&"R".to_string()));
        // A command /R with no /O leaves a default order in place (adds reverse to it).
        let m = merge_dir_defaults(&["R".to_string()], &vec!["OD".to_string()]);
        assert!(m.contains(&"OD".to_string()) && m.contains(&"R".to_string()));
        // An unrelated command switch leaves BOTH defaults in place.
        let m = merge_dir_defaults(&["AD".to_string()], &def);
        assert!(m.contains(&"OD".to_string()) && m.contains(&"R".to_string()) && m.contains(&"AD".to_string()));
        // Coarse /A: any command /A replaces ALL /A defaults, but not /O.
        let m = merge_dir_defaults(&["AF".to_string()], &vec!["AH".to_string(), "OD".to_string()]);
        assert!(m.contains(&"AF".to_string()) && m.contains(&"OD".to_string()) && !m.contains(&"AH".to_string()));
    }

    #[test]
    #[cfg(feature = "email")]
    fn take_first_arg_honors_quotes() {
        // Unquoted: split at the first space.
        assert_eq!(super::take_first_arg("body.txt alice@x.com"), ("body.txt".to_string(), "alice@x.com"));
        // Quoted: a path with spaces stays whole; the rest follows.
        assert_eq!(
            super::take_first_arg("\"my file.txt\" alice@x.com \"Subj\""),
            ("my file.txt".to_string(), "alice@x.com \"Subj\"")
        );
        // Only one arg.
        assert_eq!(super::take_first_arg("only.txt"), ("only.txt".to_string(), ""));
        // Leading whitespace is trimmed.
        assert_eq!(super::take_first_arg("   spaced.txt   rest"), ("spaced.txt".to_string(), "rest"));
        // Unterminated quote: take the remainder.
        assert_eq!(super::take_first_arg("\"unterminated"), ("unterminated".to_string(), ""));
    }

    #[test]
    fn txt_title_switch_parsing() {
        // No /T: → whole thing is the body.
        assert_eq!(parse_txt("just a body"), (None, "just a body"));
        // Quoted title with spaces; rest is the body.
        assert_eq!(parse_txt(r#"/T:"My Title" the body"#), (Some("My Title".into()), "the body"));
        // Bare (unquoted) title to the next space; case-insensitive sigil.
        assert_eq!(parse_txt("-t:Notes hello"), (Some("Notes".into()), "hello"));
        // Empty body after a title is fine.
        assert_eq!(parse_txt(r#"/T:"Only Title""#), (Some("Only Title".into()), ""));
    }

    #[test]
    fn path_tokens_honor_quotes_and_count() {
        assert_eq!(split_path_tokens("a.txt b.txt"), vec!["a.txt", "b.txt"]);
        // Quoted paths keep their spaces; quotes are stripped.
        assert_eq!(
            split_path_tokens(r#""a file.txt" out.txt"#),
            vec!["a file.txt", "out.txt"]
        );
        // Wrong arity is rejected (one path, three paths).
        assert!(parse_two_paths("COPY", "only.txt").is_err());
        assert!(parse_two_paths("COPY", "a b c").is_err());
        assert_eq!(parse_two_paths("COPY", "a b").unwrap(), ("a".to_string(), "b".to_string()));
    }

    fn run(src: &str) -> (Interpreter, StubHost) {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run(src, &mut host).expect("script ran");
        (interp, host)
    }

    fn run_res(src: &str) -> Result<(Interpreter, StubHost), XplError> {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run(src, &mut host)?;
        Ok((interp, host))
    }

    #[test]
    fn set_and_read_variables() {
        let (i, _) = run("SET x = 2 + 3\nPR $x");
        assert_eq!(i.output(), &["5".to_string()]);
        assert_eq!(i.get_var("x"), Some(&Value::Int(5)));
    }

    #[test]
    fn budget_slices_a_run_and_resumes_to_completion() {
        // A long straight-line script (200 increments + a PR) — enough instructions that a small
        // budget must yield many times before finishing.
        let mut src = String::from("SET x = 0\n");
        for _ in 0..200 {
            src.push_str("SET x = $x + 1\n");
        }
        src.push_str("PR $x\n");

        // Reference: an unbudgeted (foreground) run.
        let (refi, _) = run(&src);
        let want_out = refi.output().to_vec();
        assert_eq!(want_out, &["200".to_string()]);

        // Sliced: tiny budget, resume until done. Result must match the unbudgeted run exactly.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run_sliced(&src, 8, &mut host).unwrap();
        assert!(interp.is_yielded(), "a 200-instruction script must yield under an 8-step budget");
        let mut slices = 1;
        while interp.is_yielded() {
            interp.resume_yield(8, &mut host).unwrap();
            slices += 1;
        }
        assert!(slices > 10, "expected many slices, got {slices}");
        assert!(!interp.is_suspended(), "must run to completion");
        assert_eq!(interp.get_var("x"), Some(&Value::Int(200)));
        assert_eq!(interp.output(), want_out.as_slice());
    }

    #[test]
    fn compound_assignment_operators() {
        // += -= *= /= ^= desugar to `name = name <op> (expr)`.
        let (i, _) = run("SET x = 5\nSET x += 3");
        assert_eq!(i.get_var("x"), Some(&Value::Int(8)));
        let (i, _) = run("SET x = 2\nSET x ^= 10");
        assert_eq!(i.get_var("x"), Some(&Value::Int(1024)));
        let (i, _) = run("SET x = 5\nSET x -= 2");
        assert_eq!(i.get_var("x"), Some(&Value::Int(3)));
        let (i, _) = run("SET x = 5\nSET x *= 4");
        assert_eq!(i.get_var("x"), Some(&Value::Int(20)));
        let (i, _) = run("SET x = 20\nSET x /= 4");
        assert_eq!(i.get_var("x"), Some(&Value::Int(5)));
        // The RHS is a full expression (it's parenthesized around the op).
        let (i, _) = run("SET x = 10\nSET x *= 2 + 3");
        assert_eq!(i.get_var("x"), Some(&Value::Int(50)));
    }

    #[test]
    fn set_accepts_optional_dollar_on_the_target() {
        // `SET $x = …` / `SET $x += …` mean the same as the bare-name forms.
        let (i, _) = run("SET $x = 7");
        assert_eq!(i.get_var("x"), Some(&Value::Int(7)));
        let (i, _) = run("SET x = 7\nSET $x += 1");
        assert_eq!(i.get_var("x"), Some(&Value::Int(8)));
    }

    #[test]
    fn compound_assignment_on_an_indexed_element() {
        let (i, _) = run("SET a = SPLIT(\"10,20,30\", \",\")\nSET a[1] += 5\nPR $a[1]");
        assert_eq!(i.output().last().map(String::as_str), Some("25"));
    }

    #[test]
    fn change_verb_errors_echo_the_actual_verb() {
        // A malformed change command names the verb the user actually typed (CH or its CI synonym),
        // not a hardcoded "CH".
        let e = run_res("CH").err().unwrap();
        assert!(e.message().starts_with("CH "), "got {}", e.message());
        let e2 = run_res("CI").err().unwrap();
        assert!(e2.message().starts_with("CI "), "got {}", e2.message());
        // Empty-find form too: `CIA //replace/`.
        let e3 = run_res("CIA //x/").err().unwrap();
        assert!(e3.message().starts_with("CIA:"), "got {}", e3.message());
    }

    #[test]
    fn nested_runaway_loop_inherits_the_slice_budget() {
        // The user's actual freeze shape: a RUN-ed (here RUNCODE-ed) script with an infinite
        // no-WAIT loop. It must INHERIT the caller's preemption budget so it YIELDS instead of
        // spinning the CPU unbudgeted inside the nested run — otherwise the GUI freezes.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        let src = "SET code = \"WHILE 1\\nSET a = 1\\nENDWHILE\"\nRUNCODE $code\n";
        interp.run_sliced(src, 8, &mut host).unwrap();
        assert!(interp.is_yielded(), "a nested runaway loop must yield, not run unbudgeted");
        // It never finishes; drive several slices and confirm it stays alive and parked (no freeze).
        for _ in 0..20 {
            interp.resume_yield(8, &mut host).unwrap();
            assert!(interp.is_yielded(), "still looping under the budget");
        }
        // abort() drops the whole stack at once (outer NestedRun frame + inner Yield frame).
        assert!(interp.abort(), "abort reports it stopped a running script");
        assert!(!interp.is_suspended(), "the entire continuation stack is cleared");
    }

    #[test]
    fn sliced_nested_run_cascades_back_to_the_caller() {
        // When a sliced nested run finishes, resume_yield must cascade out to the parked outer
        // frame so the caller continues after the RUN (here: it PRs once the RUNCODE returns).
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        // Inner: 60 straight-line statements — enough to force a yield under an 8-step budget.
        let mut inner = String::new();
        for _ in 0..60 {
            inner.push_str("SET a = 1\\n");
        }
        let src = format!("SET code = \"{inner}\"\nRUNCODE $code\nPR done\n");
        interp.run_sliced(&src, 8, &mut host).unwrap();
        assert!(interp.is_yielded(), "the nested run must yield at least once");
        let mut guard = 0;
        while interp.is_yielded() {
            interp.resume_yield(8, &mut host).unwrap();
            guard += 1;
            assert!(guard < 10_000, "should finish, not loop forever");
        }
        assert!(!interp.is_suspended(), "stack drained after the cascade");
        assert_eq!(
            interp.output(),
            &["done".to_string()],
            "the outer script continued after the nested RUN completed"
        );
    }

    #[test]
    fn run_bg_dispatches_background_script_op() {
        // `RUN/BG <script>` → a host RunBackgroundScript op (spawns `$ED --run` as a job),
        // NOT the in-process `exec_run` path. No `/R` → return_var None.
        let (_, h) = run("RUN/BG foo.xpl");
        assert!(
            h.log.iter().any(|l| l
                == r#"RunBackgroundScript { path: "foo.xpl", args: [], return_var: None }"#),
            "log = {:?}",
            h.log
        );
    }

    #[test]
    fn run_bg_forwards_positional_args() {
        // `RUN/BG <script> a b` carries the positional args through to the host op, so the child
        // `$ED --run <script> a b` receives them as `$1`/`$2` — parity with a foreground RUN.
        let (_, h) = run("RUN/BG foo.xpl alpha beta");
        assert!(
            h.log.iter().any(|l| l
                == r#"RunBackgroundScript { path: "foo.xpl", args: ["alpha", "beta"], return_var: None }"#),
            "log = {:?}",
            h.log
        );
    }

    #[test]
    fn runcode_evaluates_xpl_from_a_variable() {
        // `RUNCODE $var` runs the XPL text held in the variable (an eval).
        let (i, _) = run("SET c = \"PR ran from var\"\nRUNCODE $c");
        assert_eq!(i.output().last().map(String::as_str), Some("ran from var"));
    }

    #[test]
    fn runcode_defers_inner_dollar_refs_to_run_time() {
        // Single-pass substitution: the code's own `$name` isn't expanded when RUNCODE's args are
        // substituted — it resolves when the code runs. Set `name` AFTER building the code string.
        let (i, _) = run("SET code = \"PR hi $name\"\nSET name = World\nRUNCODE $code");
        assert_eq!(i.output().last().map(String::as_str), Some("hi World"));
    }

    #[test]
    fn runcode_bg_dispatches_background_code_op() {
        let (_, h) = run("RUNCODE/BG PR hi");
        assert!(
            h.log.iter().any(|l| l == r#"RunBackgroundCode { code: "PR hi", return_var: None }"#),
            "log = {:?}",
            h.log
        );
        let (_, h2) = run("RUNCODE/BG/R:out PR hi");
        assert!(
            h2.log.iter().any(|l| l
                == r#"RunBackgroundCode { code: "PR hi", return_var: Some("OUT") }"#),
            "log = {:?}",
            h2.log
        );
    }

    #[test]
    fn run_bg_return_switch_names_the_variable() {
        // `RUN/BG/R:<var> <script>` carries the destination variable through to the host op. The
        // switch token is case-folded (like all verb switches) → `RESULT`; harmless since XPL
        // variable names are case-insensitive ($result == $RESULT).
        let (_, h) = run("RUN/BG/R:result foo.xpl");
        assert!(
            h.log.iter().any(|l| l
                == r#"RunBackgroundScript { path: "foo.xpl", args: [], return_var: Some("RESULT") }"#),
            "log = {:?}",
            h.log
        );
    }

    #[test]
    fn unbudgeted_run_never_yields() {
        // Foreground `run` (no budget) must complete in one go — preemption is opt-in.
        let (i, _) = run("SET x = 0\nSET x = $x + 1\nSET x = $x + 1\nPR $x");
        assert!(!i.is_yielded());
        assert!(!i.is_suspended());
        assert_eq!(i.output(), &["2".to_string()]);
    }

    #[test]
    fn message_dispatches_os_message_op() {
        // `MESSAGE <text>` fires an OS-native box via `EditorOp::OsMessage` (fire-and-forget: it does
        // NOT suspend, unlike a bare `MSG`). The StubHost just logs the op.
        let (i, h) = run("MESSAGE Alarm: stand up");
        assert!(!i.is_suspended(), "MESSAGE must not park the script");
        assert_eq!(h.log, ["OsMessage { title: None, text: \"Alarm: stand up\" }"]);
    }

    #[test]
    fn message_title_switch_sets_the_caption() {
        // `MESSAGE /T:"title" <text>` — the quoted title becomes the window caption; the rest is the
        // body. Bare (unquoted) titles stop at the first space, like TXT.
        let (_, h) = run("MESSAGE /T:\"Wake up\" Time to stretch");
        assert_eq!(
            h.log,
            ["OsMessage { title: Some(\"Wake up\"), text: \"Time to stretch\" }"]
        );
    }

    #[test]
    fn lock_key_verbs_dispatch_setlock() {
        // S=on, C=off, T=toggle; CL=Caps, NL=Num, SL=Scroll. (Tested at this layer so the OS lock
        // state isn't actually flipped during the suite — the StubHost just logs the op.)
        let (_, h) = run("SCL\nCCL\nTCL\nSNL\nCNL\nTNL\nSSL\nCSL\nTSL");
        assert_eq!(
            h.log,
            [
                "SetLock { key: Caps, mode: On }",
                "SetLock { key: Caps, mode: Off }",
                "SetLock { key: Caps, mode: Toggle }",
                "SetLock { key: Num, mode: On }",
                "SetLock { key: Num, mode: Off }",
                "SetLock { key: Num, mode: Toggle }",
                "SetLock { key: Scroll, mode: On }",
                "SetLock { key: Scroll, mode: Off }",
                "SetLock { key: Scroll, mode: Toggle }",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn binder_load_verbs_dispatch_setbinder() {
        // Quoted literal → text binder; the slot is upper-cased.
        let (_, h) = run(r#"LDTX h "hello""#);
        assert!(
            h.log.contains(&r#"SetBinder { script: false, slot: 'H', source: Literal("hello") }"#.to_string()),
            "log: {:?}",
            h.log
        );
        // LDSC and its synonym LDPM both make a script binder.
        for verb in ["LDSC", "LDPM"] {
            let (_, h) = run(&format!(r#"{verb} b "DFA""#));
            assert!(
                h.log.contains(&r#"SetBinder { script: true, slot: 'B', source: Literal("DFA") }"#.to_string()),
                "{verb} log: {:?}",
                h.log
            );
        }
        // No source → the selection; a bare token → a file path.
        let (_, h) = run("LDTX h");
        assert!(h.log.contains(&"SetBinder { script: false, slot: 'H', source: Selection }".to_string()));
        let (_, h) = run("LDTX p notes.txt");
        assert!(h.log.contains(&r#"SetBinder { script: false, slot: 'P', source: File("notes.txt") }"#.to_string()));
    }

    #[test]
    fn binder_refs_dispatch_usebinder_casefolded() {
        let (_, h) = run("@a");
        assert!(h.log.contains(&"UseBinder { script: false, slot: 'A' }".to_string()), "log: {:?}", h.log);
        let (_, h) = run("&Z");
        assert!(h.log.contains(&"UseBinder { script: true, slot: 'Z' }".to_string()));
        let (_, h) = run("@0"); // digit slot
        assert!(h.log.contains(&"UseBinder { script: false, slot: '0' }".to_string()));
        // Chained in one statement-line: both fire (and a leading verb still works).
        let (_, h) = run("GT;@A");
        assert!(h.log.contains(&"UseBinder { script: false, slot: 'A' }".to_string()), "log: {:?}", h.log);
    }

    #[test]
    fn binder_bad_slot_is_trappable() {
        let (i, h) = run(r#"LDTX hh "x""#);
        assert!(i.status_message().unwrap().contains("slot must be"));
        assert!(!h.log.iter().any(|l| l.starts_with("SetBinder")), "log: {:?}", h.log);
    }

    #[test]
    fn if_else_chooses_branch() {
        let (i, _) = run("SET n = 7\nIF $n > 5\nPR big\nELSE\nPR small\nENDIF");
        assert_eq!(i.output(), &["big".to_string()]);
    }

    #[test]
    fn single_line_if() {
        let (i, _) = run("SET n = 1\nIF $n PR yes");
        assert_eq!(i.output(), &["yes".to_string()]);
    }

    #[test]
    fn while_loop_counts() {
        let src = "SET i = 0\nSET sum = 0\nWHILE $i < 5\nSET sum = $sum + $i\nSET i = $i + 1\nENDWHILE\nPR $sum";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["10".to_string()]);
    }

    #[test]
    fn while_with_break_and_continue() {
        let src = "SET i = 0\nSET s = 0\nWHILE 1\nSET i = $i + 1\nIF $i > 9 BREAK\nIF $i % 2 == 0 CONTINUE\nSET s = $s + $i\nENDWHILE\nPR $s";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["25".to_string()]);
    }

    #[test]
    fn goto_loop() {
        let src = "SET n = 0\n:loop\nSET n = $n + 1\nIF $n < 3 GOTO :loop\nPR $n";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["3".to_string()]);
    }

    #[test]
    fn sub_call_with_args_and_return() {
        let src = "CALL add 3 4\nPR $RV\nSUB add\nRETURN $1 + $2\nENDSUB";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["7".to_string()]);
    }

    #[test]
    fn routine_invokable_without_call_verb() {
        // `add 3 4` ≡ `CALL add 3 4` — the CALL verb is optional for a defined routine.
        let src = "add 3 4\nPR $RV\nSUB add\nRETURN $1 + $2\nENDSUB";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["7".to_string()]);
        // A genuinely unknown verb still errors.
        assert!(run_res("nosuchverb x").is_err(), "unknown command should still error");
    }

    #[test]
    fn positional_args_count_and_all() {
        let src = "CALL show a b c\nSUB show\nPR $#\nPR $*\nENDSUB";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["3".to_string(), "a b c".to_string()]);
    }

    #[test]
    fn local_saves_and_restores() {
        let src = "SET x = outer\nCALL f\nPR $x\nSUB f\nLOCAL x\nSET x = inner\nPR $x\nENDSUB";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["inner".to_string(), "outer".to_string()]);
    }

    // ---- arrays / maps ------------------------------------------------------

    #[test]
    fn split_index_and_len() {
        // LEN is an expression function, so it runs in SET (PR only substitutes).
        let src = "SET parts = SPLIT(\"a,b,c\", \",\")\nSET n = LEN($parts)\nPR $parts[1]\nPR $n";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["b".to_string(), "3".to_string()]);
    }

    #[test]
    fn array_element_assignment_and_autoextend() {
        // Create via index, auto-extend with gaps, read back.
        let src = "SET a[0] = first\nSET a[2] = third\nSET n = LEN($a)\nPR $a[0]\nPR $a[1]\nPR $a[2]\nPR $n";
        let (i, _) = run(src);
        assert_eq!(
            i.output(),
            &["first".to_string(), "".to_string(), "third".to_string(), "3".to_string()]
        );
    }

    #[test]
    fn map_assignment_by_string_key() {
        let src = "SET m[name] = Alice\nSET m[role] = admin\nPR $m[name] is $m[role]";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["Alice is admin".to_string()]);
    }

    #[test]
    fn build_array_in_a_loop() {
        let src = "SET i = 0\nWHILE $i < 3\nSET a[$i] = $i * 10\nSET i = $i + 1\nENDWHILE\nSET out = JOIN($a, \"-\")\nPR $out";
        let (i, _) = run(src);
        assert_eq!(i.output(), &["0-10-20".to_string()]);
    }

    #[test]
    fn index_assign_to_scalar_is_fatal() {
        assert!(matches!(run_res("SET x = 5\nSET x[0] = y"), Err(XplError::Fatal(_))));
    }

    // ---- error model --------------------------------------------------------

    #[test]
    fn unknown_verb_is_fatal() {
        assert!(matches!(run_res("FLOOB x"), Err(XplError::Fatal(_))));
    }

    #[test]
    fn assigning_special_is_fatal() {
        assert!(matches!(run_res("SET OS = nope"), Err(XplError::Fatal(_))));
    }

    #[test]
    fn divide_by_zero_is_fatal() {
        assert!(matches!(run_res("SET x = 5 / 0"), Err(XplError::Fatal(_))));
    }

    #[test]
    fn extend_search_flag_parses_and_rejects_bad_combos() {
        // `/X` sets extend on a find (verb-attached and leading-token forms).
        assert!(parse_search("SE", &["X".into()], "/./").unwrap().extend);
        assert!(parse_search("SE", &[], "/X /./").unwrap().extend);
        // `/X` is find-only (rejected on a replace) and is mutually exclusive with `/S` (scope).
        assert!(parse_search("CH", &["X".into()], "/foo/bar/").is_err());
        assert!(parse_search("SE", &["X".into(), "S".into()], "/./").is_err());
    }

    #[test]
    fn trappable_error_sets_flag_and_continues() {
        let mut interp = Interpreter::new();
        let mut host = StubHost {
            next_editor_error: Some(XplError::trappable("not found")),
            ..Default::default()
        };
        interp
            .run("INSERT x\nIF $ERROR PR caught", &mut host)
            .expect("trappable did not abort");
        assert_eq!(interp.output(), &["caught".to_string()]);
        assert!(!interp.error_flag()); // `PR caught` (action) auto-cleared it
    }

    #[test]
    fn error_flag_auto_clears_on_next_action() {
        let mut interp = Interpreter::new();
        let mut host = StubHost {
            next_editor_error: Some(XplError::trappable("boom")),
            ..Default::default()
        };
        interp
            .run("INSERT x\nPR after\nIF $ERROR PR still\nIFNOT $ERROR PR cleared", &mut host)
            .unwrap();
        assert_eq!(interp.output(), &["after".to_string(), "cleared".to_string()]);
        assert!(!interp.error_flag());
    }

    #[test]
    fn seterr_clerr_manage_flag() {
        let (i, _) = run("SETERR\nIF $ERROR PR on\nCLERR\nIFNOT $ERROR PR off");
        assert_eq!(i.output(), &["on".to_string(), "off".to_string()]);
    }

    #[test]
    fn error_is_a_numeric_code_default_generic_one() {
        // Bare SETERR raises the generic code 1; $ERROR reads back as that number.
        let (i, _) = run("SETERR\nPR $ERROR");
        assert_eq!(i.output(), &["1".to_string()]);
    }

    #[test]
    fn seterr_takes_an_explicit_code() {
        let (i, _) = run("SETERR 7\nPR $ERROR");
        assert_eq!(i.output(), &["7".to_string()]);
    }

    #[test]
    fn seterr_rejects_a_non_numeric_code() {
        assert!(run_res("SETERR nope").is_err());
    }

    #[test]
    fn errormsg_catalog_is_indexed_by_code() {
        // $ERRORMSG[0] is the sentinel; $ERRORMSG[$ERROR] names the current error's class.
        let (i, _) = run("PR $ERRORMSG[0]\nSETERR 2\nPR $ERRORMSG[$ERROR]");
        assert_eq!(i.output(), &["[none]".to_string(), "file not found".to_string()]);
    }

    #[test]
    fn errormsg_out_of_range_is_empty() {
        let (i, _) = run("PR [$ERRORMSG[9999]]");
        assert_eq!(i.output(), &["[]".to_string()]);
    }

    // ---- host seam ----------------------------------------------------------

    #[test]
    fn loadkbd_and_insert_reach_host() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("INSERT q\nLOADKBD layout2.kbd", &mut host).unwrap();
        assert_eq!(
            host.log,
            vec![r#"InsertText("q")"#.to_string(), "LOADKBD layout2.kbd".to_string()]
        );
        assert_eq!(host.loaded_kbd.as_deref(), Some("layout2.kbd"));
    }

    #[test]
    fn host_special_resolves_in_expressions() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.specials.insert("OS".into(), Value::str("windows"));
        host.specials.insert("SE".into(), Value::str("picked"));
        interp.run("PR $OS\nSET u = UPPER(SEL())\nPR $u", &mut host).unwrap();
        assert_eq!(interp.output(), &["windows".to_string(), "PICKED".to_string()]);
    }

    #[test]
    fn sv_saves_selection_to_var() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.specials.insert("SE".into(), Value::str("picked text"));
        interp.run("SV snippet\nPR $snippet", &mut host).unwrap();
        assert_eq!(interp.output(), &["picked text".to_string()]);
    }

    #[test]
    fn scan_code_special_reads_dedicated_method() {
        let mut interp = Interpreter::new();
        let mut host = StubHost { last_scan: 38, ..Default::default() };
        interp.run("SET k = $SC\nPR $k", &mut host).unwrap();
        assert_eq!(interp.output(), &["38".to_string()]);
    }

    #[test]
    fn insert_unwraps_a_quoted_literal() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("INSERT \"q\"\nINSERT \" \"\nINSERT bare", &mut host).unwrap();
        assert_eq!(
            host.log,
            vec![
                r#"InsertText("q")"#.to_string(),
                r#"InsertText(" ")"#.to_string(),
                r#"InsertText("bare")"#.to_string(),
            ]
        );
    }

    #[test]
    fn bare_quoted_string_command_inserts_its_text() {
        // A quoted-string literal at command position is sugar for INSERT — standalone and chained
        // (e.g. the `AR;" "` keybinding), so the space isn't lost after another command runs.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run(r#"" ""#, &mut host).unwrap(); // bare quoted space, standalone
        interp.run(r#"INSERT "x";" ""#, &mut host).unwrap(); // chained after a command
        assert_eq!(
            host.log,
            vec![
                r#"InsertText(" ")"#.to_string(),
                r#"InsertText("x")"#.to_string(),
                r#"InsertText(" ")"#.to_string(),
            ]
        );
    }

    #[test]
    fn insert_of_backslash_and_quote_literals_survive_the_splitter() {
        // The desugared forms for a backslash / double-quote KBD cell must make
        // it through splitter -> parser -> INSERT intact.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run(r#"INSERT "\\""#, &mut host).unwrap(); // a single backslash
        interp.run(r#"INSERT "\"""#, &mut host).unwrap(); // a single double-quote
        assert_eq!(
            host.log,
            vec![r#"InsertText("\\")"#.to_string(), r#"InsertText("\"")"#.to_string()]
        );
    }

    fn for_out(src: &str) -> Vec<String> {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run(src, &mut host).unwrap();
        interp.output().to_vec()
    }
    fn assert_for(src: &str, want: &[&str]) {
        let got = for_out(src);
        let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want, "src: {src}");
    }

    #[test]
    fn for_range_counts_steps_and_descends() {
        assert_for("FOR i = 1 TO 3 ; PR $i ; EF", &["1", "2", "3"]);
        assert_for("FOR i = 0 TO 10 STEP 5 ; PR $i ; EF", &["0", "5", "10"]);
        assert_for("FOR i = 3 TO 1 STEP -1 ; PR $i ; EF", &["3", "2", "1"]);
        assert_for("FOR i = 5 TO 1 ; PR $i ; EF", &[]); // empty range → zero iterations
        // The counter is left one PAST the last value (standard counting-loop behavior).
        assert_for("FOR i = 1 TO 3 ; EF\nPR $i", &["4"]);
    }

    #[test]
    fn for_exclusive_upper_bound() {
        assert_for("FOR/X i = 1 TO 3 ; PR $i ; EF", &["1", "2"]);
        // The array idiom: /X with `0 TO LEN($a)` walks exactly every index.
        assert_for(
            r#"SET a = SPLIT("x,y,z", ",") ; FOR/X i = 0 TO LEN($a) ; PR $($a[$i]) ; EF"#,
            &["x", "y", "z"],
        );
    }

    #[test]
    fn for_in_arrays_maps_and_strings() {
        assert_for(r#"SET a = SPLIT("x,y,z", ",") ; FOR v IN $a ; PR $v ; EF"#, &["x", "y", "z"]);
        // Maps iterate their keys (sorted).
        assert_for(r#"SET m["b"] = 2 ; SET m["a"] = 1 ; FOR k IN $m ; PR $k ; EF"#, &["a", "b"]);
        // Strings iterate characters.
        assert_for(r#"FOR c IN "ab" ; PR $c ; EF"#, &["a", "b"]);
    }

    #[test]
    fn for_tuple_index_value() {
        // Array: first var = index, second = element.
        assert_for(
            r#"FOR i, x IN ["a", "b", "c"] ; PR $($i . ":" . $x) ; EF"#,
            &["0:a", "1:b", "2:c"],
        );
        // Map: first var = key (sorted), second = value.
        assert_for(
            r#"SET m["b"] = 2 ; SET m["a"] = 1 ; FOR k, v IN $m ; PR $($k . "=" . $v) ; EF"#,
            &["a=1", "b=2"],
        );
        // String: first var = index, second = char.
        assert_for(r#"FOR i, c IN "hi" ; PR $($i . $c) ; EF"#, &["0h", "1i"]);
        // BREAK/CONTINUE still work in the tuple form.
        assert_for(
            r#"FOR i, x IN [9, 8, 7, 6] ; IF $i == 2 ; BREAK ; EI ; PR $x ; EF"#,
            &["9", "8"],
        );
    }

    #[test]
    fn map_literal_builds_and_iterates_in_scripts() {
        assert_for(r#"SET m = {a: 1, b: 2} ; PR $m"#, &["{a=1, b=2}"]);
        // Index a map literal stored in a var.
        assert_for(r#"SET m = {x: 10, y: 20} ; PR $($m["y"])"#, &["20"]);
        // Tuple FOR over a map literal.
        assert_for(r#"FOR k, v IN {a: 1, b: 2} ; PR $($k . $v) ; EF"#, &["a1", "b2"]);
    }

    #[test]
    fn switch_basic_and_default() {
        // The matching CASE runs; the others don't.
        assert_for(
            "SET x = 2 ; SWITCH $x ; CASE 1 ; PR one ; CASE 2 ; PR two ; CASE 3 ; PR three ; ESW",
            &["two"],
        );
        // DEFAULT runs when nothing matches.
        assert_for("SWITCH 9 ; CASE 1 ; PR one ; DEFAULT ; PR other ; ENDSWITCH", &["other"]);
        // No match, no DEFAULT → nothing in the switch runs; flow continues after it.
        assert_for("SWITCH 9 ; CASE 1 ; PR one ; ESW ; PR done", &["done"]);
    }

    #[test]
    fn switch_multi_value_strings_and_no_fallthrough() {
        // Comma-separated values share one CASE.
        assert_for(
            "SET x = 3 ; SWITCH $x ; CASE 1, 2, 3 ; PR low ; CASE 4, 5 ; PR high ; ESW",
            &["low"],
        );
        // String scrutinee + string case values.
        assert_for(r#"SWITCH "b" ; CASE "a" ; PR A ; CASE "b" ; PR B ; ESW"#, &["B"]);
        // No fall-through: only the first matching arm runs.
        assert_for("SWITCH 1 ; CASE 1 ; PR first ; CASE 1 ; PR second ; ESW", &["first"]);
    }

    #[test]
    fn switch_block_form_and_expression_scrutinee() {
        // Multi-line block form; the scrutinee is a full expression, evaluated once.
        let src = "SET n = 1\nSWITCH $n + 1\nCASE 2\nPR matched\nDEFAULT\nPR no\nENDSWITCH";
        assert_for(src, &["matched"]);
        // A CASE body can hold multiple statements.
        assert_for("SWITCH 1 ; CASE 1 ; PR a ; PR b ; DEFAULT ; PR z ; ESW", &["a", "b"]);
    }

    #[test]
    fn for_break_continue_and_nesting() {
        assert_for("FOR i = 1 TO 5 ; IF $i == 3 ; BREAK ; EI ; PR $i ; EF", &["1", "2"]);
        // CONTINUE still advances the counter (skips 2, doesn't hang).
        assert_for("FOR i = 1 TO 4 ; IF $i == 2 ; CONTINUE ; EI ; PR $i ; EF", &["1", "3", "4"]);
        // Nested loops use independent hidden temps.
        assert_for("FOR i = 1 TO 2 ; FOR j = 1 TO 2 ; PR $j ; EF ; EF", &["1", "2", "1", "2"]);
    }

    #[test]
    fn fence_is_live_recognizes_shell_out_and_keywords() {
        // Shell pass-throughs are live code (they run a command), so a `|X …|` PRINTEX field executes
        // instead of printing verbatim.
        assert!(fence_is_live("X curl https://example.com/x.txt"));
        assert!(fence_is_live("XY echo hi"));
        assert!(fence_is_live("XV ls"));
        // Statement keywords and `;`-chains stay live; plain text stays substitution.
        assert!(fence_is_live("set a = 1"));
        assert!(fence_is_live("for n in [1,2,3];set s=$n;ef"));
        assert!(!fence_is_live("just some words"));
        assert!(!fence_is_live("$weath"));
    }

    #[test]
    fn stray_carriage_returns_are_tolerated() {
        // CRLF endings, a leading CR, and a stray lone CR between verb and args (a mangled file)
        // must all parse — `\r` is stripped, so `pr\r $x` is `pr $x`, not the verb `PR\r`.
        assert_for("\rset x = 5\r\npr\r $x\r\r\n", &["5"]);
    }

    #[test]
    fn cr_escape_in_a_string_literal_survives_cr_stripping() {
        // `flatten` strips stray CR *bytes*, but the two-char `\r` ESCAPE is `\`+`r` (no 0x0D), so it
        // passes through untouched and `lex_string` turns it into a real CR in the value. ASC 13 = CR.
        assert_for(r#"SET x = "abc\rdef" ; PR $(LEN($x)) ; PR $(ASC(SUBSTR($x, 3, 1)))"#, &["7", "13"]);
    }

    #[test]
    fn array_literal_builds_and_iterates_in_scripts() {
        // Build with the literal + PUSH; mutate via SET (helpers are pure).
        assert_for("SET a = [] ; SET a = PUSH($a, 1) ; SET a = PUSH($a, 2) ; PR $a", &["[1, 2]"]);
        // FOR over a literal.
        assert_for("FOR v IN [10, 20, 30] ; PR $v ; EF", &["10", "20", "30"]);
        // Sort a literal, index the result.
        assert_for("SET s = SORT([3, 1, 2]) ; PR $($s[0])", &["1"]);
    }

    #[test]
    fn for_step_zero_is_a_parse_error_and_iter_is_callable() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        assert!(interp.run("FOR i = 1 TO 5 STEP 0 ; PR $i ; EF", &mut host).is_err());
        // ITER is also a normal expression function (a map's keys, an array's elements…).
        assert_for(r#"PR $(JOIN(ITER(SPLIT("a,b", ",")), "-"))"#, &["a-b"]);
    }

    #[test]
    fn vars_persist_across_runs() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("SET keep = 42", &mut host).unwrap();
        interp.run("PR $keep", &mut host).unwrap();
        assert_eq!(interp.output(), &["42".to_string()]);
    }

    // ---- library system -----------------------------------------------------

    #[test]
    fn call_into_library_routine() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp
            .load_user_library_source("SUB greet\nPR hello $1\nENDSUB")
            .unwrap();
        interp.run("CALL greet World", &mut host).unwrap();
        assert_eq!(interp.output(), &["hello World".to_string()]);
    }

    #[test]
    fn library_routine_can_return_value() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp
            .load_user_library_source("SUB dbl\nRETURN $1 * 2\nENDSUB")
            .unwrap();
        interp.run("CALL dbl 21\nPR $RV", &mut host).unwrap();
        assert_eq!(interp.output(), &["42".to_string()]);
    }

    #[test]
    fn user_library_overrides_system() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.load_system_library_source("SUB who\nPR system\nENDSUB").unwrap();
        interp.load_user_library_source("SUB who\nPR user\nENDSUB").unwrap();
        interp.run("CALL who", &mut host).unwrap();
        assert_eq!(interp.output(), &["user".to_string()]);
    }

    #[test]
    fn script_sub_wins_over_library() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.load_user_library_source("SUB f\nPR from-lib\nENDSUB").unwrap();
        interp.run("CALL f\nSUB f\nPR from-script\nENDSUB", &mut host).unwrap();
        assert_eq!(interp.output(), &["from-script".to_string()]);
    }

    #[test]
    fn library_label_does_not_collide_with_script_label() {
        // Both define :loop — per-routine label scoping keeps them independent.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp
            .load_user_library_source(
                "SUB count\nSET i = 0\n:loop\nSET i = $i + 1\nIF $i < 3 GOTO :loop\nRETURN $i\nENDSUB",
            )
            .unwrap();
        interp
            .run("SET n = 0\n:loop\nSET n = $n + 1\nIF $n < 2 GOTO :loop\nCALL count\nPR $n-$RV", &mut host)
            .unwrap();
        assert_eq!(interp.output(), &["2-3".to_string()]);
    }

    // ---- RUN positional arguments ------------------------------------------

    #[test]
    fn run_passes_positional_args_to_the_script() {
        // `RUN <file> a b` seeds the script's `$1`/`$2`/`$*`/`$#`, like a routine call.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("echo.xpl".into(), "PR $#|$1|$2|$*".into());
        interp.run("RUN echo.xpl alpha beta", &mut host).unwrap();
        assert_eq!(interp.output().last().map(String::as_str), Some("2|alpha|beta|alpha beta"));
    }

    #[test]
    fn run_error_reports_file_and_line() {
        // A fatal error in a RUN-ed file is stamped `<file>:<line>: …` so a bad startup line points
        // at itself (the "unknown switch /#" from a XyWrite-style `#---#` divider was untraceable).
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("boot.xpl".into(), "PR one\nNOSUCHVERB here\nPR three".into());
        let err = interp.run("RUN boot.xpl", &mut host).unwrap_err();
        assert!(err.message().starts_with("boot.xpl:2: "), "located: {}", err.message());
        assert!(err.message().contains("unknown command"), "{}", err.message());
    }

    #[test]
    fn run_error_location_is_the_innermost_file() {
        // `RUN a` → `RUN b` (bad line 2): the error names b.xpl:2, not the outer a.xpl.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("a.xpl".into(), "PR a\nRUN b.xpl".into());
        host.files.insert("b.xpl".into(), "PR b\nBADVERB\nPR b2".into());
        let err = interp.run("RUN a.xpl", &mut host).unwrap_err();
        assert!(err.message().starts_with("b.xpl:2: "), "innermost: {}", err.message());
    }

    #[test]
    fn top_level_error_has_no_file_prefix() {
        // A bad command NOT inside a RUN keeps its bare message (no spurious file:line).
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        let err = interp.run("NOSUCHVERB", &mut host).unwrap_err();
        assert!(!err.message().contains(':') || !err.message().contains(".xpl"), "{}", err.message());
    }

    #[test]
    fn top_level_file_error_is_stamped_with_the_label() {
        // A bad line DIRECTLY in a labelled top-level run (startup.xpl / `--run <file>`) → file:line.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        let err = interp
            .run_sliced_with_args("PR ok\nNOSUCHVERB\n", &[], Some("startup.xpl"), 1000, &mut host)
            .unwrap_err();
        assert!(err.message().starts_with("startup.xpl:2: "), "{}", err.message());
    }

    #[test]
    fn top_level_label_defers_to_a_nested_run_location() {
        // startup.xpl RUNs a file that fails: the RUN-ed file's location wins, not startup.xpl.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("lib.xpl".into(), "PR lib\nBADVERB\n".into());
        let err = interp
            .run_sliced_with_args("PR start\nRUN lib.xpl\n", &[], Some("startup.xpl"), 1000, &mut host)
            .unwrap_err();
        assert!(err.message().starts_with("lib.xpl:2: "), "innermost wins: {}", err.message());
    }

    #[test]
    fn run_without_args_has_no_positionals() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("echo.xpl".into(), "PR $#|$1".into());
        interp.run("RUN echo.xpl", &mut host).unwrap();
        assert_eq!(interp.output().last().map(String::as_str), Some("0|"));
    }

    #[test]
    fn run_quoted_path_with_spaces_takes_args() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("my script.xpl".into(), "PR $#|$1".into());
        interp.run(r#"RUN "my script.xpl" one"#, &mut host).unwrap();
        assert_eq!(interp.output().last().map(String::as_str), Some("1|one"));
    }

    #[test]
    fn run_unquoted_spaced_path_without_args_still_resolves() {
        // Backward compatibility: the split would guess path "my" + arg "script.xpl", but "my" isn't
        // a file while the whole operand is — so it's treated as the path with no args.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("my script.xpl".into(), "PR $#|$1".into());
        interp.run("RUN my script.xpl", &mut host).unwrap();
        assert_eq!(interp.output().last().map(String::as_str), Some("0|"));
    }

    // ---- library management verbs ------------------------------------------

    #[test]
    fn loadlib_addlib_unloadlib_via_verbs() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("a.lib".into(), "SUB a\nPR routine-a\nENDSUB".into());
        host.files.insert("b.lib".into(), "SUB b\nPR routine-b\nENDSUB".into());
        // A library loaded mid-run takes effect on the *next* run (the current
        // program was already compiled), so load first, then call.
        interp.run("LOADLIB a.lib\nADDLIB b.lib", &mut host).unwrap();
        assert_eq!(interp.user_library_count(), 2);
        interp.run("CALL a\nCALL b", &mut host).unwrap();
        assert_eq!(interp.output(), &["routine-a".to_string(), "routine-b".to_string()]);
        // Unload, then a CALL to a now-missing routine is fatal.
        interp.run("UNLOADLIB", &mut host).unwrap();
        assert_eq!(interp.user_library_count(), 0);
        assert!(matches!(interp.run("CALL a", &mut host), Err(XplError::Fatal(_))));
    }

    #[test]
    fn loadlib_parse_error_is_trappable_and_preserves_library() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("good.lib".into(), "SUB g\nPR ok\nENDSUB".into());
        host.files.insert("bad.lib".into(), "PR top-level junk".into());
        interp.run("LOADLIB good.lib", &mut host).unwrap();
        // Loading a bad file sets $ERROR but leaves the good library intact.
        interp.run("LOADLIB bad.lib\nIF $ERROR PR load-failed", &mut host).unwrap();
        assert!(interp.output().contains(&"load-failed".to_string()));
        // good.lib is still loaded (RELOADLIB replays it; CALL works).
        interp.run("CALL g", &mut host).unwrap();
        assert_eq!(interp.output(), &["ok".to_string()]);
    }

    #[test]
    fn loadlib_and_addlib_confirm_on_success() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("a.lib".into(), "SUB g\nPR ok\nENDSUB".into());
        host.files.insert("b.lib".into(), "SUB h\nPR ok\nENDSUB\nSUB i\nPR ok\nENDSUB".into());
        interp.run("LOADLIB a.lib", &mut host).unwrap();
        assert!(
            interp.output().iter().any(|l| l == "loaded 1 routine from a.lib"),
            "LOADLIB confirms: {:?}",
            interp.output()
        );
        interp.run("ADDLIB b.lib", &mut host).unwrap();
        assert!(
            interp.output().iter().any(|l| l == "added 2 routines from b.lib"),
            "ADDLIB confirms with a plural count: {:?}",
            interp.output()
        );
    }

    #[test]
    fn loadkbd_confirms_on_success() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("LOADKBD mine.kbd", &mut host).unwrap();
        assert_eq!(interp.status_message(), Some("loaded keyboard from mine.kbd"));
    }

    #[test]
    fn reloadlib_replays_load_sequence_and_picks_up_edits() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("lib.lib".into(), "SUB v\nPR v1\nENDSUB".into());
        interp.run("LOADLIB lib.lib", &mut host).unwrap();
        interp.run("CALL v", &mut host).unwrap();
        assert_eq!(interp.output(), &["v1".to_string()]);
        // Edit the file on disk, then RELOADLIB re-reads it.
        host.files.insert("lib.lib".into(), "SUB v\nPR v2\nENDSUB".into());
        interp.run("RELOADLIB", &mut host).unwrap();
        interp.run("CALL v", &mut host).unwrap();
        assert_eq!(interp.output(), &["v2".to_string()]);
    }

    #[test]
    fn missing_library_file_is_trappable() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("LOADLIB nope.lib\nIFNOT $ERROR PR unexpected\nIF $ERROR PR caught", &mut host).unwrap();
        assert!(interp.output().contains(&"caught".to_string()));
    }

    #[test]
    fn shadowing_library_routine_is_rejected() {
        let mut interp = Interpreter::new();
        // A routine named after a built-in verb is skipped.
        let rejected = interp
            .load_user_library_source("SUB PR\nPR x\nENDSUB\nSUB ok\nPR y\nENDSUB")
            .unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(interp.user_library_count(), 1); // only OK
    }

    // ---- RUN ----------------------------------------------------------------

    #[test]
    fn run_executes_a_script_file_sharing_env() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("setup.xpl".into(), "SET shared = 99\nPR in-run".into());
        interp.run("RUN setup.xpl\nPR $shared", &mut host).unwrap();
        assert_eq!(interp.output(), &["in-run".to_string(), "99".to_string()]);
    }

    #[test]
    fn run_missing_file_is_trappable() {
        let (i, _) = run("RUN nope.xpl\nIF $ERROR PR caught");
        // run() helper has no files; the missing RUN sets $ERROR.
        assert!(i.output().contains(&"caught".to_string()));
    }

    #[test]
    fn input_suspends_then_resumes_with_value() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("INPUT name \"Your name?\"\nPR $name", &mut host).unwrap();
        // Parked on a Line request carrying the prompt; nothing printed yet.
        let req = interp.input_request().expect("parked on INPUT");
        assert_eq!(req.kind, InputKind::Line);
        assert_eq!(req.prompt.as_deref(), Some("Your name?"));
        assert!(interp.output().is_empty());
        // Resume with the typed line → continues past INPUT, prints it, settles.
        interp.resume_input(Value::str("Bob"), &mut host).unwrap();
        assert!(interp.input_request().is_none());
        assert_eq!(interp.output(), &["Bob".to_string()]);
    }

    #[test]
    fn readkey_suspends_with_key_kind_and_no_prompt() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("READKEY k\nPR $k", &mut host).unwrap();
        let req = interp.input_request().expect("parked on READKEY");
        assert_eq!(req.kind, InputKind::Key);
        assert_eq!(req.prompt, None);
        interp.resume_input(Value::str("a"), &mut host).unwrap();
        assert_eq!(interp.output(), &["a".to_string()]);
    }

    #[test]
    fn two_inputs_resume_in_sequence() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("INPUT a \"1?\"\nINPUT b \"2?\"\nPR $a $b", &mut host).unwrap();
        assert_eq!(interp.input_request().unwrap().prompt.as_deref(), Some("1?"));
        // First resume re-suspends on the second INPUT.
        interp.resume_input(Value::str("x"), &mut host).unwrap();
        assert_eq!(interp.input_request().unwrap().prompt.as_deref(), Some("2?"));
        interp.resume_input(Value::str("y"), &mut host).unwrap();
        assert!(interp.input_request().is_none());
        assert_eq!(interp.output(), &["x y".to_string()]);
    }

    #[test]
    fn input_resumes_across_a_while_loop() {
        // The loop's ip/frame state must survive each pause: two iterations, two resumes.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        let src = "SET i = 0\nWHILE $i < 2\nINPUT x\nSET i = $i + 1\nENDWHILE\nPR done";
        interp.run(src, &mut host).unwrap();
        assert!(interp.input_request().is_some()); // first iteration
        interp.resume_input(Value::str("one"), &mut host).unwrap();
        assert!(interp.input_request().is_some()); // second iteration
        interp.resume_input(Value::str("two"), &mut host).unwrap();
        assert!(interp.input_request().is_none());
        assert_eq!(interp.output(), &["done".to_string()]);
    }

    #[test]
    fn input_resumes_inside_a_called_sub() {
        // A pause inside a CALLed sub keeps the whole frame stack; resume returns to caller.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        let src = "CALL s\nPR after\nSUB s\nINPUT v\nPR $v\nENDSUB";
        interp.run(src, &mut host).unwrap();
        assert!(interp.input_request().is_some());
        interp.resume_input(Value::str("hi"), &mut host).unwrap();
        assert!(interp.input_request().is_none());
        assert_eq!(interp.output(), &["hi".to_string(), "after".to_string()]);
    }

    #[test]
    fn input_inside_run_pauses_and_resumes() {
        // A pause inside a RUN-ed script parks the WHOLE continuation; resuming the nested INPUT
        // cascades back out to the outer script after the RUN.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("ask.xpl".into(), "INPUT x\nPR got $x".into());
        interp.run("RUN ask.xpl\nPR after", &mut host).unwrap();
        assert!(interp.input_request().is_some()); // parked on the nested INPUT
        interp.resume_input(Value::str("hi"), &mut host).unwrap();
        assert!(interp.input_request().is_none()); // cascade finished both scripts
        assert_eq!(interp.output(), &["got hi".to_string(), "after".to_string()]);
    }

    #[test]
    fn pause_inside_nested_runs_cascades_outward() {
        // RUN A → A RUNs B → B pauses (WAIT). Resuming drives B, then A, then the top script.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("a.xpl".into(), "PR a-before\nRUN b.xpl\nPR a-after".into());
        host.files.insert("b.xpl".into(), "PR b-before\nWAIT 5\nPR b-after".into());
        interp.run("RUN a.xpl\nPR top-after", &mut host).unwrap();
        assert!(interp.sleep_request().is_some()); // parked on B's timed WAIT
        // Output up to the pause (the first pass).
        assert_eq!(interp.output(), &["a-before".to_string(), "b-before".to_string()]);
        interp.resume_wait(&mut host).unwrap();
        assert!(interp.sleep_request().is_none());
        // Resuming blanks the channel, then the cascade finishes B → A → top in order.
        assert_eq!(
            interp.output(),
            &["b-after".to_string(), "a-after".to_string(), "top-after".to_string()]
        );
    }

    #[test]
    fn run_wait_loop_with_call_and_exitall_drains_to_unsuspended() {
        // Mirrors loop10.xpl: a WAIT loop inside a RUN, with a CALL and EXITALL partway.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert(
            "loop.xpl".into(),
            "SET c = 1\n:l\nPR c=$c\nWAIT 1000\nSET c = $c + 1\nIF $c == 3 CALL out\nIF $c <= 5 GOTO l\nPR loopdone\nEXIT\nSUB out\nPR bail\nEXITALL\nENDSUB".into(),
        );
        interp.run("RUN loop.xpl", &mut host).unwrap();
        assert_eq!(interp.output(), &["c=1".to_string()]); // first pass before the WAIT
        let mut guard = 0;
        while interp.is_suspended() {
            guard += 1;
            assert!(guard < 50, "stuck suspended after {guard} resumes; out={:?}", interp.output());
            interp.resume_wait(&mut host).unwrap();
        }
        // The channel reflects the LAST pass (each resume blanks it); EXITALL bailed at c==3.
        assert!(interp.output().iter().any(|s| s == "bail"), "out={:?}", interp.output());
        // And a fresh run works afterward (not stuck).
        interp.run("PR after", &mut host).unwrap();
        assert_eq!(interp.output(), &["after".to_string()]);
    }

    #[test]
    fn exitall_inside_run_aborts_the_outer_script() {
        // EXITALL in a RUN-ed sub-script stops EVERYTHING — the outer's `PR top-after` must not run.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("sub.xpl".into(), "PR sub\nEXITALL".into());
        interp.run("RUN sub.xpl\nPR top-after", &mut host).unwrap();
        assert!(!interp.is_suspended());
        assert_eq!(interp.output(), &["sub".to_string()]); // top-after aborted
        // (Plain EXIT, by contrast, only ends the sub: the outer continues — covered elsewhere.)
    }

    #[test]
    fn exitall_during_resume_aborts_the_whole_stack() {
        // top RUNs a script that WAITs then EXITALLs; resuming must drop the parked outer too.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("w.xpl".into(), "PR w\nWAIT 1000\nEXITALL".into());
        interp.run("RUN w.xpl\nPR top-after", &mut host).unwrap();
        assert!(interp.is_suspended()); // parked on the nested WAIT
        interp.resume_wait(&mut host).unwrap();
        assert!(!interp.is_suspended()); // EXITALL dropped the whole continuation stack
        assert!(!interp.output().iter().any(|s| s == "top-after"), "out={:?}", interp.output());
        // …and a fresh run works afterward.
        interp.run("PR after", &mut host).unwrap();
        assert_eq!(interp.output(), &["after".to_string()]);
    }

    #[test]
    fn exit_inside_run_lets_the_outer_continue() {
        // Contrast with EXITALL: plain EXIT ends only the sub; the outer runs on after the RUN.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        host.files.insert("sub.xpl".into(), "PR sub\nEXIT\nPR unreached".into());
        interp.run("RUN sub.xpl\nPR top-after", &mut host).unwrap();
        assert!(!interp.is_suspended());
        assert_eq!(interp.output(), &["sub".to_string(), "top-after".to_string()]);
    }

    #[test]
    fn input_into_a_special_is_fatal() {
        assert!(run_res("INPUT SE").is_err());
        assert!(run_res("READKEY ERROR").is_err());
    }

    #[test]
    fn input_with_no_variable_is_fatal() {
        assert!(run_res("INPUT").is_err());
        assert!(run_res("READKEY").is_err());
    }

    #[test]
    fn bare_wait_parks_on_all_jobs_then_resumes() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("WAIT\nPR done", &mut host).unwrap();
        assert_eq!(interp.wait_request(), Some(WaitTarget::AllJobs));
        assert!(interp.input_request().is_none());
        assert!(interp.output().is_empty()); // PR hasn't run yet
        interp.resume_wait(&mut host).unwrap();
        assert!(interp.wait_request().is_none());
        assert_eq!(interp.output(), &["done".to_string()]);
    }

    #[test]
    fn wait_job_form_parks_on_that_job() {
        let (i, _) = run("WAIT/J 7"); // attached switch
        assert_eq!(i.wait_request(), Some(WaitTarget::Job(7)));
        let (i, _) = run("WAIT /J 7"); // leading switch
        assert_eq!(i.wait_request(), Some(WaitTarget::Job(7)));
    }

    #[test]
    fn wait_milliseconds_parks_as_a_timed_sleep() {
        // `WAIT <ms>` parks as a non-freezing timed pause (sleep_request, not wait_request);
        // the editor times it. resume_wait continues the script.
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("WAIT 250\nPR after", &mut host).unwrap();
        assert_eq!(interp.sleep_request(), Some(250));
        assert!(interp.wait_request().is_none());
        assert!(interp.output().is_empty()); // parked; PR hasn't run
        interp.resume_wait(&mut host).unwrap();
        assert!(interp.sleep_request().is_none());
        assert_eq!(interp.output(), &["after".to_string()]);
    }

    #[test]
    fn wait_bad_job_id_is_trappable() {
        assert!(run_res("WAIT/J abc").is_err());
    }

    #[test]
    fn a_fresh_run_clears_a_stale_suspension() {
        let mut interp = Interpreter::new();
        let mut host = StubHost::default();
        interp.run("INPUT x", &mut host).unwrap();
        assert!(interp.input_request().is_some());
        // Starting a new top-level run abandons the parked script.
        interp.run("PR fresh", &mut host).unwrap();
        assert!(interp.input_request().is_none());
        assert_eq!(interp.output(), &["fresh".to_string()]);
    }
}
