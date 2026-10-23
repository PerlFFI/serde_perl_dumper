use serde::{de, ser};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Raised by data structures through serde's `ser::Error` and
    /// `de::Error`, e.g. a missing field or an out-of-range number.
    #[error("{0}")]
    Message(String),

    /// The input isn't valid; the text includes the line and column.
    #[error("parse error: {0}")]
    Parse(String),
}

impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Error::Message(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Error::Message(msg.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
