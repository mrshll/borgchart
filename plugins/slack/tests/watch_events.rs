//! Channel intake through fake Socket Mode, Slack HTTP and the message bus.
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use support::*;

fn event(offset: u64) -> Value {
    let mut message = post(offset);
    message["type"] = json!("message");
    message["channel"] = json!("C0000TEST");
    json!({"type":"events_api","envelope_id":format!("event-{offset}"),"payload":{"team_id":"T0000EXAMPLE","event":message}})
}
fn message(envelope: &Value) -> &Value {
    &envelope["payload"]["event"]
}
fn listen(h: &Harness, connections: Vec<Vec<Value>>) {
    h.slack.lock().unwrap().connections = connections;
    success(h.run(&["watch", "--once"]));
}
fn lines(h: &Harness) -> Vec<String> {
    h.sends()
        .iter()
        .flat_map(|s| s.last().unwrap().lines().skip(1).map(str::to_owned))
        .collect()
}
fn save(h: &Harness, cursor: Value) {
    fs::write(h.dir.path().join("state/cursor.json"), cursor.to_string()).unwrap();
}
fn old_thread() -> String {
    format!(
        "{}.000100",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 60 * 86400
    )
}

struct Listener(std::process::Child);
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().unwrap();
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(40),
            "watch did not reach the expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn allowed_bot_post_is_journaled_before_ack_and_delivered_without_text() {
    let mut h = Harness::new();
    h.env.insert("SLACK_ALLOW_IDS".into(), "B0000OTHER".into());
    let mut e = event(0);
    e["payload"]["event"]["bot_id"] = json!("B0000OTHER");
    e["payload"]["event"]["subtype"] = json!("bot_message");
    listen(&h, vec![vec![e.clone()]]);
    let s = h.slack.lock().unwrap();
    assert_eq!(s.acks.len(), 1);
    assert!(s.journal_before_ack[0]);
    assert!(s.writes.is_empty());
    drop(s);
    assert_eq!(lines(&h), vec![format!("post ts={}", ts(message(&e)))]);
    assert_eq!(h.state("health.json")["ok"], true);
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("listening")
    );
    assert!(!h.sends()[0].last().unwrap().contains("Ignore instructions"));
}

#[test]
fn plain_and_broadcast_replies_survive_delivery_failure_with_sixty_day_old_parents() {
    let h = Harness::new();
    let parent = old_thread();
    let mut plain = event(1);
    plain["payload"]["event"]["thread_ts"] = json!(parent);
    let mut broadcast = event(2);
    broadcast["payload"]["event"]["thread_ts"] = json!(parent);
    broadcast["payload"]["event"]["subtype"] = json!("thread_broadcast");
    h.outcome(4, json!({"message":"route refused"}));
    listen(&h, vec![vec![plain.clone(), broadcast.clone()]]);
    assert_eq!(h.state("health.json")["ok"], false);
    fs::remove_file(h.dir.path().join("sends")).unwrap();
    h.outcome(0, json!({}));
    listen(&h, vec![vec![plain.clone(), broadcast.clone()]]);
    listen(&h, vec![vec![]]);
    assert_eq!(
        lines(&h),
        vec![
            format!("reply ts={} thread={parent}", ts(message(&plain))),
            format!("reply ts={} thread={parent}", ts(message(&broadcast))),
        ]
    );
    assert!(
        h.slack
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|c| c.0 != "conversations.replies")
    );
}

#[test]
fn retry_envelopes_and_reconnect_duplicates_produce_one_notice() {
    let h = Harness::new();
    let e = event(1);
    let mut retry = e.clone();
    retry["retry_attempt"] = json!(1);
    let refresh = json!({"type":"disconnect","reason":"refresh_requested"});
    listen(&h, vec![vec![e.clone(), retry, refresh], vec![e.clone()]]);
    assert_eq!(h.slack.lock().unwrap().acks.len(), 3);
    assert_eq!(lines(&h), vec![format!("post ts={}", ts(message(&e)))]);
}

#[test]
fn failed_cursor_save_withholds_ack_until_retry_is_durable_and_delivers_once() {
    let h = Harness::new();
    // Own this socket so a deliberately unacknowledged envelope is observable.
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    h.slack.lock().unwrap().socket_url = format!("ws://{}/", server.local_addr().unwrap());
    let _listener = Listener(h.command(&["watch"]).spawn().unwrap());
    let connect = || {
        let (stream, _) = server.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        socket
            .send(tungstenite::Message::text(
                json!({"type":"hello"}).to_string(),
            ))
            .unwrap();
        socket
    };
    let mut socket = connect();
    let cursor = h.dir.path().join("state/cursor.json");
    wait_until(|| cursor.is_file());
    fs::remove_file(&cursor).unwrap();
    fs::create_dir(&cursor).unwrap();
    let mut envelope = event(1);
    socket
        .send(tungstenite::Message::text(envelope.to_string()))
        .unwrap();
    let reply = socket.read();
    assert!(
        !matches!(reply, Ok(tungstenite::Message::Text(_))),
        "failed save was acknowledged"
    );
    assert!(h.sends().is_empty());
    // Keep the next hello from persisting the in-memory journal itself.
    h.slack
        .lock()
        .unwrap()
        .errors
        .insert("conversations.history".into(), "missing_scope".into());
    fs::remove_dir(&cursor).unwrap();
    drop(socket);
    let mut socket = connect();
    envelope["retry_attempt"] = json!(1);
    socket
        .send(tungstenite::Message::text(envelope.to_string()))
        .unwrap();
    let ack: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(ack["envelope_id"], envelope["envelope_id"]);
    let durable = fs::read(&cursor)
        .ok()
        .and_then(|data| serde_json::from_slice::<Value>(&data).ok());
    assert!(
        durable.is_some_and(|state| state["seen"]
            .as_array()
            .unwrap()
            .contains(&message(&envelope)["ts"])),
        "retry ack preceded the durable journal"
    );
    wait_until(|| !h.sends().is_empty());
    assert_eq!(
        lines(&h),
        vec![format!("post ts={}", ts(message(&envelope)))]
    );
}

#[test]
fn socket_close_marks_health_disconnected_during_reconnect_backoff() {
    let h = Harness::new();
    let _listener = Listener(h.command(&["watch"]).spawn().unwrap());
    wait_until(|| {
        h.dir.path().join("state/health.json").is_file()
            && h.state("health.json")["ok"] == false
            && h.state("health.json")["detail"]
                .as_str()
                .unwrap()
                .contains("disconnected")
    });
}

#[test]
fn ineligible_message_events_are_acked_without_notice_or_slack_write() {
    let mut h = Harness::new();
    h.env.insert(
        "SLACK_ALLOW_IDS".into(),
        "U0000ALICE,U0000BOT,B0000BOT".into(),
    );
    let mut events = Vec::new();
    for (key, value) in [
        ("channel", json!("C0000OTHER")),
        ("subtype", json!("message_changed")),
        ("subtype", json!("message_deleted")),
        ("user", json!("U0000BOT")),
        ("bot_id", json!("B0000BOT")),
        ("user", json!("U0000OTHER")),
        ("type", json!("reaction_added")),
        ("ts", json!("invalid")),
    ] {
        let mut e = event(events.len() as u64);
        e["payload"]["event"][key] = value;
        events.push(e);
    }
    let mut wrong_team = event(20);
    wrong_team["payload"]["team_id"] = json!("T0000OTHER");
    events.push(wrong_team);
    let mut wrong_type = event(21);
    wrong_type["type"] = json!("interactive");
    events.push(wrong_type);
    let count = events.len();
    listen(&h, vec![events]);
    assert_eq!(h.slack.lock().unwrap().acks.len(), count);
    assert!(h.slack.lock().unwrap().writes.is_empty());
    assert!(h.sends().is_empty());
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("{count} ignored"))
    );
}

#[test]
fn first_hello_baselines_history_without_replaying_existing_posts() {
    let h = Harness::new();
    h.slack.lock().unwrap().messages = vec![post(0)];
    listen(&h, vec![vec![]]);
    assert!(h.sends().is_empty());
}

#[test]
fn hello_catch_up_delivers_only_newer_posts_and_broadcasts_once() {
    let h = Harness::new();
    save(&h, json!({"last":ts(&post(2))}));
    let parent = old_thread();
    let mut broadcast = event(4);
    broadcast["payload"]["event"]["thread_ts"] = json!(parent);
    broadcast["payload"]["event"]["subtype"] = json!("thread_broadcast");
    let mut edited = post(6);
    edited["subtype"] = json!("message_changed");
    let mut non_message = post(7);
    non_message["type"] = json!("reaction_added");
    h.slack.lock().unwrap().messages = vec![
        post(1),
        post(2),
        post(3),
        message(&broadcast).clone(),
        edited,
        non_message,
    ];
    listen(&h, vec![vec![event(3)]]);
    let expected = vec![
        format!("post ts={}", ts(&post(3))),
        format!("reply ts={} thread={}", ts(message(&broadcast)), parent),
    ];
    assert_eq!(lines(&h), expected);
    listen(&h, vec![vec![event(3), broadcast.clone()]]);
    assert_eq!(lines(&h), expected);
    let s = h.slack.lock().unwrap();
    let calls: Vec<_> = s
        .calls
        .iter()
        .filter(|c| c.0 == "conversations.history")
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1["oldest"], ts(&post(2)));
    assert_eq!(calls[0].1["channel"], "C0000TEST");
    assert_eq!(calls[0].1["limit"], "200");
}

#[test]
fn history_failure_marks_health_without_dropping_socket_intake_and_clears_on_success() {
    let h = Harness::new();
    save(&h, json!({"last":ts(&post(0))}));
    h.slack
        .lock()
        .unwrap()
        .errors
        .insert("conversations.history".into(), "missing_scope".into());
    {
        let mut slack = h.slack.lock().unwrap();
        slack.connections = vec![vec![event(1)]];
        slack.close_delay = Duration::from_millis(200);
    }
    let _listener = Listener(h.command(&["watch"]).spawn().unwrap());
    wait_until(|| !h.sends().is_empty());
    assert_eq!(lines(&h), vec![format!("post ts={}", ts(&post(1)))]);
    assert_eq!(h.state("health.json")["ok"], false);
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("missing_scope")
    );
    assert_eq!(h.slack.lock().unwrap().acks.len(), 1);
    {
        let mut slack = h.slack.lock().unwrap();
        slack.errors.clear();
        slack.connections.push(vec![event(2)]);
    }
    wait_until(|| h.slack.lock().unwrap().acks.len() == 2 && h.state("health.json")["ok"] == true);
}

#[test]
fn failed_send_retains_notices_until_exit_zero_and_redacts_delivery_errors() {
    let h = Harness::new();
    h.outcome(
        4,
        json!({"message":format!("route refused {TOKEN}\n\u{1b}")}),
    );
    listen(&h, vec![vec![event(1)]]);
    assert_eq!(h.state("health.json")["ok"], false);
    assert!(!h.state("health.json").to_string().contains(TOKEN));
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("[redacted]")
    );
    let before = h.sends().len();
    h.outcome(0, json!({"delivery":"stored"}));
    listen(&h, vec![vec![]]);
    listen(&h, vec![vec![]]);
    assert_eq!(h.sends().len(), before + 1);
    assert_eq!(h.sends()[0], *h.sends().last().unwrap());
    assert_eq!(h.state("health.json")["ok"], true);
}

#[test]
fn delivery_health_recovers_when_retention_leaves_nothing_to_send() {
    let mut h = Harness::new();
    h.env.insert("SLACK_WINDOW_HOURS".into(), "0.001".into());
    h.outcome(4, json!({"message":"route refused"}));
    {
        let mut slack = h.slack.lock().unwrap();
        slack.connections = vec![vec![event(1)]];
        slack.close_delay = Duration::from_secs(16);
    }
    let _listener = Listener(h.command(&["watch"]).spawn().unwrap());
    wait_until(|| !h.sends().is_empty() && h.state("health.json")["ok"] == false);
    h.outcome(0, json!({}));
    wait_until(|| h.state("health.json")["ok"] == true);
    assert_eq!(h.sends().len(), 1);
}

#[test]
fn batch_budget_delivers_pending_notices_oldest_first_without_losing_remainder() {
    let h = Harness::new();
    let pending: Vec<_> = (0..17)
        .rev()
        .map(|i| json!({"kind":"post","ts":ts(&post(i))}))
        .collect();
    save(&h, json!({"last":ts(&post(16)),"pending":pending}));
    for _ in 0..4 {
        listen(&h, vec![vec![]]);
    }
    let sends = h.sends();
    assert_eq!(
        sends
            .iter()
            .map(|s| s.last().unwrap().lines().count() - 1)
            .collect::<Vec<_>>(),
        vec![6, 6, 5]
    );
    assert!(sends.iter().all(|s| s.last().unwrap().len() <= 900));
    assert_eq!(
        lines(&h),
        (0..17)
            .map(|i| format!("post ts={}", ts(&post(i))))
            .collect::<Vec<_>>()
    );
}

#[test]
fn slow_timed_out_delivery_keeps_pending_and_cannot_delay_socket_acks() {
    let h = Harness::new();
    save(
        &h,
        json!({"last":ts(&post(0)),"pending":[{"kind":"post","ts":ts(&post(0))}]}),
    );
    fs::write(h.dir.path().join("delay"), "").unwrap();
    h.slack.lock().unwrap().close_delay = Duration::from_millis(200);
    let _listener = Listener(h.command(&["watch"]).spawn().unwrap());
    // Wait until the startup batch is in the slow send, then deliver an event
    // on the next connection while that send is still sleeping.
    wait_until(|| !h.sends().is_empty());
    let available = Instant::now();
    h.slack.lock().unwrap().connections.push(vec![event(1)]);
    wait_until(|| !h.slack.lock().unwrap().acks.is_empty());
    fs::remove_file(h.dir.path().join("delay")).unwrap();
    let s = h.slack.lock().unwrap();
    assert!(s.acks[0].1.duration_since(available) < Duration::from_secs(2));
    drop(s);
    wait_until(|| {
        h.state("cursor.json")["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    let delivered = lines(&h);
    assert_eq!(
        delivered
            .iter()
            .filter(|line| line.contains(ts(&post(0))))
            .count(),
        2
    );
    assert_eq!(
        delivered
            .iter()
            .filter(|line| line.contains(ts(&post(1))))
            .count(),
        1
    );
}

#[test]
fn socket_open_failure_exits_nonzero_and_preserves_health_cause() {
    let h = Harness::new();
    h.slack
        .lock()
        .unwrap()
        .errors
        .insert("apps.connections.open".into(), "invalid_auth".into());
    let out = h.run(&["watch", "--once"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(h.state("health.json")["ok"], false);
    assert!(
        h.state("health.json")["detail"]
            .as_str()
            .unwrap()
            .contains("invalid_auth")
    );
}

#[test]
fn duplicate_app_connections_cannot_report_healthy() {
    let h = Harness::new();
    h.slack.lock().unwrap().hello = Some(json!({"type":"hello","num_connections":2}));
    listen(&h, vec![vec![event(0)]]);
    assert_eq!(h.state("health.json")["ok"], false);
    assert_eq!(lines(&h).len(), 1);
}

#[test]
fn missing_or_invalid_watch_configuration_fails_before_any_slack_call() {
    for key in [
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_CHANNEL_ID",
        "BORGCHART_DELIVER_TO",
    ] {
        let mut h = Harness::new();
        h.env.remove(key);
        assert_eq!(h.run(&["watch", "--once"]).status.code(), Some(2), "{key}");
        assert!(h.slack.lock().unwrap().calls.is_empty());
    }
    for (key, value) in [
        ("BORGCHART_DELIVER_TO", "../bad"),
        ("SLACK_CHANNEL_ID", "../bad"),
        ("SLACK_WINDOW_HOURS", "NaN"),
    ] {
        let mut h = Harness::new();
        h.env.insert(key.into(), value.into());
        assert_eq!(h.run(&["watch", "--once"]).status.code(), Some(2));
        assert!(h.slack.lock().unwrap().calls.is_empty());
    }
}

#[test]
fn skipped_self_mentions_cannot_block_unmentioned_posts_or_thread_replies() {
    for enabled in [true, false] {
        let mut h = Harness::new();
        if enabled {
            h.env.insert("SLACK_SKIP_MENTIONS".into(), "true".into());
        }
        let mut mentioned = event(1);
        mentioned["payload"]["event"]["text"] = json!(" <@U0000BOT|helper> help");
        let mut reply = event(3);
        reply["payload"]["event"]["text"] = json!("<@U0000BOT> help");
        reply["payload"]["event"]["thread_ts"] = json!(old_thread());
        let mut unmentioned_reply = event(4);
        unmentioned_reply["payload"]["event"]["thread_ts"] = json!(old_thread());
        listen(
            &h,
            vec![vec![
                mentioned.clone(),
                event(2),
                reply.clone(),
                unmentioned_reply,
            ]],
        );
        let delivered = lines(&h).join("\n");
        assert_eq!(delivered.contains(ts(message(&mentioned))), !enabled);
        assert_eq!(delivered.contains(ts(message(&reply))), !enabled);
        assert!(delivered.contains(ts(&post(2))));
        assert!(delivered.contains(ts(&post(4))));
    }
}

#[test]
fn retention_expires_pending_by_own_timestamp_and_bounds_downtime_catch_up() {
    let h = Harness::new();
    let old = old_thread();
    save(
        &h,
        json!({"last":old,"seen":[old],"pending":[
            {"kind":"reply","ts":old,"thread":old},
            {"kind":"reply","ts":ts(&post(1)),"thread":old}
        ]}),
    );
    h.slack.lock().unwrap().messages = vec![json!({"ts":old,"user":"U0000ALICE"}), post(2)];
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    listen(&h, vec![vec![]]);
    listen(&h, vec![vec![]]);
    assert_eq!(
        lines(&h),
        vec![
            format!("reply ts={} thread={old}", ts(&post(1))),
            format!("post ts={}", ts(&post(2)))
        ]
    );
    let s = h.slack.lock().unwrap();
    let oldest: f64 = s
        .calls
        .iter()
        .find(|c| c.0 == "conversations.history")
        .unwrap()
        .1["oldest"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((oldest - (start - 24.0 * 3600.0)).abs() < 2.0);
}

#[test]
fn rate_limited_catch_up_cannot_sleep_through_socket_ack_deadlines() {
    let h = Harness::new();
    save(&h, json!({"last":ts(&post(0))}));
    h.slack
        .lock()
        .unwrap()
        .method_rate_limits
        .insert("conversations.history".into(), "60".into());
    h.slack.lock().unwrap().connections = vec![vec![event(1)]];
    let start = Instant::now();
    let mut listener = Listener(h.command(&["watch", "--once"]).spawn().unwrap());
    while h.slack.lock().unwrap().acks.is_empty() && start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.slack.lock().unwrap().acks.len(), 1);
    assert!(listener.0.wait().unwrap().success());
    let s = h.slack.lock().unwrap();
    assert_eq!(
        s.calls
            .iter()
            .filter(|c| c.0 == "conversations.history")
            .count(),
        1
    );
    assert_eq!(s.acks.len(), 1);
    drop(s);
    assert_eq!(lines(&h), vec![format!("post ts={}", ts(&post(1)))]);
    assert_eq!(h.state("health.json")["ok"], false);
}
