# Timer — `brg-timer`

Run commands on an interval and wake a member when their output changes, fails,
or recovers. Schedule recurring UTC wakes or short-lived members in the same file. Check notices contain
names and statuses; output stays in local state for `last` to read as untrusted evidence. This is a Rust workspace crate;
`make install` installs it on PATH alongside `brg-slack`. To install just timer
from a checkout, use `cargo install --path plugins/timer`.

Name the schedule file on the timer's process line. Relative paths resolve
from the team's template folder. For example, `checks.yaml` can contain these checks.

```yaml
- name: service-status
  run: [./read-service, --summary]
  every_minutes: 5
  ignore: '(?m)^generated-at:.*\n'
- name: revision
  run: [git, rev-parse, HEAD]
  every_minutes: 10
```

The top level is a YAML list of check or schedule mappings (`[]` for neither).
Duplicate keys and merge keys are refused; YAML 1.1 booleans such as `yes` stay strings.
Names must be unique and
match `[a-z][a-z0-9-]{0,31}`. In a check, `run` is a non-empty string list executed without a
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
  timer:
    process: [brg-timer, run, checks.yaml]
    delivers_to: triager
    actions:
      last: brg-timer last <name>
      promote: brg-timer promote <name> <reason>
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
output. Removed checks, schedules and their queued signals are pruned at startup
and reload. After each check and tick, health reports `ok`, `at`, `detail`,
`pending_sends`, and per-check `status` and `last_run`. A failing check, failed
delivery or invalid configuration makes health not ok. Each tick compares the
checks file's bytes with its last read. Valid edits replace checks and schedules; a newly
added check runs on that tick. An invalid or unreadable edit keeps the last good
configuration running, with an error in health saying the last good file is still
in use. Bad bytes are not parsed again until they change. Invalid startup files
still stop startup. Alarms continue through a bad edit.

Actions run in the calling member's folder and use the same state directory:

- `brg-timer last <name>` reads committed evidence without waiting for a running
  check's lock. It prints an untrusted-data header, command, last run, status/exit,
  stderr, current output and previous distinct successful output. While failing,
  the previous successful evidence is the last good output. Output lines start
  with `| ` and control characters are escaped. Unknown names exit 2 and list
  known checks.
- `brg-timer promote <name> <reason>` sends the check ID and a reason
  to `BORGCHART_PARENT` with `--from BORGCHART_MEMBER`, so brg checks the parent
  route. Reasons are fixed: `outage`, `alert`, `question`, `other`. Success
  prints brg's output, including its ID, and exits 0. For restricted Codex members,
  it instead prints `queued; brg sends it when the command returns`: brg's
  PostToolUse hook sends as the member, to its parent only, and reports failures
  as extra context. Permanent refusals are removed. If brg cannot start, the send
  stays queued and is only retried at the next tool call, which may be the member's
  next wake; uncertain sends need inspection before
  retrying (see [bus delivery](../../docs/bus.md#attention)). Other failures
  print brg's error and exit 1; inspect any committed ID before retrying.

`brg-timer run checks.yaml --once` runs one tick, useful for testing. It exits nonzero for
invalid startup configuration; check or delivery failures appear in health.
`CHECKS_TIMEOUT_SECONDS` overrides the command timeout for tests only; production
uses the fixed 60-second default. A check whose output changes every run wakes
every interval, so use `ignore` for volatile data.
Checks run one after another, so slow commands delay the next check and tick.
Each check captures at most 16 MiB of stdout and 1 MiB of stderr. Exceeding either
cap kills the process group, discards its output and records `failing / output-limit`
with a stderr hint to reduce the check's output.

## Recurring schedules

A delivery schedule uses `name`, `cron` and optional `missed` and `prompt`. A
[run schedule](#scheduled-runs) uses `name`, `cron`, `role`, `timeout_minutes`
and optionally either `prompt` or `prompt_file`. Use unique names across
checks and schedules.

```yaml
- name: daily-review
  cron: '0 13 * * 1-5'
- name: release-notes
  cron: '0 15 * * 1'
  missed: catch-up
  prompt: Follow notes/weekly-release.md
```

On delivery schedules, `prompt` is a non-empty string of at most 300 UTF-8 bytes
after trimming, with no newlines or control characters. For longer instructions,
use the notice to tell the member which file to read. The template folder's
controller writes this standing instruction from the team's owner. Check
command output remains untrusted evidence.

Cron has exactly five numeric fields, separated by spaces or tabs, in UTC:
minute 0–59, hour 0–23, day of month 1–31, month 1–12, weekday 0–7.
Both 0 and 7 mean Sunday, including in lists and ranges such as `6,7` or `5-7`.
Each field is a comma list of `*`, `N` or `A-B`; stars and ranges can take
`/S` with a positive whole step. Write `N-<max>/S` instead of `N/S`,
and list both parts of a wrapping range instead of `22-2`. Empty list items,
out-of-range values and zero steps are refused. Use numbers instead of names,
five fields instead of nicknames such as `@daily`, and explicit days instead
of `L`, `W`, `#` or `?`. Seconds and years are refused.

Day matching follows **cronie's star rule**: if either day field begins with
`*` (including `*/2`), both day fields must match. Otherwise either may match.
Thus `0 0 1 * 1` fires on the 1st and every Monday, while `0 0 */2 * 1`
fires only on odd-numbered Mondays. Impossible dates are refused after searching
one 400-year Gregorian cycle. Expressions with more than five minute values are
also refused: use at most five, such as `0,12,24,36,48`, instead of `*/5`.

Delivery schedules require a valid `delivers_to` at startup and on reload. Each occurrence
sends `<name> <YYYY-MM-DDTHH:MMZ>` and, if set, the trimmed prompt on a second line as external `timer`, after check
notices and before alarms, in time-then-name order. New schedules and edited
expressions start after the current time and never fire retroactively.
Whitespace-only edits do not reset an expression.
Prompt-only edits preserve timing; the next send uses the new text. The hourly cap
and catch-up rules below are unchanged, and existing per-notice limits still apply.

`missed: skip` is the default for occurrences while the process was down;
`catch-up` sends the latest missed occurrence once. Occurrences passed while
running (including slow checks, suspend and forward clock jumps) send once as
the latest, under either policy. There is at most one pending occurrence per
schedule; newer occurrences replace it. Pending sends survive restart under
either policy. State is saved before each send; interrupted sends may duplicate.

A fixed cap allows **six schedule send attempts per rolling hour per timer
process**, failed attempts included. Check notices and alarms do not count.
Waiting messages persist until capacity returns. Health reports each schedule's
`next` and `pending`, and is not ok on send failure or while the cap holds a
message back. The persisted `next` is a high-water mark: setting the clock back
cannot repeat an occurrence, but a large correction delays wakes until that mark
passes. Editing the expression resets it. Send attempts outside the last hour,
including future attempts after a clock correction, are discarded.

`brg-timer validate <file>` checks the YAML and any referenced prompt files.
It needs no state directory, prints counts on success and exits 0. Invalid
files exit 1 with the same error as startup.
At startup and on reload, the timer also checks `delivers_to` for delivery
schedules, `BORGCHART_RUNS` for declared roles and `BORGCHART_RUN_ROLE` for ad-hoc roles.

To deploy, edit the timer's process line and schedule files, then run
`make install`. End only the team's timer window and run `brg start <team>`.
Later valid file edits reload without another restart.

## Scheduled runs

A run schedule creates a fresh member for each occurrence. Its first message
includes the schedule name and UTC minute, then `Report to <team>/<parent>` on
the same line, followed by the prompt if supplied. The address comes from the
new member's status row. A declared role without a parent receives the schedule
and prompt and can take its reporting instructions from its role file.
`brg status` marks each run member with its schedule.

```yaml
- name: daily-review
  cron: '0 13 * * 1-5'
  role: reviewer
  timeout_minutes: 45
  prompt_file: jobs/daily-review.md
- name: weekly-notes
  cron: '0 14 * * 2'
  role: {models: {claude: sonnet low}}
  timeout_minutes: 30
  prompt: Summarize the week's changes for the editor.
```

A string names a declared role. A mapping defines an ad-hoc role using the same
frontmatter as a role file. Pin a model and supply `prompt` or `prompt_file` for
each ad-hoc role. With one model pin, the timer uses that vendor. With multiple
pins, set `vendor` to one of the pinned vendors. A pin can include an
effort suffix such as `sonnet low`. Keep commas and equals signs out of the
model name.

`timeout_minutes` is required and must be a whole number from 1 to 1440.
Runs skip missed occurrences during downtime and refuse the `missed` field.

A run can use an inline `prompt` (use YAML `|` for multiple lines) or
`prompt_file`. Both must contain non-empty UTF-8 text of at most 16000 bytes
after trimming. Inline prompts allow newlines. Prompt files allow newlines
and tabs and may be up to 64 KiB on disk. Remove other control characters.
A restricted Claude run role takes a one-line prompt. The whole message must
fit in two lines and 700 bytes, including the header and report address.
Pane size can still prevent delivery after creation. Use an unrestricted role
or a Codex role for longer prompts. All run messages must fit the 16384-byte
message limit, including the header and report address.

Use a relative `prompt_file` path without `..`. The file must resolve inside
the folder containing the checks file, including when the path uses symlinks.
The timer checks it at startup and on reload. A bad reload keeps the last good
configuration and retries on each tick, so fixing the prompt file is enough to
complete the reload. The timer reads the file again at each occurrence, so
edits take effect at the next run. An invalid or missing file at that point records
`failed-to-start` before seating or creating a member. Changing the path
preserves schedule timing, as do prompt-only and role-only edits.

For a declared role, grant the timer plugin's `done` action in `template.yaml`,
by name or with `["*"]`. For ad-hoc roles, set the timer's `delivers_to` and
add its `done` action. The timer then runs them on the built-in `<plugin>-run`
role.

```yaml
plugins:
  timer:
    process: [brg-timer, run, checks.yaml]
    delivers_to: editor
    actions:
      done: brg-timer done
      last: brg-timer last <name>
roles:
  editor:
    prompt: editor.md
  reviewer:
    prompt: reviewer.md
    parent: editor
    start: false
    plugins: {timer: [done]}
```

For a declared run, use a dedicated role with `start: false`, no worktree and no child roles.
Restart the timer after changing which roles are eligible. Give a declared role
`max` capacity for as many concurrent schedules as need it. Set
`env: {SEAT: brg-seat}` on the timer to check subscription capacity before
creation. Run attempts, including seat
waits and launch failures, share the six-per-hour cap with delivery schedules.
If the previous run is active, seating says wait, or the cap is full, the
timer skips the occurrence. A file edit can schedule an expensive model within
one tick. A model alias such as `sonnet` resolves at each start.

`brg-timer done` marks only the calling member's current scheduled run as
finished. It exits 2 for another member or an old start. On its next tick the
timer stops and deletes finished or overdue members. It uses `delete --force`
so unread messages can't keep a stopped run member around. Cleanup failures
make health not ok and are retried. Health shows active run member IDs and
deadlines.

Members granted timer actions can write timer state and end runs early.
The timer only stops and deletes members with IDs derived for its scheduled
runs. It can clean up a previously started run by ID after its role disappears
from status, even if the grant was removed.

`brg-timer last <name>` prints one outcome line with UTC time and any reason.
Outcomes are `done`, `timed-out`, `skipped`, `failed-to-start` and
`interrupted`. A restarted timer adopts a live run with the same start ID and
original deadline. Other recorded runs are cleaned up as interrupted. A full
team start leaves stopped run members for the timer to remove as interrupted.
The timer stores no run output.

To remove a declared role or its run grant, remove its schedules and wait for
their runs to end. Then remove the role or its grant and restart the timer.
To remove ad-hoc run support, remove the ad-hoc schedules and wait for their
runs to end. Then remove `delivers_to` or `done` from the timer and restart it.

## Alarms

An alarm-only team declares the process and three actions, grants them to its
lead, and puts `[]` in `checks.yaml`. It needs no `delivers_to`:

```yaml
plugins:
  timer:
    process: [brg-timer, run, checks.yaml]
    actions:
      alarm-set: brg-timer alarm set <when> <note>
      alarm-show: brg-timer alarm show
      alarm-clear: brg-timer alarm clear
```

`brg-timer alarm set <when> <note>` replaces your alarm. Use 1–10080 whole
minutes or a future `YYYY-MM-DDTHH:MMZ` within 7 days, and one non-empty line
of at most 200 bytes without control characters. `alarm show` shows it;
`alarm clear` removes it. The shim supplies your verified team/member identity.
Setting refuses a missing heartbeat or one older than 300 seconds; tell the
owner. An invalid startup `checks.yaml` stops the process; setting then refuses
once its heartbeat becomes stale. Invalid live edits keep the last good
configuration and alarms running.

Every tick writes health, even with `[]`. After checks, their delivery and schedules,
due alarms send an ordinary `Alarm due` message to their own member. One slow pass
of checks delays alarms by up to its length, plus the 30-second tick interval.
Alarms persist in `alarms/<member>.json` and use a separate `alarms.lock`, so
setting never waits behind checks. A successful send removes only the unchanged
alarm, including when brg reports `stored` or `uncertain`. A failed send keeps
it for the next tick and marks health not ok. Overdue alarms fire on restart.
If delivery keeps failing, use `alarm clear` to remove your own alarm. For a
member that no longer exists, the owner removes `alarms/<member>.json` from
the plugin's state directory, `<team state>/plugins/timer/`.

Build with `tools/cargo-slot cargo build -p brg-timer --locked`.
For a local demonstration without a real bus, from the checkout root run these
single-line commands in the same shell (the sample uses a local file as input):

```sh
demo=$(mktemp -d); printf 'first\n' > "$demo/value"; BORGCHART_BIN="$PWD/plugins/timer/tests/fake-brg" BORGCHART_STATE_DIR="$demo/state" BORGCHART_DELIVER_TO=triager-test CHECKS_SAMPLE_FILE="$demo/value" CHECKS_SAMPLE_NOTICE="$demo/notice" target/debug/brg-timer run plugins/timer/tests/sample.yaml --once
printf 'second\n' > "$demo/value"; BORGCHART_BIN="$PWD/plugins/timer/tests/fake-brg" BORGCHART_STATE_DIR="$demo/state" BORGCHART_DELIVER_TO=triager-test CHECKS_SAMPLE_FILE="$demo/value" CHECKS_SAMPLE_NOTICE="$demo/notice" target/debug/brg-timer run plugins/timer/tests/sample.yaml --once && cat "$demo/notice"
BORGCHART_STATE_DIR="$demo/state" target/debug/brg-timer last sample
```
