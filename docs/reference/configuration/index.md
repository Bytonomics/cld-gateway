---
type: reference
title: "Configuration Reference"
status: stable
tags:
  - configuration
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Configuration reference

| Section | What it covers |
|---------|----------------|
| [Overriding the config path](#overriding-the-config-path) | Env vars that redirect config loading |
| [Top-level shape](#top-level-shape) | The full YAML shape |
| [providers](#providers) | Backend selection and per-backend settings |
| [workflow](#workflow) | Everyday behavior toggles |
| [network](#network) | Listen address and outbound allowlist |
| [Related pages](#related-pages) | Backends and logs/state detail |

The gateway reads a single YAML file at startup. Which file, and where it
lives, depends on how you're running it (see
[Installation](../../tutorials/installation.md) for the two run modes):

| Run mode | Default config path | Listen address |
|---|---|---|
| Packaged (Homebrew) | `~/.gateway/config.yml` | `127.0.0.1:6473` |
| Developer | `~/.gateway/config-dev.yml` | `127.0.0.1:6483` |

## Overriding the config path

Two environment variables override the default, checked in this order:

- `GATEWAY_CONFIG_PATH` — an exact file path. Takes priority over
  everything else.
- `GATEWAY_HOME` — a directory; the gateway looks for the default filename
  under it instead of under your home directory.

If neither is set, the gateway falls back to the table above. A missing
config file is not an error — the gateway starts with built-in defaults
and behaves as if you had written a mostly-empty file.

Any config key can also be overridden by an environment variable named
`GATEWAY_<KEY_PATH>` with dots replaced by underscores (for example
`GATEWAY_NETWORK_LISTEN_ADDR`). This is mainly useful for scripted or
containerized runs; for everyday use, edit the YAML file.

## Top-level shape

```yaml
version: 1

providers:
  active: codex
  backends:
    codex:
      default_model: gpt-5.6-sol
      unsupported_models:
        - gpt-5.2
        - gpt-5.3-codex

workflow:
  fast_mode: false
  context_management:
    enabled: true
    mode: follow_request
  claude_code:
    slash_commands:
      enabled: true
      mode: promote_latest
  conversation_state:
    enabled: true
    corruption_policy: fail_closed

network:
  listen_addr: 127.0.0.1:6483
  allowed_hosts: []
```

## `providers`

`active` (string) names the single backend that receives traffic;
`backends` is a map of backend name to that backend's settings. See
[Backends](backends.md) for the full model and how to switch.

- `active` (string) — the name of the backend in use; must match a key
  under `backends`.
- `backends.<name>.default_model` (string) — the model this backend falls
  back to when a requested model isn't supported.
- `backends.<name>.unsupported_models` (list of strings) — model names
  this backend refuses to pass through as-is; requests naming one of
  these are transparently rewritten to `default_model` instead of
  failing.

## `workflow`

Everyday behavior toggles.

- `fast_mode` (bool, default `false`) — when enabled, requests are sent
  with a higher service priority where the backend supports it, trading
  cost for latency.

- `context_management` — controls how the gateway prunes an over-long
  conversation before sending it upstream.
  - `enabled` (bool, default `true`).
  - `mode` (string, default `follow_request`) — how aggressively pruning
    edits are applied.
  - `hard_limits` — optional numeric ceilings (max characters kept per
    tool result, max tool uses kept, max thinking turns kept) that apply
    regardless of mode. Unset by default, meaning no extra ceiling beyond
    what `mode` already does.

- `claude_code.slash_commands`
  - `enabled` (bool, default `true`) — whether slash-command handling is
    active at all.
  - `mode` (string, default `promote_latest`) — how the gateway resolves
    which slash-command invocation takes effect when more than one
    candidate is present in a request.

- `conversation_state` — controls the durable memory of each conversation.
  - `enabled` (bool, default `true`).
  - `persistence_root` (path, optional) — where conversation state is
    stored on disk. Defaults to a location under the gateway's own home
    directory; see [Logs and state](logs-and-state.md).
  - `corruption_policy` (string, default `fail_closed`) — what happens if
    stored state for a session is found to be corrupt. `fail_closed`
    refuses to continue that session rather than guess; an opt-in
    quarantine-and-reset mode is also available for people who'd rather
    the gateway recover automatically and start that session fresh.
  - `retention.max_session_age_days` (integer, optional) — sessions with
    no activity for longer than this are eligible for cleanup. Unset by
    default, meaning nothing is cleaned up automatically.

## `network`

- `listen_addr` (string) — the address the gateway binds to. Defaults per
  run mode as shown in the table above. The gateway only ever binds to
  localhost addresses.
- `allowed_hosts` (list of strings, default empty) — additional outbound
  hosts the gateway is permitted to contact, beyond its backend's own
  default hosts and localhost. See [Security](../../explanation/security.md) for
  what's blocked outright regardless of this list.

## Related pages

- [Backends](backends.md) — the one-active-backend model in depth.
- [Logs and state](logs-and-state.md) — where everything above actually
  gets written to disk.
