# Logs and state

Everything the gateway persists lives under one directory, `~/.gateway/`
by default (overridable with `GATEWAY_HOME`; see
[Configuration](index.md)).

```
~/.gateway/
├── auth.json                      backend credentials
├── config.yml / config-dev.yml    runtime configuration
├── logs/
│   ├── http-exchange.log          human-readable exchange log
│   └── transport-decisions.jsonl  one line per transport decision
└── sessions/claudecode/           conversation state, one tree per session
```

## Credentials — `auth.json`

Written by `cld-gateway login` and refreshed automatically by the gateway
as needed. Contains the tokens the gateway needs to talk to its active
backend on your behalf. Treat it like any other credential file — it's not
shared, synced, or logged anywhere.

## Exchange log — `logs/http-exchange.log`

Every request/response pair the gateway handles gets one entry, written
after the exchange completes. Each entry is a flat block of `key: value`
lines followed by a dashed separator line, so the whole file stays greppable
without needing a JSON parser:

```
request_id: 7f3a2c10-...
method: POST
path: /v1/messages
status: 200
duration_ms: 842
------------------------------------
```

The `request_id` in each entry matches the `x-proxy-request-id` header the
gateway adds to its HTTP response, so you can go from "this specific client
request looked wrong" straight to its log entry. See
[Troubleshooting](../usage/troubleshooting.md) for how to use this.

The gateway rotates this file once it grows past a size threshold and
keeps only a bounded number of rotated copies — old copies are deleted
automatically so exchange logging never quietly fills your disk.

## Transport decisions — `logs/transport-decisions.jsonl`

A separate, machine-readable log (one JSON object per line) recording
which transport the gateway chose for each turn and why — useful when
diagnosing why a conversation is behaving differently than expected. See
[Troubleshooting](../usage/troubleshooting.md).

## Conversation state — `sessions/claudecode/`

Where the gateway keeps its durable memory of every conversation: which
turn came last, any branches created by rewinding and continuing
differently, and the checkpoints it uses to avoid re-sending context the
backend already has. This is what lets a conversation survive a gateway
restart.

- **Retention** — set `workflow.conversation_state.retention.max_session_age_days`
  in your config to have sessions with no recent activity cleaned up
  automatically. Left unset, nothing is removed automatically.
- **Corruption handling** — controlled by
  `workflow.conversation_state.corruption_policy` (see
  [Configuration](index.md)). The default refuses to continue a session
  whose stored state looks corrupt rather than guessing; an opt-in mode
  will instead quarantine the bad state and start that session fresh.
- **Custom location** — set `workflow.conversation_state.persistence_root`
  to store this somewhere other than under `~/.gateway/`.
