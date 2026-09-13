//! Error type shared by every codec in the crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("input is empty")]
    Empty,

    #[error("invalid JSON at line {line}, column {column}: {message}")]
    Json { message: String, line: usize, column: usize },

    #[error("invalid BSON at offset {offset:#x}: {message}")]
    Bson { message: String, offset: usize },

    #[error("input does not look like JSON or BSON")]
    Unrecognized,

    #[error("input is {size} bytes, over the {limit} byte limit")]
    TooLarge { size: usize, limit: usize },

    #[error("no such node: {0}")]
    NoSuchNode(u32),

    #[error("cannot {action} a {kind} node")]
    Unsupported { action: &'static str, kind: &'static str },

    #[error("cannot serialize: {0}")]
    Serialize(String),

    #[error("document nesting is deeper than the supported limit of {0}")]
    TooDeep(u32),

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn bson(message: impl Into<String>, offset: usize) -> Self {
        Error::Bson { message: message.into(), offset }
    }
}
