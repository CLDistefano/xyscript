---
title: run-commands
tags: 
created: 2026-06-17T04:07:18
---
# Running scripts: `RUN` and `RUN/BG`

*Draft for the XyScript user manual.*

XyScript runs `.xpl` scripts in three ways:

| Form | Where it runs | Blocks the editor? | Touches your live document? |
|------|---------------|--------------------|------------------------------|
| `RUN <file>` | In your editing session (same process) | Yes — synchronous | Yes |
| `RUN/BG <file>` | A separate background `xyscript` process | No — runs in the background | No (its own session) |
| `xyscript --run <file>` | A one-shot headless process (command line) | n/a — no UI | No (its own session) |

The rule of thumb:

- Use **`RUN`** for a script that *is part of what you're doing now* — it edits your current document, sets your variables, and finishes before you carry on.
- Use **`RUN/BG`** for *separate work that shouldn't freeze your editing* — a long computation, a batch job over files, anything that produces output or files you'll pick up later.

---

## `RUN <file>` — run a script in your session

```
RUN setup.xpl
```

- The path is resolved against the current directory (`$CWD`); an absolute path is used as-is.
- The script runs **synchronously** against your live session: it can move your caret, edit the active document, read and set your variables, change settings, and so on. The editor is busy until it finishes.
- A `RUN` script **can pause.** `READKEY`, `INPUT`, `WAIT`, and `TXT` work inside a `RUN`-ed script: the whole continuation (the sub-script plus the outer script waiting on the `RUN`) is parked and resumes when the input arrives / the wait elapses, cascading back out after the `RUN`. Nested `RUN`s pause correctly too.

`RUN` is the right tool for setup routines, macros, and generated scripts (e.g. the config snapshot `SAVECFG` writes).

---

## `RUN/BG <file>` — run a script in the background

```
RUN/BG crunch.xpl
```

This launches the script as a **separate `xyscript` process** and returns immediately — your editor stays fully responsive. It reuses the same background-job machinery as `X/BG`:

- Its output streams **live into the `*Shell Output*` tab**, under a header line, as it runs.
- It appears in `JOBS`, counts toward `$JOBS`, and can be stopped with `KILL`.
- When it finishes, `$XO` holds its full output and `$RC` its exit code.

### What the background script can and can't do

Because it's a separate process, the background script has **its own editing session** — its own documents, its own variables, and a working directory equal to your `$CWD` at launch. It **cannot see or change your live document, caret, or variables.** Think of it as a helper you hand a task to, not a second cursor in your file.

It also starts with a **clean environment**: it loads the built-in **system library only** — *not* your loaded user library, your config, or your key binders. If your background script needs library routines, have it `LOADLIB` them itself.

Results come back to you in one of three ways:

1. **The `*Shell Output*` tab** — whatever the script `PR`s (or `MSG`s) is shown there.
2. **Files it writes** — e.g. the script `SAAS result.txt`, and you open or merge that file afterward.
3. **A return variable** — see [`/R:<var>`](#return-the-output-into-a-variable) below.

### Knowing which job you launched: `$LJ`

`$LJ` is the id of the **most-recently-launched** background job (`RUN/BG` or `X/BG`), or `0` if none. Capture it right after launching so you can wait on or kill *that* job:

```
RUN/BG crunch.xpl
WAIT/J $LJ          ; wait (without freezing) until that job finishes
ME result.txt       ; merge the file it produced into the document at the caret
```

`WAIT/J <id>` parks the *current* script until that one job ends; the editor keeps rendering meanwhile. (`WAIT` with no id waits for **all** background jobs; `WAIT <ms>` is a plain timed pause.)

### Return the output into a variable

`RUN/BG/R:<var> <file>` does everything `RUN/BG` does, **and** delivers the finished job's full output into the named variable `<var>` when it completes:

```
RUN/BG/R:result crunch.xpl
WAIT/J $LJ
PR $result          ; the child's entire output, now in a variable
```

This is the robust choice when you might have **several background jobs running at once** — each `/R` variable receives *its own* job's output, whereas `$XO`/`$RC` always reflect whichever job finished most recently.

Notes:
- The variable is set **on completion**, so `WAIT/J $LJ` (or otherwise wait) before reading it.
- The delivered value is the job's complete output, including the trailing newline — `TRIM` it if you need it bare.
- `/R:<var>` works on `X/BG` too (e.g. `X/BG/R:out dir`).

### Stopping a background script

```
KILL <id>           ; e.g.  KILL $LJ
```

`KILL` terminates the background process **immediately** — it's a real process stop, so it works even if the script is stuck in a long operation.

---

## `xyscript --run <file>` — headless, from the command line

```
xyscript --run report.xpl
```

Runs a script with **no editor window** and exits. `PR`/`MSG` output goes to standard output; errors go to standard error. This is exactly what `RUN/BG` launches behind the scenes (`$ED --run …`), but you can also use it directly for scripting and pipelines.

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | ran successfully |
| `1` | a fatal error stopped the script |
| `2` | the script file was missing or unreadable |
| `3` | the script tried to read interactive input (`INPUT`/`READKEY`), which isn't available headlessly |

Interactive input is not available in this mode (there's no console to type into).

---

## Worked examples

**Generate a file in the background and fold it in when ready.** Run as one command line (the `;` separates steps):

```
RUN/BG makelist.xpl ; WAIT/J $LJ ; ME list.txt
```

`makelist.xpl` builds its output and `SAAS list.txt`; your editor stays live during the work; when it finishes, the file is merged at your caret.

**Capture a computed value straight into a variable:**

```
RUN/BG/R:total sales-sum.xpl ; WAIT/J $LJ ; PR Total = $total
```

**Fire-and-forget, then check on it later.** Launch without waiting, keep editing, then:

```
JOBS                 ; see what's still running
PR $XO               ; the last finished job's output
KILL 3               ; stop job 3 if it's misbehaving
```

---

## Quick reference

| Command / variable | Meaning |
|--------------------|---------|
| `RUN <file>` | Run a script in your session, synchronously |
| `RUN/BG <file>` | Run a script as a background process |
| `RUN/BG/R:<var> <file>` | …and deliver its output into `$<var>` on completion |
| `xyscript --run <file>` | Run a script headlessly from the command line |
| `WAIT/J <id>` | Wait (non-freezing) until that job finishes |
| `WAIT` | Wait until all background jobs finish |
| `WAIT <ms>` | Pause this script for a number of milliseconds |
| `JOBS` | List the running background jobs |
| `KILL <id>` | Stop a background job immediately |
| `ME <file>` / `MERGE <file>` | Insert a file's contents at the caret |
| `$LJ` | Id of the most-recently-launched background job (0 = none) |
| `$JOBS` | Count of running background jobs |
| `$XO` | Full output of the most-recently-finished job |
| `$RC` | Exit code of the most-recently-finished job |
| `$CWD` | Current directory (background jobs and relative paths resolve here) |
| `$ED` | Path of the running `xyscript` executable |

---

## Things to watch for

- **A background script can't touch your live document.** Hand results back through a file, the `*Shell Output*` tab, or a `/R:<var>` return variable.
- **`$XO`/`$RC` are "last finished," not per-job.** With only one background job at a time they're reliable; with several at once, prefer `/R:<var>` to avoid ambiguity.
- **The background script's environment is clean** — system library only, working directory `$CWD`. It does *not* inherit your user library, config, or binders; have it `LOADLIB` what it needs.
- **`RUN/BG` is for interactive use.** Launching it from a `--run` script that exits immediately can fail to start the child (the parent process exits before the job has a chance to spawn).
- **Background shell-out needs its own `TRUST`.** A background script that wants to run `X`/`XV` must enable `TRUST 1` itself — the child starts untrusted, regardless of the parent's setting.
