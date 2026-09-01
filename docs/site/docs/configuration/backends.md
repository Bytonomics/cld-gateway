# Backends

The gateway talks to exactly **one** backend provider at a time, even
though its config file is shaped to hold settings for more than one. Think
of `providers` as a small address book: every backend you've configured
lives there, but only the one marked `active: true` actually receives
traffic.

```yaml
providers:
  codex:
    active: true
    default_model: gpt-5.6-sol
    unsupported_models: [gpt-5.2, gpt-5.3-codex]
```

## What a backend entry looks like

Each entry under `providers` is keyed by the backend's name and carries:

- `active` — exactly one entry across the whole map should have this set
  to `true`. That's the backend every request goes to.
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
  see [The setup command](../getting-started/setup-command.md).
- Edit `providers` directly: set `active: false` on the current backend
  and `active: true` on the one you want, then restart the gateway for the
  change to take effect.

## Why only one at a time

The gateway is a translation layer between one client-facing API shape and
whichever backend is active. Everything downstream of that choice —
default models, unsupported-model handling, request shaping — is scoped to
a single backend on purpose, so there's never ambiguity about which set of
model names or limits apply to a given request. If you want to switch
providers, you switch the whole gateway over; you don't route different
requests to different backends.
