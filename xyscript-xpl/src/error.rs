//! The two-tier error model (`stage1-interpreter-plan.md` §8).
//!
//! - **Trappable** — a verb's soft failure (search-not-found, file-not-found,
//!   shell exit nonzero, …). Sets `$ERROR` to a non-zero *code* and *keeps
//!   running*. `$ERROR` auto-clears (back to `0`) at each action dispatch
//!   (except an exempt verb set), so `<action>; IF $ERROR …` works.
//!   `SETERR`/`CLERR` manage it manually.
//! - **Fatal** — unknown command/label/sub, divide-by-zero, write to a
//!   read-only special, a structural/parse error. Aborts the whole script and
//!   is surfaced visibly. This is what keeps a bad key-cell from wedging the
//!   input loop.
//!
//! Trappable failures never propagate as `Err` out of the executor — they are
//! folded into `$ERROR` at the dispatch site. Only [`XplError::Fatal`] unwinds.
//! The enum is intentionally shaped so a future `TRY`/catch (trappable-fatals,
//! §12 backlog) can be added as a catch boundary without changing it.
//!
//! ## Error codes (`$ERROR` / `$ERRORMSG`)
//!
//! `$ERROR` is a numeric code: `0` = success, any `N > 0` names a class of
//! failure. `$ERRORMSG[n]` returns the *generic* description for code `n`
//! (`$ERRORMSG[0] == "[none]"`); the *specific* runtime text of the last
//! failure stays on the status line / `$LS`. The catalog below is the single
//! source of truth for both the numbers and their generic text.
//!
//! Rollout is incremental: a plain [`XplError::trappable`] reports the generic
//! code [`ErrCode::Generic`] (`1`); curated sites call
//! [`XplError::trappable_code`] with a specific [`ErrCode`]. Uncoded sites keep
//! working — they simply read as code `1` until migrated.

/// Systematic trappable-error codes. The discriminant is the value `$ERROR`
/// takes; [`error_message`] maps it to the generic text `$ERRORMSG[n]` returns.
/// Code `0` (success) is intentionally absent — it is not an error. Grow this
/// list as more sites are migrated to specific codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrCode {
    /// A generic / as-yet-uncoded soft failure. The default for [`XplError::trappable`].
    Generic = 1,
    /// A file (to open/read) does not exist.
    FileNotFound = 2,
    /// A verb needs a selection/defined block and there is none.
    NothingSelected = 3,
    /// A numeric argument was expected but the text isn't a number.
    BadNumber = 4,
    /// The command / verb / label / routine name is not known.
    UnknownCommand = 5,
    /// An index, line, or count is outside the valid range.
    OutOfRange = 6,
    /// A directory (to change to / list) does not exist.
    NoSuchDir = 7,
    /// A target file already exists (and clobbering was declined).
    FileExists = 8,
    /// An argument is missing or malformed (not one of the above).
    BadArgument = 9,
}

/// The highest defined error code — the top index of the `$ERRORMSG` catalog.
pub const MAX_ERROR_CODE: u32 = 9;

/// The **generic** message for an error code (what `$ERRORMSG[n]` returns).
/// `0` → `"[none]"`; a code past the catalog → `""` (the lenient out-of-range
/// convention shared with array indexing).
pub fn error_message(code: u32) -> &'static str {
    match code {
        0 => "[none]",
        1 => "error",
        2 => "file not found",
        3 => "nothing selected",
        4 => "invalid number",
        5 => "unknown command",
        6 => "out of range",
        7 => "no such directory",
        8 => "file already exists",
        9 => "invalid argument",
        _ => "",
    }
}

/// An error raised while running XPL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XplError {
    /// Soft failure: set `$ERROR` to `code` (non-zero) and continue.
    Trappable {
        /// The `$ERROR` value this failure reports (see [`ErrCode`]).
        code: u32,
        /// The specific human-readable message (goes to the status line / `$LS`).
        msg: String,
    },
    /// Hard failure: abort the script.
    Fatal(String),
}

impl XplError {
    pub fn fatal(msg: impl Into<String>) -> XplError {
        XplError::Fatal(msg.into())
    }
    /// A trappable failure with the generic code ([`ErrCode::Generic`] = `1`).
    pub fn trappable(msg: impl Into<String>) -> XplError {
        XplError::Trappable { code: ErrCode::Generic as u32, msg: msg.into() }
    }
    /// A trappable failure with a specific [`ErrCode`] — for curated sites.
    pub fn trappable_code(code: ErrCode, msg: impl Into<String>) -> XplError {
        XplError::Trappable { code: code as u32, msg: msg.into() }
    }
    /// The human-readable message, regardless of tier.
    pub fn message(&self) -> &str {
        match self {
            XplError::Trappable { msg, .. } | XplError::Fatal(msg) => msg,
        }
    }
    /// The `$ERROR` code this error reports (`0` for a fatal, which never
    /// folds into `$ERROR`).
    pub fn code(&self) -> u32 {
        match self {
            XplError::Trappable { code, .. } => *code,
            XplError::Fatal(_) => 0,
        }
    }
    /// Whether this is a fatal (script-aborting) error.
    pub fn is_fatal(&self) -> bool {
        matches!(self, XplError::Fatal(_))
    }
}

/// Result of a host/verb operation.
pub type VmResult<T> = Result<T, XplError>;
