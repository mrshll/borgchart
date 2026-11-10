# Seating

Before starting each member, check its explicit vendor and model (without the effort):

```sh
brg-seat route claude opus
brg-seat route codex gpt-6-astra
```

`go: ...` exits 0. `wait: ...` exits 3. Invalid arguments exit 2; help exits 0. A wait or failed check never authorizes another vendor or model. This command checks quota; it does not launch members or change their settings. Leads use the absolute plugin shim listed under “Commands you can run” for the granted `brg-seat route <vendor> <model>` action. A stale member context or damaged bus refuses the plugin action; any nonzero result means wait. A missing executable exits 127.

The `brg-seat` Rust plugin asks the installed Claude CLI's experimental `get_usage` control request or Codex's `app-server` subscription usage endpoint. It uses the existing vendor login, without opening credential stores. Install `brg-seat` alongside `brg` from the same revision and launch with that pinned bin directory first on PATH. The plugin is optional: teams declare it and grant its action; core brg has no seating subcommand. There is no Python adapter or external quota-reader setting.

Shared and applicable model-specific windows warn at 90% and refuse at 100%. An existing warning stays until usage falls below 80%. Backend denial, unknown model scope, missing utilization, malformed data, unavailable CLI, or stale local cache means wait. Claude fable requires an explicit fable scope. Codex class names and versioned model names map to the same class, but unrelated classes do not authorize one another. Retry advice names the latest known hard reset only when every relevant hard limit has a valid future reset; it is not a promise of restored access.

## Cache and configuration

Only normalized quota fields enter the private cache. The default is `$XDG_CACHE_HOME/borgchart/seat-v2`, or `~/.cache/borgchart/seat-v2`. The cache directory must be owned by the current user with mode 0700; files must be owned regular files with mode 0600 and one hard link. A symlink at the cache leaf or a cache file is refused. Existing parent aliases such as macOS's temporary-directory alias are resolved before descriptor-relative operations.

Successful readings last at most 120 seconds, shortened by the earliest future reset. Cached probe failures back off for 30 seconds; `--refresh` bypasses a successful reading but never that backoff. A shared lock serializes readers, with a 22-second lock deadline. Each vendor exchange has a 20-second deadline and separate 4 MiB stdout/stderr limits. Child process groups are killed on completion or failure. Output errors never relay vendor response text.

The cache root follows the launch environment's `XDG_CACHE_HOME`, or `HOME/.cache`; it must be absolute and contain no `..` components. There is no seating key in brg's configuration and no plugin-specific cache setting. Unwritable cache means wait. If a write fails after the probe, no backoff can be stored, so a later invocation may probe again.

File-backed credential and configuration **metadata**, plus the CLI's identity, fence each reading. An identity change during a probe returns wait; the next invocation may probe under the new identity. This includes a routine token refresh or settings rewrite. Credential contents are never read. Raw responses, errors, profiles and tokens are never persisted in the cache.

This fence cannot observe credentials held only in a keychain, keyring or another non-file store. If an account switch does not change the tracked files, a previous account's reading can be reused until the cache TTL expires (at most 120 seconds). Include that account-switch behavior in live qualification.

An identity change resets advisory hysteresis; warnings are not carried between possibly different accounts. Old identity entries are not pruned automatically. The cache is disposable and may be cleared after all seating checks have finished.

## Authentication and qualification limits

API-key teams omit this plugin and its grant. The plugin does not read brg configuration or infer its auth mode: calling it directly still checks subscription headroom, which cannot authorize API spending. Probes receive the launch environment allowlist without member identity or arbitrary API/backend overrides. Claude probes disable hooks, MCP configuration and setting sources; launched members may honor user settings that route them elsewhere. A subscription result does not authorize a paid API or proxy route.

Claude profile reads are deliberately skipped: no account metadata or persisted-utilization freshness guard is read from either profile. brg trusts the usage reply as returned. A silently stale Claude reply can permit seating near the real limit; a vendor-supported freshness guard is possible future work. Missing, malformed and incompatible replies still mean wait.

Fake tests do not qualify real vendor versions or sandbox access. Before a deployment cutover, check both vendors from the actual lead environment, record CLI versions and cold-start latency, and verify stat-only identity is stable before and after a normal probe. Repeated identity rewrites would cause repeated waits. Confirm the qualified Claude version accepts the empty `--setting-sources` argument. No command here grants filesystem or network access; unavailable login/cache/network remains wait. Changing a deployed team's plugin declaration changes its frozen files and requires an owner-coordinated full restart.

## Declaring the plugin

The bundled development teams grant this action to their leads:

```yaml
plugins:
  seat:
    actions:
      route: brg-seat route <vendor> <model>
roles:
  lead:
    prompt: lead.md
    plugins: {seat: [route]}
```

This is an existing no-path plugin: its executable resolves through the instance's frozen launch PATH. Invoking an absolute brg binary alone does not select its sibling plugin. Keep the pinned bin directory first in the operator’s launch PATH; see the [pinned install recipe](../teams/README.md#pinned-installation). When a member creates or fully starts a child, brg removes exactly that caller’s own member shim directory from the captured base PATH. Otherwise the child’s plugin action could re-enter the parent’s identity-fenced shim. All other PATH entries and their order remain unchanged. This minimal core correction supports nested plugin actions; it adds no resolver or fallback and leaves every identity fence intact.

A Codex lead's vendor sandbox must permit the quota probe's network access and cache writes. Declaring this action does not grant either; a denied probe stays wait. Qualify these from the actual lead environment before cutover.

Existing running instances keep their captured environment. A coordinated full stop/start under the fixed binary refreshes it; start from the root before child copies. Starting only a member in an already running copy reuses that copy’s environment. Deleting or recreating copies is unnecessary.
