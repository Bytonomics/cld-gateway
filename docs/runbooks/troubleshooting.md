---
type: runbook
title: "Troubleshooting"
status: stable
tags:
  - troubleshooting
  - operations
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Troubleshooting

| Section | What it covers |
|---------|----------------|
| [Symptom → log → fix](#symptom-log-fix) | Quick-reference table for common failures |
| [Full vs. incremental transport](#full-vs-incremental-transport) | Where transport-decision detail lives |
| [Still stuck?](#still-stuck) | Filing an issue |

Start every investigation the same way: find the exchange in
`~/.gateway/logs/http-exchange.log`, correlated by request ID. If a client
gave you an error, it also received an `x-proxy-request-id` response
header carrying that ID — search the log for it and read the whole entry
before doing anything else. See [Logs and state](../reference/configuration/logs-and-state.md)
for the log's format and location.

## Symptom → log → fix

| Symptom | Where to look | Likely fix |
|---|---|---|
| Requests fail immediately with `authentication_error` | `GET /auth/status`, then the matching exchange-log entry | Run `cld-gateway login` again; credentials are missing or expired past automatic refresh. |
| A conversation seems to restart from scratch instead of continuing | `~/.gateway/logs/transport-decisions.jsonl` for that session | Check `workflow.conversation_state.enabled` is `true` in your config, and that `persistence_root` (if set) points to a writable, stable location. |
| Responses stop mid-stream with no error | Exchange-log entry's duration and status for that request | Likely the backend itself dropped the connection or hit its own limit; retry the turn. If it happens repeatedly on long turns, check whether your backend has an idle timeout you're bumping against. |
| A model name you sent isn't the one that answered | Exchange-log entry's response, and your config's `providers.<backend>.unsupported_models` | Expected behavior if the requested model is listed under `unsupported_models` — it was substituted with `default_model`. Adjust the config if you don't want that substitution. |
| Gateway won't start / exits immediately | Terminal output from `cld-gateway serve` | Usually a config file that fails to parse — check the YAML in your active config path (see [Configuration](../reference/configuration/index.md)) for a syntax error. |
| Startup logs a warning about credentials but the process stays up | Terminal output, then `GET /auth/status` | This is expected: `serve` starts even with stale credentials so the process doesn't flap. Log in to clear the warning. |
| A session behaves oddly after an unclean shutdown (crash, `kill -9`) | `~/.gateway/logs/transport-decisions.jsonl`, and whether the corruption policy triggered | With the default `fail_closed` corruption policy, the gateway refuses to continue that specific session rather than guess at its state — start a new session, or opt into the quarantine-and-reset corruption policy if you'd rather it self-heal automatically. |
| Disk usage from logs keeps growing | `~/.gateway/logs/` file sizes | Shouldn't happen — the exchange log rotates and old rotated files are deleted automatically. If it is happening, that's worth reporting as a bug. |

## Full vs. incremental transport

The gateway can carry a conversation turn to its backend either as a full
resend of context or as an incremental update to an existing backend-side
session, depending on whether the backend session it would reuse is still
considered valid. `~/.gateway/logs/transport-decisions.jsonl` records which one was
chosen for every turn and why. If you're trying to understand latency or
cost differences between turns in the same conversation, this file is the
place to look — it will show whether a given turn fell back to a full
resend and, if so, the reason.

## Still stuck?

Open an issue on the project's GitHub repository with the relevant
exchange-log entry (redact anything sensitive first) and the gateway
version you're running.
