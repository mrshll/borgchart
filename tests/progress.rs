//! The single reader shared by inbox and status.
#![cfg(feature = "test-support")]
mod common;
mod instance_env;

use borgchart::bus::store;
use borgchart::state::StateDir;
use instance_env::*;
use serde_json::json;

#[test]
fn progress_counts_every_recipient_and_keeps_sequence_extrema_after_acks() {
    let env = Env::new();
    let instance = env.create("oncall", "work/a");
    env.ok(&["create", &format!("{instance}/helper")]);
    env.ok(&["create", &format!("{instance}/scout")]);
    let lead = env.member(&instance, "lead");
    let helper = env.member(&instance, "helper");
    let scout = env.member(&instance, "scout");
    let read = || {
        let bus = store::open(&StateDir::from_root(
            &env.path(&format!("state/{instance}")),
        ))
        .unwrap();
        store::progress(&bus, &store::snapshot(&bus).unwrap())
    };
    for p in read().values() {
        assert_eq!(p.unread, 0);
        assert_eq!(p.newest, 0);
        assert_eq!(p.lowest_unacked, None);
    }
    for to in [&lead, &helper, &lead] {
        let out = env.publish(&instance, &json!({"to": to, "body": "hello"}));
        assert!(out.status.success(), "{}", both(&out));
    }
    let ack = |id: &str, record: &str| {
        let start = env.starts(id).last().unwrap().clone();
        let out = env
            .cmd(&["msg", "ack", record])
            .env("BORGCHART_TEAM", &instance)
            .env("BORGCHART_MEMBER", id)
            .env(
                "BORGCHART_INCARNATION",
                start["env"]["BORGCHART_INCARNATION"].as_str().unwrap(),
            )
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", both(&out));
    };
    ack(&lead, "m1");
    let lead_address = format!("{instance}/{lead}");
    let helper_address = format!("{instance}/{helper}");
    let p = read();
    assert_eq!(
        (
            p[&lead_address].unread,
            p[&lead_address].newest,
            p[&lead_address].lowest_unacked
        ),
        (1, 3, Some(3))
    );
    assert_eq!(
        (
            p[&helper_address].unread,
            p[&helper_address].newest,
            p[&helper_address].lowest_unacked
        ),
        (1, 2, Some(2))
    );
    assert_eq!(
        p.get(&format!("{instance}/{scout}"))
            .copied()
            .unwrap_or_default()
            .unread,
        0
    );
    ack(&lead, "m3");
    ack(&helper, "m2");
    let p = read();
    assert_eq!(
        (
            p[&lead_address].unread,
            p[&lead_address].newest,
            p[&lead_address].lowest_unacked
        ),
        (0, 3, None)
    );
    assert_eq!(
        (
            p[&helper_address].unread,
            p[&helper_address].newest,
            p[&helper_address].lowest_unacked
        ),
        (0, 2, None)
    );
    env.ok(&["stop", &instance]);
}
