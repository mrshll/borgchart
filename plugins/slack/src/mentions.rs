//! Journal leading mentions before acknowledging; deliver requests off the socket thread.
use crate::{
    Error, Result,
    api::{self, Api},
};
use borgchart::plugin::{self, State};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    net::{TcpStream, ToSocketAddrs},
    sync::{Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{Message, client::IntoClientRequest};

mod deliver;
use deliver::deliver;

const DELIVERY_INTERVAL: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const SILENCE_LIMIT: Duration = Duration::from_secs(90);
const RETENTION: u64 = 14 * 86400;
#[derive(Default, Deserialize, Serialize)]
pub struct Request {
    pub channel: String,
    pub ts: String,
    pub thread: String,
    user: String,
    text: String,
    at: u64,
    state: String,
}
fn channel(value: &str) -> bool {
    value.starts_with(['C', 'G'])
        && value.len() > 1
        && value
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn filename(channel: &str, ts: &str) -> String {
    format!("req-{channel}-{ts}.json")
}
pub fn resolve(api: &Api, target: &str) -> Result<Request> {
    if let Some((c, ts)) = target
        .split_once('-')
        .filter(|(c, ts)| channel(c) && api::valid_ts(ts))
    {
        let request: Request = State::from_env()?.read_json(&filename(c, ts));
        if request.channel == c
            && request.ts == ts
            && api::valid_ts(&request.thread)
            && now().saturating_sub(request.at) <= RETENTION
        {
            return Ok(request);
        }
        return Err(Error::usage(
            "no such request; use the ID from the request message (requests expire after 14 days)",
        ));
    }
    api::check_ts(target)?;
    Ok(Request {
        channel: api.channel()?,
        ts: target.into(),
        thread: target.into(),
        ..Request::default()
    })
}
fn clipped(text: &str, chars: usize, bytes: usize) -> String {
    let mut cut = text.chars().take(chars).collect::<String>();
    let mut count = cut.chars().count();
    let mut framed_bytes = cut.len() + 2 * cut.matches('\n').count();
    if cut.len() < text.len() || framed_bytes > bytes {
        while count + 12 > chars || framed_bytes + 14 > bytes {
            let Some(c) = cut.pop() else {
                break;
            };
            count -= 1;
            framed_bytes -= c.len_utf8() + if c == '\n' { 2 } else { 0 };
        }
        cut.push_str("\n[truncated]");
    }
    cut
}
#[derive(Default)]
struct Health {
    connection: String,
    delivery: String,
    ignored: usize,
}
impl Health {
    fn write(&self, files: &State) -> Result<()> {
        let ok = self.connection.is_empty() && self.delivery.is_empty();
        files.health(
            ok,
            &api::safe(&format!(
                "{}; {} ignored{}{}",
                if self.connection.is_empty() {
                    "listening"
                } else {
                    &self.connection
                },
                self.ignored,
                if self.delivery.is_empty() { "" } else { "; " },
                self.delivery
            )),
        )?;
        Ok(())
    }
}
fn accept(
    envelope: &Value,
    bot: &str,
    team: &str,
    users: &std::collections::BTreeSet<String>,
) -> Option<Request> {
    let e = &envelope["payload"]["event"];
    let ts = e["ts"].as_str()?;
    let c = e["channel"].as_str()?;
    let user = e["user"].as_str()?;
    let text = e["text"].as_str()?;
    let thread = e["thread_ts"].as_str().unwrap_or(ts);
    if envelope["type"] != "events_api"
        || envelope["payload"]["team_id"] != team
        || e["type"] != "app_mention"
        || !users.contains(user)
        || user == bot
        || !e["bot_id"].is_null()
        || !e["subtype"].is_null()
        || !channel(c)
        || !api::valid_ts(ts)
        || !api::valid_ts(thread)
        || !api::leading_mention(text, bot)
    {
        return None;
    }
    Some(Request {
        channel: c.into(),
        ts: ts.into(),
        thread: thread.into(),
        user: user.into(),
        text: clipped(&api::safe(text), 4000, usize::MAX),
        at: now(),
        state: "pending".into(),
    })
}
fn connection(
    api: &Api,
    files: &State,
    bot: &str,
    team: &str,
    users: &std::collections::BTreeSet<String>,
    wake: &mpsc::Sender<()>,
    health: &Mutex<Health>,
) -> Result<bool> {
    let error =
        || Error::failure("Socket Mode connection failed; check connectivity and app access");
    let opened = api.call_with(
        "apps.connections.open",
        json!({}),
        true,
        &api::var("SLACK_APP_TOKEN"),
        false,
    )?;
    let url = opened["url"].as_str().ok_or_else(error)?;
    let request = url.into_client_request().map_err(|_| error())?;
    let scheme = request.uri().scheme_str().unwrap_or("");
    if scheme != "wss" && !(scheme == "ws" && std::env::var_os("SLACK_API_BASE").is_some()) {
        return Err(error());
    }
    let host = request.uri().host().ok_or_else(error)?;
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if scheme == "wss" { 443 } else { 80 });
    let address = (host, port).to_socket_addrs()?.next().ok_or_else(error)?;
    let stream = TcpStream::connect_timeout(&address, PING_INTERVAL)?;
    stream.set_read_timeout(Some(PING_INTERVAL))?;
    stream.set_write_timeout(Some(PING_INTERVAL))?;
    let (mut socket, _) = tungstenite::client_tls(request, stream).map_err(|_| error())?;
    let mut last_frame = Instant::now();
    loop {
        let frame = match socket.read() {
            Ok(frame) => frame,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) && last_frame.elapsed() < SILENCE_LIMIT =>
            {
                socket
                    .send(Message::Ping(vec![].into()))
                    .map_err(|_| error())?;
                continue;
            }
            Err(_) => return Ok(false),
        };
        last_frame = Instant::now();
        let Message::Text(text) = frame else {
            if frame.is_close() {
                return Ok(false);
            }
            socket.flush().map_err(|_| error())?;
            continue;
        };
        let envelope: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if envelope["type"] == "disconnect" {
            return Ok(true);
        }
        if envelope["type"] == "hello" {
            let mut status = health.lock().unwrap();
            status.connection = if envelope["num_connections"].as_u64().unwrap_or(1) > 1 {
                "another Socket Mode connection is open for this app; stop the other listener"
                    .into()
            } else {
                String::new()
            };
            status.write(files)?;
        }
        let Some(id) = envelope["envelope_id"].as_str() else {
            continue;
        };
        let mut status = health.lock().unwrap();
        let mut recorded = false;
        if let Some(request) = accept(&envelope, bot, team, users) {
            let name = filename(&request.channel, &request.ts);
            if !std::path::Path::new(&api::var("BORGCHART_STATE_DIR"))
                .join(&name)
                .exists()
            {
                files.write_json(&name, &request)?;
                recorded = true;
            }
        } else {
            status.ignored += 1;
            status.write(files)?;
        }
        socket
            .send(Message::text(json!({"envelope_id":id}).to_string()))
            .map_err(|_| error())?;
        drop(status);
        if recorded {
            let _ = wake.send(());
        }
    }
}
pub fn run(api: &Api, once: bool) -> Result<()> {
    for name in [
        "SLACK_APP_TOKEN",
        "SLACK_BOT_TOKEN",
        "SLACK_MENTION_USERS",
        "BORGCHART_DELIVER_TO",
        "BORGCHART_STATE_DIR",
    ] {
        if api::var(name).is_empty() {
            return Err(Error::usage(format!("set {name} before running mentions")));
        }
    }
    let allow = api::var("SLACK_MENTION_USERS");
    let user = Regex::new(r"^[UW][A-Z0-9]+$").unwrap();
    if !allow.split(',').all(|s| user.is_match(s.trim())) {
        return Err(Error::usage(
            "set SLACK_MENTION_USERS to comma-separated user IDs",
        ));
    }
    let users = api::csv(&allow);
    let target = api::var("BORGCHART_DELIVER_TO");
    if !api::valid_address(&target) {
        return Err(Error::usage("set BORGCHART_DELIVER_TO to a member ID"));
    }
    let files = State::from_env()?;
    let _lock = files.lock()?;
    let auth = loop {
        match api.call("auth.test", json!({}), false) {
            Ok(auth) => break auth,
            Err(error) if once => return Err(error),
            Err(_) => files.health(false, "cannot identify bot; check Slack access; retrying")?,
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let (bot, team) = (
        auth["user_id"].as_str().unwrap_or(""),
        auth["team_id"].as_str().unwrap_or(""),
    );
    if bot.is_empty() || team.is_empty() {
        return Err(Error::failure(
            "auth.test returned no bot or workspace identity; check Slack access",
        ));
    }
    let health = Mutex::new(Health {
        connection: "connecting".into(),
        ..Health::default()
    });
    std::thread::scope(|scope| {
        let (wake, receiver) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let (files, target, health) = (&files, &target, &health);
        let worker = scope.spawn(move || {
            let mut ready = Some(ready);
            loop {
                if let Err(error) = deliver(api, files, target, health) {
                    let mut status = health.lock().unwrap();
                    status.delivery = error.to_string();
                    let _ = status.write(files);
                }
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                if let Err(mpsc::RecvTimeoutError::Disconnected) =
                    receiver.recv_timeout(DELIVERY_INTERVAL)
                {
                    break;
                }
            }
        });
        started
            .recv()
            .map_err(|_| Error::failure("delivery worker stopped; restart mentions"))?;
        let mut backoff = 1;
        loop {
            let result = connection(api, files, bot, team, &users, &wake, health);
            if matches!(result, Ok(true)) {
                backoff = 1;
                continue;
            }
            if once {
                break;
            }
            {
                let mut status = health.lock().unwrap();
                status.connection = "Socket Mode disconnected; reconnecting".into();
                let _ = status.write(files);
            }
            std::thread::sleep(Duration::from_secs(backoff));
            backoff = (backoff * 2).min(60);
        }
        drop(wake);
        worker
            .join()
            .map_err(|_| Error::failure("delivery worker stopped; restart mentions"))
    })
}
