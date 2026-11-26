//! The site's first screen (`docs/index.html`) types a small team template
//! from `docs/hero-team.js`, one beat at a time. This test builds the
//! template as it stands at the start and after every beat, with the files
//! the editor never shows and the child team's template beside it, and
//! checks each stage validates with no errors or warnings, so the page
//! cannot show a file brg would refuse.

use std::fs;
use std::path::{Path, PathBuf};

use borgchart::config;
use borgchart::diag::Report;
use borgchart::validate::validate_with;
use serde_json::Value;

const DATA: &str = "docs/hero-team.js";

/// The JSON value `docs/hero-team.js` assigns to `window.HERO_TEAM`.
fn data() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DATA);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {DATA}: {e}"));
    let start = text
        .find("window.HERO_TEAM = ")
        .unwrap_or_else(|| panic!("{DATA} must assign `window.HERO_TEAM = {{…}};`"))
        + "window.HERO_TEAM = ".len();
    let body = text[start..].trim_end().trim_end_matches(';');
    serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("{DATA} is not JSON after the assignment: {e}"))
}

fn lines(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{DATA}: expected a list of lines, got {value}"))
        .iter()
        .map(|l| l.as_str().expect("a line is a string").to_string())
        .collect()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Validate the shown files as they stand, in a fresh catalog.
fn check(data: &Value, shown: &[(String, Vec<String>)], stage: &str) {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().canonicalize().unwrap().join("templates");
    let team = catalog.join(data["team"].as_str().expect("team is a name"));
    for (name, text) in data["unseen"]
        .as_object()
        .expect("unseen is a map of files")
    {
        write(&team.join(name), text.as_str().unwrap());
    }
    for (name, text) in shown {
        write(&team.join(name), &(text.join("\n") + "\n"));
    }
    for (child, files) in data["templates"].as_object().expect("templates is a map") {
        for (name, text) in files.as_object().unwrap() {
            write(&catalog.join(child).join(name), text.as_str().unwrap());
        }
    }
    // The Slack tokens are `{from:}` references, so validate wants a
    // resolver configured; it never runs one, so a made-up one will do.
    let config_file = dir.path().join("config.yaml");
    write(
        &config_file,
        &format!(
            "schema: 1\ntemplates: [{}]\nsecrets: {{resolver: [fake-resolver, '--env-file={{file}}', '--']}}\n",
            catalog.display()
        ),
    );
    let mut report = Report::default();
    let config = config::load_file(&config_file, &mut report).expect("the test's config loads");
    let (report, validated) = validate_with(&team, config);
    let problems: Vec<String> = report.diags.iter().map(|d| d.to_string()).collect();
    assert!(
        validated.is_some() && problems.is_empty(),
        "{DATA}: the template {stage} doesn't validate cleanly:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_stage_the_first_screen_types_is_a_valid_template() {
    let data = data();
    // The channel ID comes from the start environment, as the page's
    // reader would export it; without it validate warns. A stand-in.
    for name in data["environment"]
        .as_array()
        .expect("environment is a list of names")
    {
        let name = name.as_str().expect("a variable name");
        // SAFETY: this binary's only test sets these before validating, and
        // nothing else reads or writes the environment meanwhile.
        unsafe { std::env::set_var(name, "stand-in") };
    }
    let mut shown: Vec<(String, Vec<String>)> = data["start"]
        .as_object()
        .expect("start is a map of files")
        .iter()
        .map(|(name, text)| (name.clone(), lines(text)))
        .collect();
    check(&data, &shown, "at the start");
    let beats = data["beats"].as_array().expect("beats is a list");
    assert!(!beats.is_empty(), "{DATA} has no beats");
    for beat in beats {
        let name = beat["name"].as_str().expect("a beat has a name");
        for edit in beat["edits"].as_array().expect("a beat has edits") {
            let file = edit["file"].as_str().expect("an edit names its file");
            let new = lines(&edit["lines"]);
            let at = shown
                .iter()
                .position(|(n, _)| n == file)
                .unwrap_or_else(|| {
                    shown.push((file.to_string(), Vec::new()));
                    shown.len() - 1
                });
            let text = &mut shown[at].1;
            match edit["after"].as_u64() {
                Some(after) => {
                    let after = after as usize;
                    assert!(
                        after <= text.len(),
                        "{DATA}: `{name}` inserts past the end of {file}"
                    );
                    text.splice(after..after, new);
                }
                None => text.extend(new),
            }
        }
        check(&data, &shown, &format!("after `{name}`"));
    }
}
