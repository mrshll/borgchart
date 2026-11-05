//! Errors and warnings collected while validating a team and deployment.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diag {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    pub message: String,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.level {
            Level::Error => "error",
            Level::Warning => "warning",
        };
        match &self.file {
            Some(file) => write!(f, "{level}: {}: {}", file.display(), self.message),
            None => write!(f, "{level}: {}", self.message),
        }
    }
}

/// Collects every problem found, so one run reports the whole team.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub diags: Vec<Diag>,
}

impl Report {
    pub fn error(&mut self, file: Option<&Path>, message: impl Into<String>) {
        self.push(Level::Error, file, message.into());
    }

    pub fn warning(&mut self, file: Option<&Path>, message: impl Into<String>) {
        self.push(Level::Warning, file, message.into());
    }

    fn push(&mut self, level: Level, file: Option<&Path>, message: String) {
        self.diags.push(Diag {
            level,
            file: file.map(Path::to_path_buf),
            message,
        });
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Warning)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}
