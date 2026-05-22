# Unsupported (Intentionally Ignored) Features

This file documents client-visible features that the gateway currently **ignores on purpose**, including the rationale.

## Anthropic `stop_sequences`

- **Status:** Ignored (no-op)
- **Applies to:** `POST /v1/messages`
- **Behavior:**
  - If the request includes a non-empty `stop_sequences` array, the gateway does not forward it upstream.
  - The gateway logs a warning: `ignoring Anthropic stop_sequences (unsupported)`.

### Why it’s ignored

Anthropic `stop_sequences` does not have a guaranteed 1:1, safe equivalent across the upstream modes we support today (especially the ChatGPT Codex backend). Implementing stop strings via post-processing is risky (it can truncate valid JSON/tool payloads, corrupt streaming, or change tool-call semantics).

Until we have a proven, end-to-end compatibility design (and tests) for stop semantics, we treat `stop_sequences` as unsupported and ignore it explicitly rather than attempting an unsafe approximation.
