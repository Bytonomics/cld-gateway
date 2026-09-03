---
type: explanation
title: "Gateway Overview"
status: stable
tags:
  - overview
  - anthropic-api
stale_after: 2027-05-03
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Gateway Overview

| Section | What it covers |
|---------|----------------|
| [What the gateway does](#what-the-gateway-does) | The translation role it plays |
| [Drop-in Anthropic API](#drop-in-anthropic-api) | Why clients don't need to change |
| [Conversation state that survives restarts](#conversation-state-that-survives-restarts) | Durable per-conversation memory |
| [Configurable translation and transport](#configurable-translation-and-transport) | What one YAML file controls |
| [Where to go next](#where-to-go-next) | Installation, quickstart, configuration |

## What the gateway does

**cld-gateway** is a local proxy that lets Claude Code-style clients talk to a Codex/ChatGPT
backend. It speaks the Anthropic Messages API on one side, so any client built for that API can
point at the gateway instead of Anthropic's servers, and it handles the translation, streaming,
and bookkeeping needed to make an OpenAI-family backend behave like one.

It runs on your own machine, listens only on localhost, and keeps its state (your conversations,
your credentials, its logs) under a single directory in your home folder.

## Drop-in Anthropic API

Point any Anthropic Messages API client at the gateway's local address and it works: streaming
responses, tool calls, system prompts, and the usual request/response shape all come through
unchanged from the client's point of view. You don't rewrite your client or your prompts to add
a second backend — you repoint one URL.

## Conversation state that survives restarts

The gateway remembers where each conversation left off — which turn came last, which branch
you're on if you rewound and forked, what was already sent upstream — persisted to disk. Restart
the gateway, restart your laptop, and the conversation picks back up where it was instead of
re-sending everything from scratch.

## Configurable translation and transport

A single YAML file controls which backend is active, which model it defaults to, how
aggressively old conversation context gets pruned to stay under limits, and where logs and state
live. Change a setting, restart the service, and the new behavior takes effect — no rebuild, no
redeploy. See [Configuration reference](../reference/configuration/index.md) for every setting.

## Where to go next

- [Installation](../tutorials/installation.md) — install the gateway and its companion CLI.
- [Quickstart](../tutorials/quickstart.md) — log in, start serving, and send your first request.
- [Configuration reference](../reference/configuration/index.md) — every setting, what it
  defaults to, and how to change it.
