//! borgchart internals, used by `brg` and `brg-seat`.
//!
//! This crate is not a public library: nothing here is promised stable.

pub mod address;
pub mod bounded_process;
pub mod bus;
pub mod checkouts;
pub mod cleanup;
pub mod cli;
pub mod complete;
pub mod composer;
pub mod config;
pub mod config_snapshot;
pub mod diag;
pub mod exec;
pub mod frontmatter;
pub mod gate;
pub mod launch;
pub mod lifecycle;
pub mod names;
pub mod nesting;
pub mod paths;
pub mod probe;
pub mod proctree;
pub mod registry;
pub mod rules;
pub mod runtime;
pub mod state;
pub mod team;
pub mod tmux;
pub mod tree;
pub mod tui;
pub mod validate;
pub mod wake;
pub mod watcher;
pub mod worktree;
pub mod yaml;
