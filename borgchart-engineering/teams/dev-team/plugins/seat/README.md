# Seating plugin

Both the root team and dev teams use this same plugin before each launch. The design panel has no launch authority; its parent lead checks seating for its members. `seat route <claude|codex> <model>` prints `go: <reason>` (exit 0), or `wait: <reason>` (exit 3). Bad adapter arguments exit 2. Any error means wait, never substitute a vendor or model.

The plugin requires `uv`, Python 3.11 or later, and the locally authenticated vendor CLIs. Its bundled `usage-route` reader is the default; no external checkout, PATH entry or configuration is needed. The reader uses only the Python standard library. See [quota reader details](../../../../../docs/usage.md) for protocols, cache and compatibility.

One optional `seat-reader` file next to brg's configuration names a replacement executable on PATH or by absolute path. That directory is the parent of `$BORGCHART_CONFIG` when set, otherwise `~/.config/borgchart`. The file contains only the executable name or path, without quotes, flags or shell syntax. An invalid configured reader does not fall back. This lookup follows brg's config location, not `XDG_CONFIG_HOME`. Neither the adapter nor the reader stores credentials.

The adapter asks for exactly the selected vendor and model; it never requests `auto`, substitutes a fallback or launches returned argv. Codex aliases are matched by model class (`astra`, `sol`, `terra`, `luna`, including `gpt-<release>-<class>`). Supported Claude quota scopes are `opus`, `sonnet` and explicitly reported `fable`. Unknown mappings mean wait. The reader checks quota only; brg validates models and effort when launching.

A successful response must authorize that route, include one matching available evaluation, and report a fresh, timezone-aware `ok` observation for the vendor. Unknown, stale, future, blocked, malformed, missing or failed readings, and a 60-second timeout, all return `wait`. Blocked or waiting decisions retain a valid future retry time when provided. Raw reader stderr is never relayed.

The reader owns quota policy: advisory at 90%, stop at 100%, blocked or unknown. The adapter preserves advisory messages (scope, utilization, observation and reset times), checks that constraints do not contradict availability, and adds no lower cutoff. A known 99% reading can authorize a launch. Normal tool approvals still apply when the reader needs network or private-cache access.

Run the adapter tests from the repository root with `uv run --no-project --with pytest==8.3.5 pytest tools/tests/test_seat.py tools/tests/test_usage.py tools/tests/test_seat_usage.py`. The fake sources exist only in tests. Before cutover, check both vendors and every declared model from an actual lead's plugin environment; unit tests do not establish access to live usage sources.
