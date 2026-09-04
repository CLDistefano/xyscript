//! XPL — the XyScript Programming Language interpreter (Stage 1).
//!
//! Pipeline (`stage1-interpreter-plan.md`): `splitter → lexer → parser →
//! structured AST → flat IR with resolved jump targets → executor`. The
//! interpreter is **headless**: it never touches the OS, the keyboard crate, or
//! a GUI. It reaches the editor, keyboard layer, shell, and clock only through
//! the [`Host`] trait (`host.rs`), which the eventual app crate implements and
//! tests stub.
//!
//! Modules are built in the plan's §13 order; each lands with green tests before
//! the next. This file re-exports the stable surface as modules come online.

pub mod ast;
pub mod env;
pub mod error;
pub mod expr;
pub mod host;
pub mod ir;
pub mod lexer;
pub mod library;
pub mod parser;
pub mod splitter;
pub mod subst;
pub mod switches;
pub mod value;
pub mod vm;

pub use ast::{Program, Span, Stmt};
pub use error::{error_message, ErrCode, VmResult, XplError, MAX_ERROR_CODE};
pub use env::SPECIAL_NAMES;
pub use expr::{eval, parse as parse_expr, EvalContext, Expr, ExprError, FUNCTION_NAMES};
pub use library::{BUILTIN_VERBS, CONTROL_KEYWORDS};
pub use host::{
    parse_mda_attr, ATTR_BOLD, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE, BinderSource, CalcContext, CaretShape, CaseKind,
    ClipKind, CmlinePlacement, ColorGround, DefineKind, DeleteKind, DirOnly, DirOpts, DirOrder,
    DocTypeReq, EditorOp, HelpResponse, Host, LintFixChoices, LockKey, MarkGutterMode, Motion, MsgColorSlot, PathPlace, PersistKind,
    PvMarginSide, SearchSpec, SortSpec, StubHost, UiSizeKind, UnderlineStyle, ViewFlag, ViewToggle,
};
pub use library::{parse_lib, LibSource, Library, Routine};
pub use parser::{parse as parse_program, ParseError};
pub use splitter::split_commands;
pub use subst::substitute;
pub use value::{fold_ident, xpl_cmp, Number, Value};
pub use vm::{InputKind, InputRequest, Interpreter, WaitTarget, PREEMPT_SLICE};
