//! Driver-agnostic error model. Raw driver errors never cross the wire;
//! the trait boundary translates them into [`DriverError`] carrying a
//! stable [`Code`] (ADR-004).

use crate::Engine;
use serde::{Deserialize, Serialize};

/// Stable error codes. Grows as implementation surfaces real cases; existing
/// codes never change meaning. Wire-stable from v0.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Code {
    #[error("connection failed")]
    ConnectionFailed,

    #[error("authentication failed")]
    AuthFailed,

    #[error("query timed out")]
    QueryTimedOut,

    #[error("query was canceled")]
    QueryCanceled,

    #[error("syntax error")]
    SyntaxError,

    #[error("undefined object")]
    UndefinedObject,

    #[error("duplicate object")]
    DuplicateObject,

    #[error("invalid parameter value")]
    InvalidParameterValue,

    #[error("operation not supported on this engine")]
    UnsupportedForEngine,

    #[error("connection pool exhausted")]
    PoolExhausted,

    #[error("cursor not found")]
    CursorNotFound,

    #[error("transaction not found")]
    TransactionNotFound,

    #[error("driver internal error")]
    DriverInternal,

    #[error("other: {message}")]
    Other { message: String },
}

/// Error returned by every [`crate::Driver`] method. Serializes flat for the
/// wire; never carries raw driver strings (those go into `message` cleaned,
/// or into `engine_sqlstate` for codes that map cleanly).
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct DriverError {
    pub code: Code,
    pub message: String,
    pub engine: Option<Engine>,
    /// PG SQLSTATE (5-char) or tiberius error number, when available. Used by
    /// clients that want to dispatch on the engine's native classification
    /// without re-parsing `message`.
    pub engine_sqlstate: Option<String>,
}

impl DriverError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            engine: None,
            engine_sqlstate: None,
        }
    }

    pub fn with_engine(mut self, engine: Engine) -> Self {
        self.engine = Some(engine);
        self
    }

    pub fn with_sqlstate(mut self, sqlstate: impl Into<String>) -> Self {
        self.engine_sqlstate = Some(sqlstate.into());
        self
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::new(Code::ConnectionFailed, e.to_string())
    }
}

/// Non-fatal advisory carried alongside a result stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverWarning {
    pub message: String,
    /// Engine-specific code if any (PG SQLSTATE, tiberius error number).
    pub code: Option<String>,
}

impl DriverWarning {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
}
