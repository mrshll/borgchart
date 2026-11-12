//! Slack channel intake, bounded ID notices and guarded writes.
pub mod api;
pub mod watch;
pub mod writes;
use std::fmt;

#[derive(Debug)]
pub enum Error {
    Usage(String),
    Failure(String),
}
impl Error {
    pub fn usage(text: impl Into<String>) -> Self {
        Self::Usage(text.into())
    }
    pub fn failure(text: impl Into<String>) -> Self {
        Self::Failure(text.into())
    }
    pub fn code(&self) -> i32 {
        if matches!(self, Self::Usage(_)) { 2 } else { 1 }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(s) | Self::Failure(s) => f.write_str(s),
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::failure("cannot access plugin state; check its permissions")
    }
}
pub type Result<T> = std::result::Result<T, Error>;
