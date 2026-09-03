---
type: tutorial
title: "Quickstart"
status: stable
tags:
  - quickstart
  - onboarding
stale_after: 2027-03-03
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Quickstart

| Section | What it covers |
|---------|----------------|
| [1. Log in](#1-log-in) | Authenticating against the active backend |
| [2. Start serving](#2-start-serving) | Running the daemon |
| [3. Point a client at it](#3-point-a-client-at-it) | Repointing an Anthropic Messages API client |
| [4. Verify it's alive](#4-verify-its-alive) | The health check |
| [5. Send a first turn](#5-send-a-first-turn) | Confirming an end-to-end request |

This walks through logging in, starting the service, and confirming a
request makes it all the way through.

## 1. Log in

```sh
cld-gateway login
```

Follow the browser prompt to authenticate. On success, the CLI confirms
your credentials were saved.

## 2. Start serving

If you installed via Homebrew and started the background service, this is
already running. Otherwise, start it directly:

```sh
cld-gateway serve
```

The gateway binds to localhost only — `127.0.0.1:6473` for a packaged
install, `127.0.0.1:6483` for a developer install — and does not accept
connections from other machines.

## 3. Point a client at it

Configure your Anthropic Messages API client to use the gateway's address
instead of Anthropic's own endpoint. If you're using the packaged wrapper
commands (`cldg` / `clddg`), this is already done for you — just run one of
those commands to launch your client.

## 4. Verify it's alive

```sh
curl http://127.0.0.1:6473/health
```

A healthy gateway responds with a small JSON body describing its status.

## 5. Send a first turn

Send any request through your client, or directly:

```sh
curl http://127.0.0.1:6473/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"<your-model>","max_tokens":256,"messages":[{"role":"user","content":"Say hello."}]}'
```

You should see a streamed response. To confirm the gateway logged the
exchange, check the newest entry in its exchange log at
`~/.gateway/logs/http-exchange.log` — every request/response pair is
recorded there, keyed by a request ID that also comes back in the
response's `x-proxy-request-id` header. That correlation is the starting
point for any troubleshooting; see
[Troubleshooting](../runbooks/troubleshooting.md).
