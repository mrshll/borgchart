//! An isolated plugin process and recording gh executable; never a network call.
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
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
        this.write("github.yaml", "owner: example-owner\ngh_config_dir: ~/machine-profile\nrepositories: [example-org/widgets]\n");
        this.write("gh", r#"#!/bin/sh
printf '%s\n' "$@" >> "$FIXTURE_DIR/calls"
printf '%s\n' '--end--' >> "$FIXTURE_DIR/calls"
printf '%s\n' "${GH_TOKEN-unset}" "${GITHUB_TOKEN-unset}" "${GH_ENTERPRISE_TOKEN-unset}" "${GITHUB_ENTERPRISE_TOKEN-unset}" "${GH_HOST-unset}" "${GH_REPO-unset}" "${GH_DEBUG-unset}" > "$FIXTURE_DIR/environment"
printf '%s' "$GH_CONFIG_DIR" > "$FIXTURE_DIR/config-dir"
response=unexpected
number=
after=
for arg do
  case "$arg" in
    user) response=machine ;;
    users/*) response=owner ;;
    repos/*|orgs/*)
      response=issues
      since=${arg#*since=}; since=${since%%&*}
      if [ -f "$FIXTURE_DIR/issues-split" ] && [ "$since" \> "$(/bin/cat "$FIXTURE_DIR/issues-split")" ]; then response=issues-after; fi ;;
    graphql) response=graphql ;;
    query=mutation*) response=reaction ;;
    number=*) number=${arg#number=} ;;
    after=*) after=${arg#after=} ;;
  esac
done
if [ "$response" = graphql ] && [ -n "$number" ] && [ -f "$FIXTURE_DIR/graphql-$number${after:+-$after}" ]; then
  response=graphql-$number${after:+-$after}
fi
/bin/cat "$FIXTURE_DIR/$response"
exit "$(/bin/cat "$FIXTURE_DIR/$response-code")"
"#);
        fs::set_permissions(this.path("gh"), fs::Permissions::from_mode(0o755)).unwrap();
        this.response("machine", 200, "{\"id\":101,\"login\":\"example-bot\"}");
        this.response("owner", 200, "{\"id\":202,\"login\":\"example-owner\"}");
        this.response("issues", 200, "[]");
        this.response(
            "reaction",
            200,
            r#"{"data":{"addReaction":{"reaction":{"content":"EYES"}}}}"#,
        );
        this.response(
            "graphql",
            200,
            "{\"data\":{\"viewer\":{\"databaseId\":101}}}",
        );
        this
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    pub fn write(&self, name: &str, text: &str) {
        fs::write(self.path(name), text).unwrap();
    }
    pub fn response(&self, name: &str, status: u16, body: &str) {
        self.write(
            name,
            &format!("HTTP/2.0 {status} status\r\nContent-Type: application/json\r\n\r\n{body}"),
        );
        self.write(
            &format!("{name}-code"),
            if status >= 400 { "1" } else { "0" },
        );
    }
    pub fn environment(&self, command: &mut Command) {
        command
            .env_clear()
            .current_dir(self.dir.path())
            .env("PATH", self.dir.path())
            .env("HOME", self.dir.path())
            .env("FIXTURE_DIR", self.dir.path())
            .env("BORGCHART_STATE_DIR", self.path("state"))
            .env("GH_CONFIG_DIR", "ignored-inherited-profile")
            .env("BORGCHART_CONFIG", self.path("config.yaml"));
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brg-github"));
        command.args(["run", "--once"]);
        self.environment(&mut command);
        command
    }
    pub fn run(&self) -> Output {
        self.command().output().unwrap()
    }
    pub fn health(&self) -> Value {
        self.json("state/health.json")
    }
    pub fn detail(&self) -> String {
        self.health()["detail"].as_str().unwrap().into()
    }
    pub fn json(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(name)).unwrap()).unwrap()
    }
    /// A fixture file's text, empty when it was never written.
    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }
    pub fn calls(&self) -> String {
        self.read("calls")
    }
}
