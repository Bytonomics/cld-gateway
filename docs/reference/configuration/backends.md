---
type: reference
title: "Backends"
status: stable
tags:
  - configuration
  - backends
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Backends

| Section | What it covers |
|---------|----------------|
| [What a backend entry looks like](#what-a-backend-entry-looks-like) | The fields under each `providers.backends` entry |
| [Switching the active backend](#switching-the-active-backend) | Changing which backend receives traffic |
| [Why only one at a time](#why-only-one-at-a-time) | The single-backend design |

The gateway talks to exactly **one** backend provider at a time, even
though its config file is shaped to hold settings for more than one. Think
of `providers.backends` as a small address book: every backend you've
configured lives there, and `providers.active` names the one that
actually receives traffic.

```yaml
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-5.6-sol
      unsupported_models: [gpt-5.2, gpt-5.3-codex]
```

## What a backend entry looks like

`providers.active` (string) names the single backend every request goes
to. `providers.backends` is a map keyed by backend name; each entry
(`config.BackendProviderConfig`) carries:

- `default_model` — used two ways: as the model chosen when a request
  doesn't specify one, and as the fallback when a request names a model
  this backend can't serve.
- `unsupported_models` — a list of model names this backend will not pass
  through as requested. A request naming one of these is transparently
  rewritten to use `default_model` instead — the client isn't rejected,
  it just gets served by the fallback model without knowing the
  substitution happened.

## Switching the active backend

Two ways to do this:

- Run `cld-gateway-sh setup` and pick the backend from the guided flow —
  see [The setup command](../../tutorials/setup-command.md).
- Edit `providers.active` directly to name the backend you want, then
  restart the gateway for the change to take effect.

## Why only one at a time

The gateway is a translation layer between one client-facing API shape and
whichever backend is active. Everything downstream of that choice —
default models, unsupported-model handling, request shaping — is scoped to
a single backend on purpose, so there's never ambiguity about which set of
model names or limits apply to a given request. If you want to switch
providers, you switch the whole gateway over; you don't route different
requests to different backends.
