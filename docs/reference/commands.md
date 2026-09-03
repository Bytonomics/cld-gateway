---
type: reference
title: "Command Reference"
status: stable
tags:
  - cli
  - commands
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Command reference

| Section | What it covers |
|---------|----------------|
| [cld-gateway serve](#cld-gateway-serve) | Starting the HTTP service |
| [cld-gateway login [backend]](#cld-gateway-login-backend) | Establishing backend credentials |
| [cld-gateway-sh setup](#cld-gateway-sh-setup) | The Homebrew-side setup helper |
| [Exit behavior](#exit-behavior) | Shutdown and login exit codes |

## `cld-gateway serve`

Starts the gateway's HTTP service in the foreground. This is what the
Homebrew background service runs on your behalf; run it directly if you
want to see logs live or you're not using the background service.

Before it starts accepting traffic, `serve` checks that your backend
credentials are present and valid. If they're missing or have expired
past the point of automatic refresh, it logs a clear warning and starts
anyway — requests will fail with an authentication error until you log in,
rather than the whole process refusing to start. This is deliberate: a
stale token shouldn't take down a service that might otherwise be fine.

## `cld-gateway login [backend]`

Runs an interactive, browser-based login flow for the given backend
(defaults to the gateway's current active backend if omitted). On success,
credentials are written to `~/.gateway/auth.json` and the gateway can
start serving requests for that backend.

This is the only supported way to establish credentials — there's no
non-interactive login path, by design: it always goes through the
backend's own login page, the same one you'd see in a browser.

## `cld-gateway-sh setup`

The Homebrew-side setup helper — installs config, client settings, and
commands into your home directory, and offers a guided way to choose your
active backend. See [The setup command](../tutorials/setup-command.md)
for the full walkthrough.

## Exit behavior

- A clean shutdown (Ctrl-C, or a stop from the service manager) lets any
  in-flight request finish streaming before the process exits, up to a
  short drain timeout.
- `login` exits non-zero if the flow is cancelled or the backend rejects
  the credentials; nothing is written to `auth.json` in that case.
