//! Test-only exact protocol stand-ins. No release binary uses these controls.
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub fn run(args: &[String]) -> bool {
    let provider = if args == ["app-server", "--stdio"] {
        "codex"
    } else if args
        == [
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
        ]
    {
        "claude"
    } else {
        return false;
    };
    let root =
        PathBuf::from(std::env::var_os("HOME").expect("isolated fake home")).join("fake-usage");
    fs::create_dir_all(&root).unwrap();
    let control: Value = fs::read(root.join(format!("{provider}.json")))
        .ok()
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap_or_else(|| json!({}));
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(format!("{provider}.jsonl")))
        .unwrap();
    writeln!(log, "{}", json!({"event":"start","argv":args,"env":std::env::vars().collect::<std::collections::BTreeMap<_,_>>()})).unwrap();
    if control["mode"] == "exit" {
        eprintln!("DO_NOT_LOG_VENDOR_ERROR");
        return true;
    }
    if control["mode"] == "stall" {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return true;
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut read = || {
        let request: Value =
            serde_json::from_str(&lines.next().expect("request").unwrap()).unwrap();
        writeln!(log, "{}", json!({"event":"request","request":request})).unwrap();
        request
    };
    let init = read();
    if provider == "codex" {
        assert_eq!(init["id"], 1);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["capabilities"]["experimentalApi"], true);
        println!("{}", json!({"id":1,"result":{}}));
    } else {
        assert_eq!(init["request_id"], "init");
        assert_eq!(init["request"]["subtype"], "initialize");
        println!(
            "{}",
            json!({"type":"control_response","response":{"request_id":"init","response":{}}})
        );
    }
    std::io::stdout().flush().unwrap();
    if provider == "codex" {
        assert_eq!(read()["method"], "initialized");
    }
    let usage = read();
    if provider == "codex" {
        assert_eq!(usage["id"], 2);
        assert_eq!(usage["method"], "account/rateLimits/read");
    } else {
        assert_eq!(usage["request_id"], "usage");
        assert_eq!(usage["request"]["subtype"], "get_usage");
        assert_eq!(usage["request"]["skip_behaviors"], true);
    }
    if control["mode"] == "malformed" {
        println!("DO_NOT_LOG_RESPONSE");
        return true;
    }
    if control["mode"] == "rotate" {
        let home = root.parent().unwrap();
        let credential = if provider == "codex" {
            home.join(".codex/auth.json")
        } else {
            home.join(".claude/.credentials.json")
        };
        fs::write(credential, "DO_NOT_READ_FAKE_ROTATED_CREDENTIAL").unwrap();
    }
    let now = std::time::SystemTime::now();
    let reset = now + std::time::Duration::from_secs(3600);
    let response = control.get("response").cloned().unwrap_or_else(|| if provider == "codex" {
        json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{"usedPercent":25,"resetsAt":reset.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),"windowDurationMins":300}}})
    } else {
        json!({"subscription_type":"max","rate_limits_available":true,"rate_limits":{"seven_day_opus":null,"seven_day_sonnet":null,"five_hour":{"utilization":25,"resets_at":borgchart::bus::time::format_utc(reset)},"seven_day":{"utilization":25,"resets_at":borgchart::bus::time::format_utc(reset)},"limits":[{"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}},"percent":25,"resets_at":borgchart::bus::time::format_utc(reset)}]}})
    });
    println!(
        "{}",
        if provider == "codex" {
            json!({"id":2,"result":response})
        } else {
            json!({"type":"control_response","response":{"request_id":"usage","response":response}})
        }
    );
    true
}
