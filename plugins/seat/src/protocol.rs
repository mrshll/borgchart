//! Vendor protocol only: raw responses are transient and never logged.
use super::policy::Provider;
use borgchart::bounded_process::{self, Action, Error, Limits};
use serde_json::{Value, json};
use std::io;
use std::process::Command;

pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;

fn invalid() -> Error {
    Error::Io(io::ErrorKind::InvalidData)
}

struct Dialogue {
    provider: Provider,
    initialized: bool,
    buffer: Vec<u8>,
    result: Option<Value>,
}
impl Dialogue {
    fn new(provider: Provider) -> Self {
        Self {
            provider,
            initialized: false,
            buffer: Vec::new(),
            result: None,
        }
    }
    fn start(&self) -> Vec<u8> {
        encode(match self.provider {
            Provider::Codex => {
                json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"borgchart_usage","version":"1"},"capabilities":{"experimentalApi":true}}})
            }
            Provider::Claude => {
                json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize"}})
            }
        })
    }
    fn usage(&self) -> Vec<u8> {
        match self.provider {
            Provider::Codex => {
                let mut request = encode(json!({"method":"initialized"}));
                request.extend(encode(json!({"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}})));
                request
            }
            Provider::Claude => encode(
                json!({"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}}),
            ),
        }
    }
    fn advance(&mut self, bytes: &[u8]) -> Result<Action, Error> {
        self.buffer.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut next = None;
        while let Some(end) = self.buffer[consumed..].iter().position(|b| *b == b'\n') {
            let end = consumed + end;
            let data: Value =
                serde_json::from_slice(&self.buffer[consumed..end]).map_err(|_| invalid())?;
            consumed = end + 1;
            if !data.is_object() {
                return Err(invalid());
            }
            let response = match self.provider {
                Provider::Codex => {
                    let expected = if self.initialized { 2 } else { 1 };
                    if data.get("id") != Some(&json!(expected)) {
                        continue;
                    }
                    if data.get("error").is_some() {
                        return Err(invalid());
                    }
                    data.get("result")
                        .filter(|v| v.is_object())
                        .ok_or_else(invalid)?
                }
                Provider::Claude => {
                    let expected = if self.initialized { "usage" } else { "init" };
                    let Some(response) = data.get("response").filter(|r| r.is_object()) else {
                        continue;
                    };
                    if data.get("type").and_then(Value::as_str) != Some("control_response")
                        || response.get("request_id").and_then(Value::as_str) != Some(expected)
                    {
                        continue;
                    }
                    if response.get("subtype").and_then(Value::as_str) == Some("error") {
                        return Err(invalid());
                    }
                    response
                        .get("response")
                        .filter(|v| v.is_object())
                        .ok_or_else(invalid)?
                }
            };
            if self.initialized {
                // A reply arriving before its request was sent is not evidence.
                if next.is_some() {
                    return Err(invalid());
                }
                self.result = Some(response.clone());
                self.buffer.clear();
                return Ok(Action::Complete);
            }
            self.initialized = true;
            next = Some(self.usage());
        }
        self.buffer.drain(..consumed);
        Ok(next.map(Action::Send).unwrap_or(Action::Continue))
    }
}
fn encode(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).expect("fixed protocol request");
    bytes.push(b'\n');
    bytes
}

/// Authentication stays in the vendor CLI. No arbitrary parent overrides or
/// member identity reach this process, and no response/error text is logged.
pub fn read(
    provider: Provider,
    executable: &std::path::Path,
    deadline: std::time::Instant,
) -> Result<Value, Error> {
    if !executable.is_absolute() {
        return Err(invalid());
    }
    let mut command = Command::new(executable);
    match provider {
        Provider::Codex => {
            command.args(["app-server", "--stdio"]);
        }
        Provider::Claude => {
            command.args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
                "--setting-sources",
                "",
                "--settings",
                "{\"disableAllHooks\":true}",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
            ]);
        }
    }
    command
        .env_clear()
        .envs(
            borgchart::runtime::base_env()
                .into_iter()
                .filter(|(key, _)| !key.starts_with("BORGCHART_")),
        )
        .current_dir(std::env::temp_dir());
    exchange(
        provider,
        &mut command,
        Limits {
            deadline,
            stdout_bytes: MAX_OUTPUT,
            stderr_bytes: MAX_OUTPUT,
        },
    )
}
fn exchange(provider: Provider, command: &mut Command, limits: Limits) -> Result<Value, Error> {
    let mut dialogue = Dialogue::new(provider);
    bounded_process::exchange(command, &dialogue.start(), limits, |bytes| {
        dialogue.advance(bytes)
    })?;
    dialogue.result.ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(v: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&v).unwrap();
        bytes.push(b'\n');
        bytes
    }
    #[test]
    fn codex_handshake_waits_and_sends_initialized_before_usage() {
        let mut d = Dialogue::new(Provider::Codex);
        let init: Value = serde_json::from_slice(&d.start()).unwrap();
        assert_eq!(init["id"], 1);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
        assert!(matches!(
            d.advance(&line(json!({"method":"notice"}))),
            Ok(Action::Continue)
        ));
        let response = line(json!({"id":1,"result":{}}));
        for byte in &response[..response.len() - 1] {
            assert!(matches!(d.advance(&[*byte]), Ok(Action::Continue)));
        }
        let Ok(Action::Send(next)) = d.advance(b"\n") else {
            panic!("missing usage request")
        };
        let lines: Vec<Value> = next
            .split(|b| *b == b'\n')
            .filter(|b| !b.is_empty())
            .map(|b| serde_json::from_slice(b).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["method"], "initialized");
        assert_eq!(lines[1]["id"], 2);
        assert_eq!(lines[1]["method"], "account/rateLimits/read");
        assert_eq!(lines[1]["params"]["excludeResetCreditDetails"], true);
        assert!(matches!(
            d.advance(&line(json!({"id":2,"result":{"limits":[]}}))),
            Ok(Action::Complete)
        ));
        assert_eq!(d.result, Some(json!({"limits":[]})));
    }
    #[test]
    fn claude_control_handshake_and_notification_batch() {
        let mut d = Dialogue::new(Provider::Claude);
        let init: Value = serde_json::from_slice(&d.start()).unwrap();
        assert_eq!(
            init,
            json!({"type":"control_request","request_id":"init","request":{"subtype":"initialize"}})
        );
        let mut bytes = line(json!({"type":"system"}));
        bytes.extend(line(
            json!({"type":"control_response","response":{"request_id":"init","response":{}}}),
        ));
        bytes.extend(line(json!({"type":"system"})));
        let Ok(Action::Send(request)) = d.advance(&bytes) else {
            panic!("missing usage request")
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&request).unwrap(),
            json!({"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}})
        );
        assert!(matches!(d.advance(&line(json!({"type":"control_response","response":{"request_id":"usage","response":{"plan":"max"}}}))), Ok(Action::Complete)));
        assert_eq!(d.result, Some(json!({"plan":"max"})));
    }
    #[test]
    fn invalid_data_and_vendor_errors_never_escape() {
        for provider in [Provider::Claude, Provider::Codex] {
            for bad in [b"[]\n".as_slice(), b"DO_NOT_LOG\n", b"\xff\n", b"true\n"] {
                assert_eq!(Dialogue::new(provider).advance(bad).err(), Some(invalid()));
            }
            let deep = format!("{}0{}\n", "[".repeat(129), "]".repeat(129));
            assert_eq!(
                Dialogue::new(provider).advance(deep.as_bytes()).err(),
                Some(invalid())
            );
        }
        for bad in [
            json!({"id":1,"error":"DO_NOT_LOG"}),
            json!({"id":1,"result":null}),
        ] {
            assert_eq!(
                Dialogue::new(Provider::Codex).advance(&line(bad)).err(),
                Some(invalid())
            );
        }
        assert_eq!(Dialogue::new(Provider::Claude).advance(&line(json!({"type":"control_response","response":{"request_id":"init","subtype":"error","response":{"secret":"DO_NOT_LOG"}}}))).err(), Some(invalid()));
    }
}
