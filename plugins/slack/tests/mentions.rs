//! Mention intake through the executable, fake Socket Mode and message bus.
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use support::*;

fn mention(offset: u64) -> Value {
    let mut event = post(offset);
    event["type"] = json!("app_mention");
    event["channel"] = json!("C0000EXAMPLE");
    event["text"] = json!("  <@U0000BOT|helper> investigate the build");
    json!({"type":"events_api","envelope_id":format!("event-{offset}"),"payload":{"team_id":"T0000EXAMPLE","event":event}})
}
fn id(envelope: &Value) -> String {
    format!(
        "C0000EXAMPLE-{}",
        envelope["payload"]["event"]["ts"].as_str().unwrap()
    )
}
fn listen(h: &Harness, connections: Vec<Vec<Value>>) {
    h.slack.lock().unwrap().connections = connections;
    success(h.run(&["mentions", "--once"]));
}
fn record(h: &Harness, event: &Value, age: u64, state: &str) {
    let event_data = &event["payload"]["event"];
    fs::write(h.dir.path().join(format!("state/req-{}.json", id(event))), json!({
        "channel":"C0000EXAMPLE", "ts":event_data["ts"], "thread":event_data["thread_ts"].as_str().unwrap_or(event_data["ts"].as_str().unwrap()),
        "user":"U0000ALICE", "text":event_data["text"], "at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()-age, "state":state
    }).to_string()).unwrap();
}

#[test]
fn leading_mention_is_journaled_before_ack_delivered_once_and_reacted_to() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::new();
    h.env.remove("SLACK_CHANNEL_ID");
    let event = mention(0);
    listen(&h, vec![vec![event.clone(), event.clone()]]);
    let s = h.slack.lock().unwrap();
    assert_eq!(s.acks.len(), 2);
    assert!(s.journal_before_ack.iter().all(|v| *v));
    assert_eq!(s.acks[0].0["envelope_id"], event["envelope_id"]);
    assert_eq!(
        s.writes,
        vec![(
            "reactions.add".into(),
            json!({"channel":"C0000EXAMPLE","timestamp":event["payload"]["event"]["ts"],"name":"eyes"})
        )]
    );
    drop(s);
    let sends = h.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(
        &sends[0][..6],
        ["msg", "send", "--json", "--external", "slack", "triager"]
    );
    let body = sends[0].last().unwrap();
    assert!(body.contains(&format!("brg-slack reply {} <text>", id(&event))));
    assert!(body.contains(&format!("brg-slack thread {}", id(&event))));
    assert!(body.contains("|   <@U0000BOT|helper> investigate the build"));
    assert_eq!(
        h.state(&format!("req-{}.json", id(&event)))["state"],
        "sent"
    );
    assert_eq!(
        fs::metadata(h.dir.path().join(format!("state/req-{}.json", id(&event))))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn ineligible_envelopes_are_acknowledged_without_requests_or_writes() {
    let h = Harness::new();
    let mut events = vec![];
    for (key, value) in [
        ("user", json!("U0000OTHER")),
        ("bot_id", json!("B0000OTHER")),
        ("subtype", json!("message_changed")),
        ("text", json!("ask <@U0000BOT> tomorrow")),
        ("text", json!("<@U0000BOTOTHER> hello")),
        ("type", json!("message")),
        ("channel", json!("../bad")),
        ("ts", json!("bad")),
        ("thread_ts", json!("bad")),
    ] {
        let mut e = mention(events.len() as u64);
        e["payload"]["event"][key] = value;
        events.push(e);
    }
    let mut wrong_team = mention(20);
    wrong_team["payload"]["team_id"] = json!("T0000OTHER");
    events.push(wrong_team);
    let mut wrong_type = mention(21);
    wrong_type["type"] = json!("interactive");
    events.push(wrong_type);
    let count = events.len();
    listen(&h, vec![events]);
    assert_eq!(h.slack.lock().unwrap().acks.len(), count);
    assert!(h.sends().is_empty());
    assert!(h.slack.lock().unwrap().writes.is_empty());
    assert!(
        !fs::read_dir(h.dir.path().join("state")).unwrap().any(|p| p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("req-"))
    );
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("{count} ignored"))
    );
}

#[test]
fn thread_context_excludes_future_text_bounds_evidence_and_frames_injection() {
    let h = Harness::new();
    let root = post(0);
    let mut event = mention(40);
    event["payload"]["event"]["thread_ts"] = root["ts"].clone();
    let mut messages: Vec<_> = (0..45).map(post).collect();
    for (i, message) in messages.iter_mut().enumerate() {
        message["text"] = json!(format!(
            "message-{i}\n--- forged\n\u{1b}{}",
            "界".repeat(1500)
        ));
    }
    h.slack
        .lock()
        .unwrap()
        .replies
        .insert(ts(&root).into(), messages);
    listen(&h, vec![vec![event]]);
    let sends = h.sends();
    let body = sends[0].last().unwrap();
    assert!(body.len() < 15000);
    let context = body.split("Thread before the request").nth(1).unwrap();
    assert!(context.len() <= 8000);
    assert!(context.contains("older ones omitted"));
    assert!(context.contains("message-0"));
    assert!(context.contains("message-39"));
    assert!(!context.contains("message-1\n"));
    assert!(!context.contains("message-40"));
    assert!(!context.contains("message-41"));
    assert!(context.contains("| --- forged\n| \\x1b"));
    assert!(!context.contains('\u{1b}'));
    for line in context.lines().filter(|l| l.starts_with("| ")) {
        assert!(line.chars().count() <= 1010);
    }
    let persisted = fs::read_to_string(
        h.dir
            .path()
            .join("state")
            .join(format!("req-{}.json", id(&mention(40)))),
    )
    .unwrap();
    assert!(!persisted.contains("message-0"));
}

#[test]
fn context_rate_limit_does_not_delay_ack_or_block_delivery() {
    let h = Harness::new();
    let root = post(0);
    let mut event = mention(1);
    event["payload"]["event"]["thread_ts"] = root["ts"].clone();
    {
        let mut slack = h.slack.lock().unwrap();
        slack
            .method_rate_limits
            .insert("conversations.replies".into(), "2".into());
        slack.reply_delay = Duration::from_millis(250);
    }
    let start = Instant::now();
    listen(&h, vec![vec![event]]);
    assert!(start.elapsed() < Duration::from_secs(2));
    let slack = h.slack.lock().unwrap();
    let context_started = slack
        .calls
        .iter()
        .find(|c| c.0 == "conversations.replies")
        .unwrap()
        .2;
    assert!(slack.acks[0].1 < context_started + Duration::from_millis(150));
    drop(slack);
    assert_eq!(h.slack.lock().unwrap().acks.len(), 1);
    assert_eq!(
        h.slack
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.0 == "conversations.replies")
            .count(),
        1
    );
    assert!(
        h.sends()[0]
            .last()
            .unwrap()
            .contains("Thread context could not be read")
    );
}

#[test]
fn pending_requests_survive_bus_failure_and_restart_without_a_new_event() {
    let h = Harness::new();
    let event = mention(0);
    h.outcome(4, json!({"message":"route refused"}));
    listen(&h, vec![vec![event.clone()]]);
    assert_eq!(
        h.state(&format!("req-{}.json", id(&event)))["state"],
        "pending"
    );
    assert!(h.slack.lock().unwrap().writes.is_empty());
    h.outcome(0, json!({}));
    listen(&h, vec![vec![]]);
    assert_eq!(
        h.state(&format!("req-{}.json", id(&event)))["state"],
        "sent"
    );
    assert!(h.sends().len() >= 2);
    assert_eq!(h.state("health.json")["ok"], true);
    assert!(h.sends().iter().all(|s| s.last() == h.sends()[0].last()));
    assert_eq!(h.slack.lock().unwrap().writes.len(), 1);
}

#[test]
fn old_requests_fail_after_an_hour_and_expire_after_fourteen_days() {
    let h = Harness::new();
    let failed = mention(0);
    let expired = mention(1);
    record(&h, &failed, 3601, "pending");
    record(&h, &expired, 14 * 86400 + 1, "sent");
    h.outcome(5, json!({"message":"route refused"}));
    listen(&h, vec![vec![]]);
    assert_eq!(
        h.state(&format!("req-{}.json", id(&failed)))["state"],
        "failed"
    );
    assert!(
        !h.dir
            .path()
            .join(format!("state/req-{}.json", id(&expired)))
            .exists()
    );
    assert_eq!(h.state("health.json")["ok"], false);
}

#[test]
fn disconnect_refresh_opens_a_new_connection_even_in_once_mode() {
    let h = Harness::new();
    listen(
        &h,
        vec![
            vec![json!({"type":"disconnect","reason":"refresh_requested"})],
            vec![mention(0)],
        ],
    );
    assert_eq!(
        h.slack
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.0 == "apps.connections.open")
            .count(),
        2
    );
    assert_eq!(h.sends().len(), 1);
}

#[test]
fn request_actions_remain_in_the_recorded_channel_and_thread() {
    let mut h = Harness::new();
    h.env.remove("SLACK_CHANNEL_ID");
    let root = post(0);
    let mut event = mention(1);
    event["payload"]["event"]["thread_ts"] = root["ts"].clone();
    record(&h, &event, 0, "sent");
    h.slack
        .lock()
        .unwrap()
        .replies
        .insert(ts(&root).into(), vec![root.clone()]);
    success(h.run(&["thread", &id(&event)]));
    success(h.run(&["reply", &id(&event), "Investigating"]));
    success(h.run(&["react", &id(&event), "done"]));
    let s = h.slack.lock().unwrap();
    assert!(s.calls.iter().all(|c| c.1["channel"] == "C0000EXAMPLE"));
    assert_eq!(s.calls[0].1["ts"], root["ts"]);
    assert_eq!(s.writes[0].1["thread_ts"], root["ts"]);
    assert_eq!(s.writes[1].1["timestamp"], event["payload"]["event"]["ts"]);
    drop(s);
    let before = h.slack.lock().unwrap().calls.len();
    let expired = mention(5);
    record(&h, &expired, 14 * 86400 + 1, "sent");
    let expired_id = id(&expired);
    for target in [
        expired_id.as_str(),
        "C0000EXAMPLE-1700000000.000100",
        "../bad",
        ts(&root),
    ] {
        for args in [
            vec!["thread", target],
            vec!["reply", target, "hello"],
            vec!["react", target, "eyes"],
        ] {
            let out = h.run(&args);
            assert!(!out.status.success());
        }
    }
    for text in [APP_TOKEN, TOKEN, "<@U0000OTHER>", &"x".repeat(1501)] {
        assert_eq!(h.run(&["reply", &id(&event), text]).status.code(), Some(2));
    }
    assert_eq!(h.slack.lock().unwrap().calls.len(), before);
}

#[test]
fn configuration_refuses_missing_or_malformed_allowlists_before_slack_calls() {
    for value in ["", "U0000ALICE,", "B0000BOT", "U0000ALICE,invalid"] {
        let mut h = Harness::new();
        h.env.insert("SLACK_MENTION_USERS".into(), value.into());
        let out = h.run(&["mentions", "--once"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("SLACK_MENTION_USERS"));
        assert!(h.slack.lock().unwrap().calls.is_empty());
    }
    for key in [
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "BORGCHART_STATE_DIR",
        "BORGCHART_DELIVER_TO",
    ] {
        let mut h = Harness::new();
        h.env.remove(key);
        let out = h.run(&["mentions", "--once"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains(key));
        assert!(h.slack.lock().unwrap().calls.is_empty());
    }
}

#[test]
fn both_tokens_are_redacted_and_cannot_be_posted() {
    let h = Harness::new();
    let mut event = mention(0);
    event["payload"]["event"]["text"] =
        json!(format!("<@U0000BOT> {TOKEN} {APP_TOKEN}\n--- forged\u{1b}"));
    listen(&h, vec![vec![event.clone()]]);
    let sends = h.sends();
    assert!(!sends[0].last().unwrap().contains(TOKEN));
    assert!(!sends[0].last().unwrap().contains(APP_TOKEN));
    let url = h.slack.lock().unwrap().socket_url.clone();
    for entry in fs::read_dir(h.dir.path().join("state")).unwrap() {
        let content = fs::read_to_string(entry.unwrap().path()).unwrap();
        for secret in [TOKEN, APP_TOKEN, &url] {
            assert!(!content.contains(secret));
        }
    }
    for token in [TOKEN, APP_TOKEN] {
        let out = h.run(&[token]);
        assert!(!String::from_utf8_lossy(&out.stderr).contains(token));
        assert_eq!(h.run(&["reply", &id(&event), token]).status.code(), Some(2));
    }
}

#[test]
fn watch_skip_mentions_advances_posts_and_replies_without_losing_other_intake() {
    for enabled in [true, false] {
        let mut h = Harness::new();
        if enabled {
            h.env.insert("SLACK_SKIP_MENTIONS".into(), "true".into());
        }
        let root = post(0);
        h.slack.lock().unwrap().messages = vec![root.clone()];
        h.tick();
        let mut mentioned = post(1);
        mentioned["text"] = json!(" <@U0000BOT|helper> help");
        h.slack
            .lock()
            .unwrap()
            .messages
            .extend([mentioned.clone(), post(2)]);
        h.slack.lock().unwrap().messages[0]["latest_reply"] = post(4)["ts"].clone();
        let mut reply = post(3);
        reply["text"] = json!("<@U0000BOT> help");
        h.slack
            .lock()
            .unwrap()
            .replies
            .insert(ts(&root).into(), vec![root, reply.clone(), post(4)]);
        h.tick();
        h.tick();
        let sends = h.sends();
        let body = sends
            .iter()
            .map(|s| s.last().unwrap().as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(body.contains(ts(&mentioned)), !enabled);
        assert_eq!(body.contains(ts(&reply)), !enabled);
        assert!(body.contains(ts(&post(2))));
        assert!(body.contains(ts(&post(4))));
        assert_eq!(sends.len(), 1);
    }
}

#[test]
fn partial_thread_page_warns_that_later_replies_may_be_missing() {
    let h = Harness::new();
    let root = post(0);
    let mut event = mention(2);
    event["payload"]["event"]["thread_ts"] = root["ts"].clone();
    {
        let mut slack = h.slack.lock().unwrap();
        slack.partial_replies = true;
        slack
            .replies
            .insert(ts(&root).into(), vec![root.clone(), post(1)]);
    }
    listen(&h, vec![vec![event]]);
    let sends = h.sends();
    let body = sends[0].last().unwrap();
    assert!(body.contains(
        "partial: thread is longer than one page; the latest earlier replies may be missing"
    ));
}

#[test]
fn duplicate_socket_connections_report_unhealthy_until_the_other_listener_stops() {
    let h = Harness::new();
    h.slack.lock().unwrap().hello = Some(json!({"type":"hello", "num_connections":2}));
    listen(&h, vec![vec![mention(0)]]);
    assert_eq!(h.state("health.json")["ok"], false);
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("stop the other listener")
    );
    assert_eq!(h.sends().len(), 1);
}

#[test]
fn framing_newline_heavy_requests_still_fits_the_bus_byte_budget() {
    for text in ["界".repeat(5000), "\n".repeat(5000)] {
        let h = Harness::new();
        let root = post(0);
        let mut event = mention(25);
        event["payload"]["event"]["thread_ts"] = root["ts"].clone();
        event["payload"]["event"]["text"] = json!(format!("<@U0000BOT> {text}"));
        let mut earlier = (0..25).map(post).collect::<Vec<_>>();
        for m in &mut earlier {
            m["text"] = json!("界".repeat(1000));
        }
        h.slack
            .lock()
            .unwrap()
            .replies
            .insert(ts(&root).into(), earlier);
        listen(&h, vec![vec![event.clone()]]);
        let saved = h.state(&format!("req-{}.json", id(&event)));
        assert!(saved["text"].as_str().unwrap().chars().count() <= 4000);
        assert!(saved["text"].as_str().unwrap().contains("[truncated]"));
        let sends = h.sends();
        let body = sends[0].last().unwrap();
        assert!(body.len() < 15000, "{} bytes", body.len());
        let request = body
            .split("Request (ts=")
            .nth(1)
            .unwrap()
            .split_once('\n')
            .unwrap()
            .1
            .split("Thread before the request")
            .next()
            .unwrap();
        assert!(request.lines().all(|line| line.starts_with("| ")));
    }
}

struct Listener(std::process::Child);
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().unwrap();
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "listener did not reach the expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn transient_bus_failure_recovers_health_in_the_same_listener() {
    let h = Harness::new();
    {
        let mut slack = h.slack.lock().unwrap();
        slack.connections = vec![vec![mention(0)], vec![mention(1)]];
        slack.close_delay = Duration::from_millis(200);
    }
    h.outcome(4, json!({"message":"route refused"}));
    let _listener = Listener(h.command(&["mentions"]).spawn().unwrap());
    wait_until(|| {
        h.dir.path().join("state/health.json").exists()
            && h.state("health.json")["detail"]
                .as_str()
                .unwrap()
                .contains("route refused")
    });
    h.outcome(0, json!({}));
    wait_until(|| h.sends().len() >= 3 && h.state("health.json")["ok"] == true);
    let s = h.slack.lock().unwrap();
    let opens: Vec<_> = s
        .calls
        .iter()
        .filter(|c| c.0 == "apps.connections.open")
        .collect();
    assert!(opens.len() >= 2);
    assert!(opens[1].2.duration_since(opens[0].2) >= Duration::from_secs(1));
}

#[test]
fn reaction_failure_reports_health_without_retrying_the_committed_request() {
    let h = Harness::new();
    h.slack
        .lock()
        .unwrap()
        .method_rate_limits
        .insert("reactions.add".into(), "2".into());
    let event = mention(0);
    listen(&h, vec![vec![event.clone(), event.clone()]]);
    assert_eq!(h.state("health.json")["ok"], false);
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("reactions.add")
    );
    assert_eq!(h.sends().len(), 1);
    assert_eq!(
        h.state(&format!("req-{}.json", id(&event)))["state"],
        "sent"
    );
    assert_eq!(
        h.slack
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| c.0 == "reactions.add")
            .count(),
        1
    );
}

#[test]
fn short_thread_keeps_the_root_and_nineteen_latest_earlier_replies() {
    let h = Harness::new();
    let root = post(0);
    let mut event = mention(25);
    event["payload"]["event"]["thread_ts"] = root["ts"].clone();
    let messages = (0..27)
        .map(|i| {
            let mut m = post(i);
            m["text"] = json!(format!("context-{i}"));
            m
        })
        .collect();
    h.slack
        .lock()
        .unwrap()
        .replies
        .insert(ts(&root).into(), messages);
    listen(&h, vec![vec![event]]);
    let sends = h.sends();
    let body = sends[0].last().unwrap();
    assert_eq!(body.matches("--- ts=").count(), 20);
    assert!(body.contains("| context-0\n"));
    assert!(!body.contains("| context-5\n"));
    assert!(body.contains("| context-6\n"));
    assert!(body.contains("| context-24\n"));
    assert!(!body.contains("| context-25\n"));
}

#[test]
fn pending_requests_deliver_oldest_first_even_when_files_were_created_in_reverse() {
    let h = Harness::new();
    let older = mention(0);
    let newer = mention(1);
    record(&h, &newer, 1, "pending");
    record(&h, &older, 2, "pending");
    listen(&h, vec![vec![]]);
    let sends = h.sends();
    assert_eq!(sends.len(), 2);
    assert!(sends[0].last().unwrap().contains(&id(&older)));
    assert!(sends[1].last().unwrap().contains(&id(&newer)));
}
