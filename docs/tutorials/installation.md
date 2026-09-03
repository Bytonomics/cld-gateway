---
type: tutorial
title: "Installation"
status: stable
tags:
  - installation
  - homebrew
stale_after: 2027-03-03
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Installation

| Section | What it covers |
|---------|----------------|
| [Homebrew](#homebrew) | Installing the daemon and its companion commands |
| [Two run modes](#two-run-modes) | Packaged vs. developer installs |
| [First run](#first-run) | Logging in before serving traffic |

## Homebrew

The gateway ships as a Homebrew formula from a dedicated tap.

```sh
brew tap Bytonomics/tap
brew install cld-gateway
```

This installs:

- `cld-gateway` — the daemon itself.
- `cld-gateway-sh` — a setup and maintenance helper.
- `cldg` and `clddg` — wrapper commands that launch your Claude Code-style
  client already pointed at the packaged gateway (`clddg` additionally
  skips the client's own permission prompts).
- A default `config.yml` and `settings.json`, plus a set of packaged
  slash-commands for the client.

After installation, run the setup helper once to finish wiring your home
directory:

```sh
cld-gateway-sh setup
```

See [The setup command](setup-command.md) for what this does and what you
can customize while running it.

Homebrew also registers `cld-gateway` as a background service, so it can
start automatically and restart itself if it crashes:

```sh
brew services start cld-gateway
```

## Two run modes

The gateway has two independent installs that never share state:

- **Packaged** — installed via Homebrew, launched by `cldg` / `clddg` or
  the Homebrew service, listening on `127.0.0.1:6473`, reading
  `~/.gateway/config.yml`.
- **Developer** — run from a source checkout during gateway development
  itself, launched by developer wrapper commands, listening on
  `127.0.0.1:6483`, reading `~/.gateway/config-dev.yml`.

If you're just using the gateway as an end user, you only need the
packaged install. The developer mode exists for people working on the
gateway's own source.

## First run

Before serving any traffic, the gateway needs credentials for its backend:

```sh
cld-gateway login
```

This opens a browser-based login flow and stores the resulting credentials
under `~/.gateway/`. Once that succeeds, start the service (or let
Homebrew's service manager do it), and you're ready to send your first
request — see [Quickstart](quickstart.md).
