//! Codex model tier aliases resolved against the vendor's visible picker.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::bounded_process::{Error, Limits};
use crate::vendor_protocol::{self, MAX_OUTPUT, Query};

pub const TIERS: [&str; 4] = ["astra", "sol", "terra", "luna"];

pub fn alias(name: &str) -> bool {
    TIERS.contains(&name)
}

fn version(slug: &str) -> Option<(&str, Vec<u32>)> {
    let (version, tier) = slug.strip_prefix("gpt-")?.split_once('-')?;
    if slug.len() > 128 || !alias(tier) || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    let numbers = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    Some((tier, numbers))
}

pub fn is_tier_slug(slug: &str) -> bool {
    version(slug).is_some()
}

pub fn newest<'a>(model: &str, listed: &'a [String]) -> Option<&'a str> {
    let (tier, pinned) = if alias(model) {
        (model, Vec::new())
    } else {
        version(model)?
    };
    listed
        .iter()
        .filter_map(|slug| {
            let (class, number) = version(slug)?;
            (class == tier && number > pinned).then_some((number, slug.as_str()))
        })
        .max()
        .map(|(_, slug)| slug)
}

pub fn listed(executable: &Path, env: &BTreeMap<String, String>) -> Result<Vec<String>, Error> {
    // The picker runs before member restrictions, outside project configuration.
    let mut command = Command::new(executable);
    command
        .args(["app-server", "--stdio"])
        .env_clear()
        .envs(env.iter().filter(|(key, _)| !key.starts_with("BORGCHART_")))
        .current_dir("/");
    let list = vendor_protocol::exchange(
        Query::CodexModels,
        &mut command,
        Limits {
            deadline: Instant::now() + Duration::from_secs(8),
            stdout_bytes: MAX_OUTPUT,
            stderr_bytes: MAX_OUTPUT,
        },
    )?;
    serde_json::from_value(list).map_err(|_| Error::Io(std::io::ErrorKind::InvalidData))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_or_unversioned_models_cannot_supersede_a_pin() {
        let models = [
            "gpt-9-other",
            "gpt-next-astra",
            "gpt-7-sol",
            "gpt-6.10-astra",
        ]
        .map(str::to_owned);
        assert_eq!(newest("gpt-6.2-astra", &models), Some("gpt-6.10-astra"));
        assert_eq!(newest("other", &models), None);
        assert_eq!(newest("gpt-7-astra", &models), None);
    }
}
