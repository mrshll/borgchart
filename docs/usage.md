# Subscription quota reader

The seating plugin needs a fresh quota decision for exactly the selected vendor
and model before a launch. This change brings that reader alongside the plugin,
so copying the team also copies its reader. Implementation is in progress.

The reader uses the locally authenticated Claude and Codex CLIs. It requests
usage metadata without a model turn. Claude's experimental `get_usage` and
Codex's `account/rateLimits/read` are compatibility boundaries: missing,
incompatible or incomplete data means unknown and prevents seating.

Quota at 90% is advisory; 100%, a backend denial or unknown usage prevents
seating. The reader does not switch vendors or models, spend credits, redeem
resets or manage login. It preserves the selected model; brg remains responsible
for model and effort validation.

The port preserves a short cache, failure backoff, scoped quota windows and
freshness checks. Cache records contain normalized quota metadata, never tokens
or raw CLI responses. Configuration belongs alongside brg's configuration; no
external orchestrator configuration is required.

Codex protocol reference: [App Server](https://learn.chatgpt.com/docs/app-server).
The client initializes a stdio connection, sends `initialized`, then requests
`account/rateLimits/read`. No thread or turn is created.
