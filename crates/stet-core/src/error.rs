// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostScript error types.
//!
//! All PLRM-defined errors plus internal control flow signals.

/// PostScript error codes and internal control flow signals.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PsError {
    #[error("VMerror")]
    VMError,
    #[error("dictfull")]
    DictFull,
    #[error("dictstackoverflow")]
    DictStackOverflow,
    #[error("dictstackunderflow")]
    DictStackUnderflow,
    #[error("execstackoverflow")]
    ExecStackOverflow,
    #[error("invalidaccess")]
    InvalidAccess,
    #[error("invalidexit")]
    InvalidExit,
    #[error("invalidfileaccess")]
    InvalidFileAccess,
    #[error("invalidfont")]
    InvalidFont,
    #[error("invalidrestore")]
    InvalidRestore,
    #[error("ioerror")]
    IOError,
    #[error("limitcheck")]
    LimitCheck,
    #[error("nocurrentpoint")]
    NoCurrentPoint,
    #[error("rangecheck")]
    RangeCheck,
    #[error("stackoverflow")]
    StackOverflow,
    #[error("stackunderflow")]
    StackUnderflow,
    #[error("syntaxerror")]
    SyntaxError,
    #[error("timeout")]
    Timeout,
    #[error("typecheck")]
    TypeCheck,
    #[error("undefined")]
    Undefined,
    #[error("undefinedfilename")]
    UndefinedFilename,
    #[error("undefinedresource")]
    UndefinedResource,
    #[error("undefinedresult")]
    UndefinedResult,
    #[error("unmatchedmark")]
    UnmatchedMark,
    #[error("unregistered")]
    Unregistered,
    #[error("unsupported")]
    Unsupported,
    #[error("configurationerror")]
    ConfigurationError,

    // Internal control flow (not PostScript errors)
    #[error("quit")]
    Quit,
    #[error("stop")]
    Stop,
    #[error("exit")]
    Exit,
}
