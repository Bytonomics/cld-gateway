# cld-gateway

**cld-gateway** is a local proxy that lets Claude Code-style clients talk to
a Codex/ChatGPT backend. It speaks the Anthropic Messages API on one side,
so any client built for that API can point at the gateway instead of
Anthropic's servers, and it handles the translation, streaming, and
bookkeeping needed to make an OpenAI-family backend behave like one.

It runs on your own machine, listens only on localhost, and keeps its state
(your conversations, your credentials, its logs) under a single directory in
your home folder.

## Drop-in Anthropic API

Point any Anthropic Messages API client at the gateway's local address and
it works: streaming responses, tool calls, system prompts, and the usual
request/response shape all come through unchanged from the client's point
of view. You don't rewrite your client or your prompts to add a second
backend — you repoint one URL.

## Conversation state that survives restarts

The gateway remembers where each conversation left off — which turn came
last, which branch you're on if you rewound and forked, what was already
sent upstream — persisted to disk. Restart the gateway, restart your
laptop, and the conversation picks back up where it was instead of
re-sending everything from scratch.

## Configurable translation and transport

A single YAML file controls which backend is active, which model it
defaults to, how aggressively old conversation context gets pruned to stay
under limits, and where logs and state live. Change a setting, restart the
service, and the new behavior takes effect — no rebuild, no redeploy.

## Get started

- [Installation](docs/getting-started/installation.md) — install the
  gateway and its companion CLI.
- [Quickstart](docs/getting-started/quickstart.md) — log in, start serving,
  and send your first request.
- [Configuration reference](docs/configuration/index.md) — every setting,
  what it defaults to, and how to change it.
