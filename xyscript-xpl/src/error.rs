//! The two-tier error model (`stage1-interpreter-plan.md` §8).
//!
//! - **Trappable** — a verb's soft failure (search-not-found, file-not-found,
//!   shell exit nonzero, …). Sets `$ERROR = 1` and *keeps running*. `$ERROR`
//!   auto-clears at each action dispatch (except an exempt verb set), so
//!   `<action>; IF $ERROR …` works. `SETERR`/`CLERR` manage it manually.
//! - **Fatal** — unknown command/label/sub, divide-by-zero, write to a
//!   read-only special, a structural/parse error. Aborts the whole script and
//!   is surfaced visibly. This is what keeps a bad key-cell from wedging the
//!   input loop.
//!
//! Trappable failures never propagate as `Err` out of the executor — they are
//! folded into `$ERROR` at the dispatch site. Only [`XplError::Fatal`] unwinds.
//! The enum is intentionally shaped so a future `TRY`/catch (trappable-fatals,
//! §12 backlog) can be added as a catch boundary without changing it.

/// An error raised while running XPL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XplError {
    /// Soft failure: set `$ERROR=1` and continue.
    Trappable(String),
    /// Hard failure: abort the script.
    Fatal(String),
}

impl XplError {
    pub fn fatal(msg: impl Into<String>) -> XplError {
        XplError::Fatal(msg.into())
    }
    pub fn trappable(msg: impl Into<String>) -> XplError {
        XplError::Trappable(msg.into())
    }
    /// The human-readable message, regardless of tier.
    pub fn message(&self) -> &str {
        match self {
            XplError::Trappable(m) | XplError::Fatal(m) => m,
        }
    }
}

/// Result of a host/verb operation.
pub type VmResult<T> = Result<T, XplError>;
