---
type: tutorial
title: "The Setup Command"
status: stable
tags:
  - installation
  - homebrew
stale_after: 2027-03-03
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# The setup command

| Section | What it covers |
|---------|----------------|
| [What it does](#what-it-does) | The wiring `setup` performs |
| [Choosing the active backend](#choosing-the-active-backend) | Switching backends through the guided flow |
| [When to run it again](#when-to-run-it-again) | Situations that call for a re-run |

`cld-gateway-sh setup` finishes wiring the gateway into your home
directory after a Homebrew install (or re-applies that wiring after an
upgrade). Run it once after installing, and again any time you want to
change the choices it offers.

```sh
cld-gateway-sh setup
```

## What it does

- Creates the gateway's state directories if they don't already exist.
- Installs the packaged runtime configuration to `~/.gateway/config.yml`
  (only if one isn't already there — it will not silently overwrite a
  config you've edited).
- Installs the packaged client settings so the wrapper commands (`cldg`,
  `clddg`) know how to launch your client against the gateway.
- Symlinks shared entries from your regular client configuration directory
  into the gateway's own client configuration directory, so things like
  your existing agents, skills, and history stay available when launched
  through the wrappers.
- Installs the packaged set of slash-commands for your client.

## Choosing the active backend

Setup also lets you choose which backend the gateway routes to when more
than one is available, and set that backend's default model. This writes
directly into `~/.gateway/config.yml`, under the same keys documented in
[Configuration](../reference/configuration/index.md) — running setup again
is a safe way to change your active backend later without hand-editing YAML.

## When to run it again

- After a Homebrew upgrade, to pick up newly packaged commands or settings.
- After deleting or renaming `~/.gateway/config.yml`, to regenerate it.
- Any time you want to switch backends or change the default model through
  a guided flow instead of editing the config file directly.
