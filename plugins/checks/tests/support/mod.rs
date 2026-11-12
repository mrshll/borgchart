//! Isolated template folders and a bus that records only test messages.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

pub struct Fixture {
    pub dir: tempfile::TempDir,
}
impl Fixture {
    pub fn new() -> Self {
        let this = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir(this.path("state")).unwrap();
        this.write(
            "brg",
            r#"#!/bin/sh
printf '%s\0' "$@" >> "$BORGCHART_STATE_DIR/sends"
printf '\036\n' >> "$BORGCHART_STATE_DIR/sends"
[ ! -f "$BORGCHART_STATE_DIR/delay" ] || /bin/sleep 40
/bin/cat "$BORGCHART_STATE_DIR/outcome"
exit "$(/bin/cat "$BORGCHART_STATE_DIR/code")"
"#,
        );
        fs::set_permissions(this.path("brg"), fs::Permissions::from_mode(0o755)).unwrap();
        this.outcome(0, json!({"id":"m100"}));
        this.config(&["cat", "value"], "");
        this
    }
    pub fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
    pub fn write(&self, name: &str, text: &str) {
        fs::write(self.path(name), text).unwrap();
    }
    pub fn config(&self, argv: &[&str], extra: &str) {
        self.write(
            "checks.yaml",
            &format!(
                "- name: sample\n  every_minutes: 1\n  run: {}\n{extra}",
                serde_json::to_string(argv).unwrap()
            ),
        );
        self.write("value", "first\n");
    }
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brg-checks"));
        command
            .env_clear()
            .current_dir(self.dir.path())
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path())
            .env("BORGCHART_BIN", self.path("brg"))
            .env("BORGCHART_STATE_DIR", self.path("state"))
            .env("BORGCHART_DELIVER_TO", "triager-test")
            .env("BORGCHART_PARENT", "lead-test")
            .env("BORGCHART_MEMBER", "reader-test")
            .env("CHECKS_TIMEOUT_SECONDS", "0.1");
        command
    }
    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    pub fn tick(&self) {
        let out = self.run(&["run", "--once"]);
        assert!(out.status.success(), "{out:?}");
    }
    pub fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(&format!("state/{name}"))).unwrap()).unwrap()
    }
    pub fn outcome(&self, code: i32, value: Value) {
        self.write("state/code", &code.to_string());
        self.write("state/outcome", &value.to_string());
    }
    pub fn sends(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.path("state/sends"))
            .unwrap_or_default()
            .split("\u{1e}\n")
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.trim_end_matches('\0')
                    .split('\0')
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }
    pub fn last(&self) -> String {
        let out = self.run(&["last", "sample"]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    }
}
pub fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "command did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}
