//! Ascending issue discovery with a five-minute overlap and explicit processing commits.
use crate::{
    config::Entry,
    gh::{Api, Error, Rest},
    state::Cursor,
};
use borgchart::bus::time::parse_utc;
use serde::Deserialize;
use std::time::SystemTime;

pub const POLL_CAP: usize = 20;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub repository: String,
    pub number: u64,
    pub node_id: String,
    pub updated_at: SystemTime,
}
pub struct Batch {
    pub items: Vec<Changed>,
    pub exhausted: bool,
}
#[derive(Deserialize)]
struct RawItem {
    node_id: String,
    number: u64,
    updated_at: String,
    repository: Option<Repository>,
}
#[derive(Deserialize)]
struct Repository {
    full_name: String,
}
fn malformed() -> Error {
    Error::Other(
        "GitHub returned an invalid issue page; check gh and retry without advancing the cursor"
            .into(),
    )
}

/// No state changes until the caller marks returned items processed. A failed
/// page discards the partial batch, including when earlier pages succeeded.
pub fn fetch(api: &impl Api, entry: &Entry, cursor: &Cursor, limit: usize) -> Result<Batch, Error> {
    let since = cursor.since();
    let mut items = Vec::new();
    let mut page = 1;
    loop {
        let value = api.get(Rest::Issues { entry, since, page })?;
        let raw: Vec<RawItem> = serde_json::from_value(value).map_err(|_| malformed())?;
        let full = raw.len() == 100;
        for raw in raw {
            let updated_at = parse_utc(&raw.updated_at).ok_or_else(malformed)?;
            let repository = if entry.is_org() {
                // Validated: later stages put it into GraphQL variables.
                let repo = raw.repository.ok_or_else(malformed)?.full_name;
                Entry::parse(&repo).map_err(|_| malformed())?;
                repo
            } else {
                entry.key()
            };
            if entry.excluded.contains(&repository.to_ascii_lowercase())
                || updated_at < cursor.floor
                || cursor.seen.get(&raw.node_id) == Some(&updated_at)
            {
                continue;
            }
            items.push(Changed {
                repository,
                number: raw.number,
                node_id: raw.node_id,
                updated_at,
            });
            if items.len() == limit {
                return Ok(Batch {
                    items,
                    exhausted: false,
                });
            }
        }
        if !full {
            return Ok(Batch {
                items,
                exhausted: true,
            });
        }
        page += 1;
    }
}
