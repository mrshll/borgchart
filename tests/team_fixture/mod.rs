//! A small valid team in a temporary directory, for the validation tests
//! (library calls, and the CLI's exit codes).
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use borgchart::config::{self, Config};
use borgchart::diag::Report;
use borgchart::validate::{Validated, validate_with};

pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    /// A small valid team: a locked triager and a lead, with a script plugin.
    pub fn new() -> Self {
        let f = Fixture {
            dir: tempfile::tempdir().unwrap(),
        };
        f.write(
            "team.yaml",
            r#"
default: claude
plugins:
  slackbot:
    path: plugins/slackbot
    process: [./watch.py]
    delivers_to: triager
    env: [SLACK_BOT_TOKEN, {SLACK_CHANNEL_ID: C0000EXAMPLE}]
    actions:
      read: slackbot read <ts>
      reply: slackbot reply <thread_ts> <text>
      escalate: slackbot escalate <ts> <severity> <reason>
members:
  triager:
    prompt: triager.md
    parent: lead
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, escalate]}
  lead:
    prompt: lead.md
    posture: locked
    untrusted_input: true
    plugins: {slackbot: [read, reply]}
"#,
        );
        f.write(
            "triager.md",
            "---\nrole: triager\nmodels: {claude: haiku}\n---\nDecide whether to escalate.\n{{includes/shared}}\n",
        );
        f.write(
            "lead.md",
            "---\nrole: lead\nmodels: {claude: opus high, codex: example-model medium}\n---\nTriage escalations.\n",
        );
        f.write("includes/shared.md", "Be brief.\n");
        f.exec("plugins/slackbot/slackbot");
        f.exec("plugins/slackbot/watch.py");
        // brg's configuration for the tests, beside the team's files.
        f.write("config.yaml", "schema: 1\n");
        f
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    pub fn exec(&self, rel: &str) {
        self.write(rel, "#!/bin/sh\necho ok\n");
        fs::set_permissions(self.path(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub fn edit(&self, rel: &str, from: &str, to: &str) {
        let text = fs::read_to_string(self.path(rel)).unwrap();
        assert!(text.contains(from), "fixture {rel} lacks {from:?}");
        self.write(rel, &text.replacen(from, to, 1));
    }

    /// Validate the team with the fixture's `config.yaml`.
    pub fn run(&self) -> (Report, Option<Validated>) {
        let mut report = Report::default();
        match config::load_file(&self.path("config.yaml"), &mut report) {
            Some(c) => validate_with(self.dir.path(), c),
            None => (report, None),
        }
    }

    /// Validate the team with the default configuration.
    pub fn run_team(&self) -> (Report, Option<Validated>) {
        validate_with(self.dir.path(), Config::default())
    }
}
