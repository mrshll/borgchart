//! Ascending issue discovery with a five-minute overlap and explicit processing commits.
use crate::{
    config::Entry,
    gh::{Api, Error, Rest},
    state::Cursor,
};
use borgchart::bus::time::parse_utc;
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const POLL_CAP: usize = 20;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub repository: String,
    pub number: u64,
    pub node_id: String,
    pub is_pull_request: bool,
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
    pull_request: Option<serde_json::Value>,
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
    let since = cursor
        .at
        .checked_sub(Duration::from_secs(300))
        .unwrap_or(UNIX_EPOCH);
    let mut items = Vec::new();
    if limit == 0 {
        return Ok(Batch {
            items,
            exhausted: false,
        });
    }
    let mut page = 1;
    loop {
        let value = api.get(Rest::Issues { entry, since, page })?;
        let raw: Vec<RawItem> = serde_json::from_value(value).map_err(|_| malformed())?;
        let full = raw.len() == 100;
        if raw.len() > 100 {
            return Err(malformed());
        }
        for raw in raw {
            let updated_at = parse_utc(&raw.updated_at).ok_or_else(malformed)?;
            let repository = if entry.is_org() {
                let repo = raw.repository.ok_or_else(malformed)?.full_name;
                let validated = Entry::parse(&repo).map_err(|_| malformed())?;
                if validated.is_org() || !entry.contains(&validated) {
                    return Err(malformed());
                }
                repo
            } else {
                entry.key()
            };
            if raw.node_id.is_empty() || raw.number == 0 {
                return Err(malformed());
            }
            if updated_at < cursor.floor || cursor.processed.get(&raw.node_id) == Some(&updated_at)
            {
                continue;
            }
            items.push(Changed {
                repository,
                number: raw.number,
                node_id: raw.node_id,
                is_pull_request: raw.pull_request.is_some(),
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
        page = page.checked_add(1).ok_or_else(malformed)?;
    }
}
