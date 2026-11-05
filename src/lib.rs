//! borgchart internals, shared by the `brg` and `borgchart` binaries.
//!
//! This crate is not a public library: nothing here is promised stable.

pub mod cli;
pub mod deploy;
pub mod diag;
pub mod frontmatter;
pub mod names;
pub mod paths;
pub mod team;
pub mod validate;
pub mod yaml;
