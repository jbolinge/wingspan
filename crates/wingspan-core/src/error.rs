//! Parse errors.
//!
//! A [`ParseError`] always says *where* it happened: the file (when the input
//! came from one), the 1-based line number, and the offending token. That
//! triple is what the UI shows the user so they can fix the offending order
//! string by hand.

use std::fmt;
use std::path::{Path, PathBuf};

/// A failure to parse a TOS order string, a trade-file name or a statement row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// File the bad input came from, when it came from a file.
    pub file: Option<PathBuf>,
    /// 1-based line number within that file.
    pub line: Option<usize>,
    /// The token the parser choked on, verbatim.
    pub token: Option<String>,
    /// Human-readable explanation.
    pub message: String,
}

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            file: None,
            line: None,
            token: None,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Attach the file/line context once it is known (the tokenizer itself
    /// only sees a bare string).
    #[must_use]
    pub fn at(mut self, file: &Path, line: usize) -> Self {
        self.file = Some(file.to_path_buf());
        self.line = Some(line);
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.file, self.line) {
            (Some(p), Some(l)) => write!(f, "{}:{l}: ", p.display())?,
            (Some(p), None) => write!(f, "{}: ", p.display())?,
            _ => {}
        }
        write!(f, "{}", self.message)?;
        if let Some(t) = &self.token {
            write!(f, " (at {t:?})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}
