//! borgchart internals, shared by the `brg` and `borgchart` binaries.
//!
//! This crate is not a public library: nothing here is promised stable.

pub mod cli;
pub mod deploy;
pub mod diag;
pub mod exec;
pub mod frontmatter;
pub mod gate;
pub mod launch;
pub mod names;
pub mod paths;
pub mod rules;
pub mod runtime;
pub mod state;
pub mod team;
pub mod tmux;
pub mod validate;
pub mod yaml;
