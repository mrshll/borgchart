//! Shared Socket Mode connection, acknowledgement, health and delivery loop.
use crate::{
    Error, Result,
    api::{self, Api},
};
use borgchart::plugin::State;
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};
use tungstenite::{Message, client::IntoClientRequest};

const DELIVERY_INTERVAL: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const SILENCE_LIMIT: Duration = Duration::from_secs(90);
pub(crate) enum Event {
    Journaled,
    Duplicate,
    Ignored,
}
#[derive(Default)]
pub(crate) struct Health {
    hello: bool,
    connection: String,
    pub(crate) delivery: String,
    pub(crate) reaction: String,
    ignored: usize,
}
impl Health {
    pub(crate) fn write(&self, files: &State) -> Result<()> {
        let ok = self.connection.is_empty() && self.delivery.is_empty() && self.reaction.is_empty();
        let state = if self.connection.is_empty() {
            "listening"
        } else {
            &self.connection
        };
        let ignored = format!("{} ignored", self.ignored);
        let parts = [state, &ignored, &self.delivery, &self.reaction];
        let detail = parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>();
        files.health(ok, &api::safe(&detail.join("; ")))?;
        Ok(())
    }
}
fn connection(
    api: &Api,
    files: &State,
    event: &impl Fn(&Value) -> Result<Event>,
    hello: &impl Fn() -> Result<()>,
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
            let _ = socket.close(None);
            let _ = socket.flush();
            return Ok(true);
        }
        if envelope["type"] == "hello" {
            let mut status = health.lock().unwrap();
            status.hello = true;
            status.connection = if envelope["num_connections"].as_u64().unwrap_or(1) > 1 {
                "another Socket Mode connection is open for this app; stop the other listener"
                    .into()
            } else {
                String::new()
            };
            status.write(files)?;
            drop(status);
            hello()?;
        }
        let Some(id) = envelope["envelope_id"].as_str() else {
            continue;
        };
        let mut status = health.lock().unwrap();
        let outcome = event(&envelope)?;
        if matches!(outcome, Event::Ignored) {
            status.ignored += 1;
            status.write(files)?;
        }
        socket
            .send(Message::text(json!({"envelope_id":id}).to_string()))
            .map_err(|_| error())?;
        drop(status);
        if matches!(outcome, Event::Journaled) {
            let _ = wake.send(());
        }
    }
}
pub(crate) fn run(
    api: &Api,
    files: &State,
    once: bool,
    event: impl Fn(&Value) -> Result<Event>,
    hello: impl Fn() -> Result<()>,
    deliver: impl Fn(&Mutex<Health>) -> Result<()> + Send,
) -> Result<()> {
    let health = Mutex::new(Health {
        connection: "connecting".into(),
        ..Health::default()
    });
    std::thread::scope(|scope| {
        let (wake, receiver) = mpsc::channel();
        let health = &health;
        let worker = scope.spawn(move || {
            loop {
                if let Err(error) = deliver(health) {
                    let mut status = health.lock().unwrap();
                    status.delivery = error.to_string();
                    let _ = status.write(files);
                }
                if let Err(mpsc::RecvTimeoutError::Disconnected) =
                    receiver.recv_timeout(DELIVERY_INTERVAL)
                {
                    break;
                }
            }
        });
        let mut backoff = 1;
        let result = loop {
            health.lock().unwrap().hello = false;
            let result = connection(api, files, &event, &hello, &wake, health);
            if health.lock().unwrap().hello {
                backoff = 1;
            }
            if matches!(result, Ok(true)) {
                backoff = 1;
                continue;
            }
            if !once || result.is_err() {
                let mut status = health.lock().unwrap();
                status.connection = match &result {
                    Err(e) => format!("Socket Mode disconnected: {e}; reconnecting"),
                    Ok(_) => "Socket Mode disconnected; reconnecting".into(),
                };
                let _ = status.write(files);
            }
            if once {
                break result.map(|_| ());
            }
            std::thread::sleep(Duration::from_secs(backoff));
            backoff = (backoff * 2).min(60);
        };
        drop(wake);
        worker
            .join()
            .map_err(|_| Error::failure("delivery worker stopped; restart mentions"))?;
        result
    })
}
