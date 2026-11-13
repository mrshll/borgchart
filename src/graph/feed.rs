//! Follow immutable message files without retaining their bodies.

use crate::bus::{
    layout::StorePaths,
    record::{Recipient, Record},
    store, time,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub(crate) struct Msg {
    pub id: String,
    pub seq: u64,
    pub from: String,
    pub to: String,
    pub at: SystemTime,
    pub ack_required: bool,
}
impl Msg {
    fn read(r: Record) -> Option<Self> {
        Recipient::parse(&r.from)?;
        Recipient::parse(&r.to)?;
        Some(Self {
            id: r.id,
            seq: r.seq,
            from: r.from,
            to: r.to,
            at: time::parse_utc(&r.at)?,
            ack_required: r.ack_required,
        })
    }
}
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Sent,
    Acked(Duration),
    Delivered,
}
#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub msg: Msg,
    pub kind: Kind,
    pub observed: Duration,
}
pub(crate) struct Feed {
    paths: StorePaths,
    next: u64,
    pub waiting: BTreeMap<u64, Msg>,
    pub routes: BTreeSet<(String, String)>,
    pub events: VecDeque<Event>,
}
impl Feed {
    pub fn open(paths: StorePaths) -> Result<Self, String> {
        let snap = store::snapshot_of(&paths).map_err(|e| e.message)?;
        let next = snap.records.keys().next_back().copied().unwrap_or(0) + 1;
        let waiting: BTreeMap<_, _> = snap
            .records
            .into_iter()
            .filter(|(seq, r)| r.ack_required && !snap.acks.contains_key(seq))
            .filter_map(|(seq, r)| Msg::read(r).map(|m| (seq, m)))
            .collect();
        let routes = waiting
            .values()
            .map(|m| (m.from.clone(), m.to.clone()))
            .collect();
        Ok(Self {
            paths,
            next,
            waiting,
            routes,
            events: VecDeque::new(),
        })
    }
    fn event(&mut self, msg: Msg, kind: Kind, observed: Duration) {
        self.events.push_back(Event {
            msg,
            kind,
            observed,
        });
        while self.events.len() > 64 {
            self.events.pop_front();
        }
    }
    pub fn poll(&mut self, now: Duration) -> Result<(), String> {
        while let Some(record) =
            store::next_record(&self.paths, self.next).map_err(|e| e.message)?
        {
            self.next += 1;
            let Some(msg) = Msg::read(record) else {
                continue;
            };
            self.routes.insert((msg.from.clone(), msg.to.clone()));
            self.event(msg.clone(), Kind::Sent, now);
            if msg.ack_required {
                self.waiting.insert(msg.seq, msg);
            } else {
                self.event(msg, Kind::Delivered, now);
            }
        }
        let acked: Vec<_> = self
            .waiting
            .values()
            .filter_map(|m| {
                let ack = store::ack_of(&self.paths, &m.id, &m.to)?;
                let latency = time::parse_utc(&ack.at)?
                    .duration_since(m.at)
                    .unwrap_or_default();
                Some((m.clone(), latency))
            })
            .collect();
        for (msg, latency) in acked {
            self.waiting.remove(&msg.seq);
            self.event(msg, Kind::Acked(latency), now);
        }
        Ok(())
    }
    pub fn moving(&self, now: Duration) -> bool {
        self.events.iter().any(|e| {
            now.saturating_sub(e.observed)
                < match e.kind {
                    Kind::Sent => Duration::from_millis(800),
                    Kind::Acked(_) => Duration::from_secs(2),
                    Kind::Delivered => Duration::ZERO,
                }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{context::Actor, fsx, record::Record, store::Ack, time};
    use std::time::{Duration, UNIX_EPOCH};

    pub(super) fn record(paths: &StorePaths, seq: u64, from: &str, ack_required: bool) {
        let r = Record {
            schema: crate::bus::SCHEMA,
            id: format!("m{seq}"),
            seq,
            from: from.into(),
            to: "demo/lead-abcd".into(),
            at: time::format_utc(UNIX_EPOCH + Duration::from_secs(100)),
            body: "PRIVATE-MARKER-7f49".into(),
            reply_to: vec![],
            ack_required,
        };
        fsx::write_new(&paths.record(seq), &serde_json::to_vec(&r).unwrap()).unwrap();
    }
    fn ack(paths: &StorePaths, seq: u64, by: &str) {
        let a = Ack {
            schema: crate::bus::SCHEMA,
            id: format!("m{seq}"),
            by: Actor::Member {
                member: by.into(),
                incarnation: "example.1".into(),
            },
            at: time::format_utc(UNIX_EPOCH + Duration::from_secs(141)),
        };
        fsx::write_new(&paths.ack(&a.id), &serde_json::to_vec(&a).unwrap()).unwrap();
    }
    #[test]
    fn opening_does_not_replay_history_or_keep_consumed_messages() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        record(&paths, 1, "external:owner", true);
        record(&paths, 2, "external:owner", true);
        ack(&paths, 1, "demo/lead-abcd");
        let mut feed = Feed::open(paths).unwrap();
        feed.poll(Duration::ZERO).unwrap();
        assert!(feed.events.is_empty());
        assert_eq!(feed.waiting.keys().copied().collect::<Vec<_>>(), vec![2]);
    }
    #[test]
    fn between_poll_send_and_ack_are_not_lost_or_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        record(&paths, 1, "external:owner", true);
        ack(&paths, 1, "demo/lead-abcd");
        feed.poll(Duration::from_secs(1)).unwrap();
        assert!(matches!(feed.events[0].kind, Kind::Sent));
        assert!(matches!(feed.events[1].kind, Kind::Acked(d) if d == Duration::from_secs(41)));
        assert!(feed.waiting.is_empty());
        feed.poll(Duration::from_secs(2)).unwrap();
        assert_eq!(feed.events.len(), 2);
    }
    #[test]
    fn new_record_is_announced_once_and_waits_until_its_recipient_acks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        record(&paths, 1, "external:owner", true);
        feed.poll(Duration::ZERO).unwrap();
        assert!(matches!(feed.events[0].kind, Kind::Sent));
        feed.poll(Duration::from_secs(1)).unwrap();
        assert_eq!(feed.events.len(), 1);
        assert!(feed.waiting.contains_key(&1));
        ack(&paths, 1, "demo/lead-abcd");
        feed.poll(Duration::from_secs(2)).unwrap();
        assert!(feed.waiting.is_empty());
        assert!(matches!(feed.events[1].kind, Kind::Acked(_)));
    }
    #[test]
    fn oversized_and_misnamed_acks_cannot_consume_waiting_messages() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        record(&paths, 1, "external:owner", true);
        record(&paths, 2, "external:owner", true);
        let a = Ack {
            schema: crate::bus::SCHEMA,
            id: "m9".into(),
            by: Actor::Member {
                member: "demo/lead-abcd".into(),
                incarnation: "example.1".into(),
            },
            at: time::format_utc(UNIX_EPOCH + Duration::from_secs(141)),
        };
        fsx::write_new(&paths.ack("m1"), &serde_json::to_vec(&a).unwrap()).unwrap();
        let oversized = format!(
            "{}{}",
            " ".repeat(4096),
            serde_json::to_string(&a).unwrap().replace("m9", "m2")
        );
        fsx::write_new(&paths.ack("m2"), oversized.as_bytes()).unwrap();
        feed.poll(Duration::ZERO).unwrap();
        assert_eq!(feed.waiting.len(), 2);
        assert!(feed.events.iter().all(|e| matches!(e.kind, Kind::Sent)));
    }
    #[test]
    fn forged_ack_and_terminal_addresses_cannot_change_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        record(&paths, 1, "external:owner", true);
        record(&paths, 2, "external:\x1b[31m", true);
        ack(&paths, 1, "demo/other-abcd");
        feed.poll(Duration::ZERO).unwrap();
        assert_eq!(feed.events.len(), 1);
        assert_eq!(feed.waiting.len(), 1);
        assert!(feed.waiting.contains_key(&1));
    }
    #[test]
    fn no_ack_recipient_delivers_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::from_root(dir.path());
        let mut feed = Feed::open(paths.clone()).unwrap();
        record(&paths, 1, "external:owner", false);
        feed.poll(Duration::ZERO).unwrap();
        assert!(feed.waiting.is_empty());
        assert!(matches!(feed.events[0].kind, Kind::Sent));
        assert!(matches!(feed.events[1].kind, Kind::Delivered));
    }
}
