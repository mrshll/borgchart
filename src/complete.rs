//! Shell completion. `BRG_COMPLETE=fish brg` prints the shell's
//! registration; on each tab press the shell runs brg again with the same
//! variable and the words typed so far, and brg prints the candidates and
//! exits before anything else runs.
//!
//! Candidates come from brg's records only (the registry, instance records
//! and the configured team folders), read without a lock: a tab press
//! writes nothing, takes no lock and starts no tmux.
//!
//! This uses `clap_complete`'s dynamic engine, which sits behind its
//! `unstable-dynamic` feature (and clap's `unstable-ext`): neither is
//! covered by semver, so check completion by hand after upgrading either.

use std::ffi::OsStr;

use clap_complete::engine::{
    ArgValueCandidates, ArgValueCompleter, CompletionCandidate, PathCompleter, ValueCompleter,
};

use crate::registry;
use crate::state::StateDir;

/// The variable that turns a brg run into a completion request. brg's own,
/// so a generic `COMPLETE` left in some environment can't capture brg.
pub const ENV_COMPLETE: &str = "BRG_COMPLETE";

/// Answer a completion request and exit, or return at once when the
/// variable isn't set.
pub fn complete(command: fn() -> clap::Command) {
    clap_complete::CompleteEnv::with_factory(command)
        .var(ENV_COMPLETE)
        .complete();
}

/// Every registered instance, copies included.
pub fn instances() -> ArgValueCandidates {
    ArgValueCandidates::new(|| instance_candidates(false))
}

/// Every registered root instance (a tree's top, not a copy).
pub fn roots() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        instance_candidates(false)
            .into_iter()
            .filter(|c| !c.get_value().to_string_lossy().contains('/'))
            .collect()
    })
}

/// Every instance and every kept member, as `<instance>/<member>`.
pub fn targets() -> ArgValueCandidates {
    ArgValueCandidates::new(|| instance_candidates(true))
}

/// Every kept member, as `<instance>/<member>`.
pub fn members() -> ArgValueCandidates {
    ArgValueCandidates::new(member_candidates)
}

/// The teams in the configured search paths, or folders once the word
/// starts like a path (`/`, `.` or `~`, which no team name does).
pub fn teams() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &OsStr| {
        let typed = current.to_string_lossy();
        if typed.starts_with(['/', '.', '~']) {
            return PathCompleter::dir().complete(current);
        }
        let mut report = crate::diag::Report::default();
        let search = crate::config::load(&mut report)
            .map(|c| c.teams)
            .unwrap_or_default();
        crate::team::list(&search)
            .into_iter()
            .filter(|(name, _)| name.starts_with(&*typed))
            .map(|(name, _)| CompletionCandidate::new(name))
            .collect()
    })
}

fn instance_candidates(with_members: bool) -> Vec<CompletionCandidate> {
    let Ok(registry) = registry::load() else {
        return Vec::new();
    };
    let mut out: Vec<CompletionCandidate> = registry
        .instances
        .iter()
        .map(|(id, entry)| CompletionCandidate::new(id).help(Some(entry.team.clone().into())))
        .collect();
    if with_members {
        out.extend(member_candidates());
    }
    out
}

fn member_candidates() -> Vec<CompletionCandidate> {
    let Ok(registry) = registry::load() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in registry.instances.keys() {
        let Ok(record) = registry::read_instance(&StateDir::new(id)) else {
            continue;
        };
        for (member, issued) in record.kept_members() {
            out.push(
                CompletionCandidate::new(format!("{id}/{member}"))
                    .help(Some(issued.role.clone().into())),
            );
        }
    }
    out
}
