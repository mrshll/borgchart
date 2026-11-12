# Checks timer — `brg-checks`

Run commands on an interval and wake a member when their output changes, fails,
or recovers. Notices contain check names and statuses; output stays in local
state for `last` to read as untrusted evidence. This is a Rust workspace crate;
`make install` installs it on PATH alongside `brg-slack`. To install just checks
from a checkout, use `cargo install --path plugins/checks`.

The process runs in the team's template folder. `CHECKS_FILE` defaults to
`checks.yaml`, relative to that working directory:

```yaml
- name: service-status
  run: [./read-service, --summary]
  every_minutes: 5
  ignore: '(?m)^generated-at:.*\n'
- name: revision
  run: [git, rev-parse, HEAD]
  every_minutes: 10
```

The top level is a YAML list of check mappings (`[]` for no checks). Duplicate
keys and merge keys are refused; YAML 1.1 booleans such as `yes` stay strings.
Names must be unique and
match `[a-z][a-z0-9-]{0,31}`. `run` is a non-empty string list executed without a
shell. Its first argument is a command on PATH or a relative executable containing
`/` inside the working directory; absolute paths, `..` and symlink escapes are
refused. Other arguments are passed literally. `every_minutes` must be a finite
positive number. Optional `ignore` uses Rust regex syntax: its matches are
removed before hashing stdout. Lookaround and backreferences are unsupported. Unknown keys and invalid regexes stop startup,
print one error naming the entry (parser errors name the file and source
location), and set health to not ok.

Adding a check means one list entry; a new kind of read needs an executable in
the team's folder or on PATH. Check executables inherit the process's environment
and working directory. Configure the plugin without `path:`:

```yaml
plugins:
  checks:
    process: [brg-checks, run]
    delivers_to: triager
    env:
      CHECKS_FILE: checks.yaml
    actions:
      last: brg-checks last <name>
      promote: brg-checks promote <name> <reason>
```

The process waits 30 seconds between ticks. All checks run on the first
tick after startup, then after their configured interval. Each command has a
fixed 60-second timeout. SHA-256 uses full stdout after `ignore`; saved evidence
is capped at 64 KiB UTF-8 per output. The first successful result establishes a
baseline without waking. A changed digest queues `<name> changed`. A nonzero exit,
timeout, output overflow or command launch failure queues `<name> failing` once. The next success
queues `<name> recovered`, plus `changed` if its digest differs from the last good
result. Restarting preserves the baseline, so a change while stopped wakes once.

Pending signals are stored oldest first. Each tick sends one notice to
`BORGCHART_DELIVER_TO` through the plugin message bus: at most six signal lines,
900 UTF-8 bytes and eight wrapped lines at width 78, with a header of at most 76 characters.
Exit 0 clears the batch. Any send error, including a timeout or a nonzero result
with an ID, keeps it for the next tick and marks health not ok with brg's sanitized
error. The batch stays persisted until delivery succeeds; an interrupted or failed
send can produce a duplicate notice later.

`BORGCHART_STATE_DIR` is required and holds locked, atomically replaced
`checks.json`, plus `health.json`, with one `plugin.lock`. Files are created 0600.
Newly created state folders are private; existing folder permissions are preserved.
State retains the command, last run and
success times (UTC), status, exit code, `timeout` or `output-limit`, last stderr line (up to 500
characters), current output, last good digest and previous distinct successful
output. Removed checks and their queued signals are pruned at startup. After each
check and tick, health reports `ok`, `at`, `detail`,
`pending_sends`, and per-check `status` and `last_run`. A failing check, failed
delivery or invalid configuration makes health not ok. Restart the process to
reload a changed checks file.

Actions run in the calling member's folder and use the same state directory:

- `brg-checks last <name>` reads committed evidence without waiting for a running
  check's lock. It prints an untrusted-data header, command, last run, status/exit,
  stderr, current output and previous distinct successful output. While failing,
  the previous successful evidence is the last good output. Output lines start
  with `| ` and control characters are escaped. Unknown names exit 2 and list
  known checks.
- `brg-checks promote <name> <reason>` sends the check ID and a reason
  to `BORGCHART_PARENT` with `--from BORGCHART_MEMBER`, so brg checks the parent
  route. Reasons are fixed: `outage`, `alert`, `question`, `other`. Success
  prints brg's output, including its ID, and exits 0. Any failure prints brg's
  error and exits 1; the member can retry. Promote has no retry spool, so a locked
  Codex member can't promote.

`brg-checks run --once` runs one tick, useful for testing. It exits nonzero for
invalid startup configuration; check or delivery failures appear in health.
`CHECKS_TIMEOUT_SECONDS` overrides the command timeout for tests only; production
uses the fixed 60-second default. There are no time/cron triggers. A check whose
output changes every run wakes every interval, so use `ignore` for volatile data.
Checks run one after another, so slow commands delay the next check and tick.
Each check captures at most 16 MiB of stdout and 1 MiB of stderr. Exceeding either
cap kills the process group, discards its output and records `failing / output-limit`
with a stderr hint to reduce the check's output.

Build with `tools/cargo-slot cargo build -p brg-checks --locked`.
For a local demonstration without a real bus, from the checkout root run these
single-line commands in the same shell (the sample uses a local file as input):

```sh
demo=$(mktemp -d); printf 'first\n' > "$demo/value"; CHECKS_FILE="$PWD/plugins/checks/tests/sample.yaml" BORGCHART_BIN="$PWD/plugins/checks/tests/fake-brg" BORGCHART_STATE_DIR="$demo/state" BORGCHART_DELIVER_TO=triager-test CHECKS_SAMPLE_FILE="$demo/value" CHECKS_SAMPLE_NOTICE="$demo/notice" target/debug/brg-checks run --once
printf 'second\n' > "$demo/value"; CHECKS_FILE="$PWD/plugins/checks/tests/sample.yaml" BORGCHART_BIN="$PWD/plugins/checks/tests/fake-brg" BORGCHART_STATE_DIR="$demo/state" BORGCHART_DELIVER_TO=triager-test CHECKS_SAMPLE_FILE="$demo/value" CHECKS_SAMPLE_NOTICE="$demo/notice" target/debug/brg-checks run --once && cat "$demo/notice"
BORGCHART_STATE_DIR="$demo/state" target/debug/brg-checks last sample
```
