# Anthropic ↔ Codex(OpenAI) Mapping (Gateway)

This gateway exposes an **Anthropic-compatible** HTTP API (`/v1/messages`) intended to work with **Claude Code**.
Internally it calls the **ChatGPT subscription Codex backend** (`https://chatgpt.com/backend-api/codex/responses`)
using a **Responses-like** payload shape (matching `others/codex` as closely as possible).

Important constraints:

- We are **not** using `https://api.openai.com/v1/responses` (API-key billing). We are using the **Codex subscription backend**.
- The gateway is currently **stateless** from the client's perspective: each request carries **full conversation history**.

## 1) Anthropic `/v1/messages` request

### Sample (Claude Code-style)

```json
{
  "model": "gpt-5.4",
  "stream": true,
  "max_tokens": 32000,
  "temperature": 1,
  "top_p": 1,
  "top_k": 0,
  "stop_sequences": [],
  "system": [
    { "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." }
  ],
  "messages": [
    {
      "role": "user",
      "content": [{ "type": "text", "text": "hi" }]
    }
  ],
  "tools": [],
  "tool_choice": { "type": "auto" },
  "metadata": { "user_id": "{\"device_id\":\"...\"}" },
  "output_config": {
    "format": {
      "type": "json_schema",
      "schema": { "type": "object", "properties": { "title": { "type": "string" } } }
    }
  }
}
```

### Field meanings (Anthropic)

- `model`: the model name Claude Code wants to use (we treat this as a routing hint).
- `stream`: if true, the client expects **Anthropic SSE** (`message_start`, `content_block_delta`, etc.).
- `system[]`: array of blocks (Claude Code typically uses text blocks).
- `messages[]`: full conversation history. `content` can be a string or block array.
- `tools[]` / `tool_choice`: tool schemas + tool selection policy.
- `output_config`: structured output controls (Claude Code uses JSON schema output sometimes).
- `max_tokens`, `temperature`, `top_p`, `top_k`, `stop_sequences`, `metadata`: sampling/limits/metadata.

Supported content blocks we parse today:

- `text`
- `image` (Anthropic `{source:{type:"base64",media_type,data}}`)
- `tool_use` (assistant tool call, when present in history)
- `tool_result` (user tool output, for tool-loop continuation)

## 2) Codex backend request (Responses-like)

### Sample (what the gateway sends)

```json
{
  "model": "gpt-5.4",
  "instructions": "You are Claude Code, Anthropic's official CLI for Claude.",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": [{ "type": "input_text", "text": "hi" }]
    }
  ],
  "tools": [],
  "tool_choice": "auto",
  "parallel_tool_calls": true,
  "store": false,
  "stream": true,
  "include": [],
  "max_output_tokens": 32000,
  "temperature": 1,
  "top_p": 1,
  "client_metadata": {
    "anthropic_metadata": "{\"user_id\":\"...\"}",
    "anthropic_top_k": "0"
  },
  "text": {
    "format": {
      "type": "json_schema",
      "strict": true,
      "schema": { "type": "object", "properties": { "title": { "type": "string" } } },
      "name": "anthropic_output_config"
    }
  }
}
```

Notes:

- `store` must be **explicitly `false`** (backend contract).
- `instructions` must be **non-empty** for the Codex backend; if the Anthropic `system[]` is empty we default to
  `"You are a helpful assistant."`.
- `input[]` uses Codex protocol `ResponseItem` shapes (see `others/codex/codex-rs/protocol/src/models.rs`).

## 3) Side-by-side mapping (requests)

| Anthropic field | Codex backend field | Mapping rule |
|---|---|---|
| `model` | `model` | Pass-through unless listed in `providers.openai.unsupported_models`, in which case use `providers.openai.default_model` from gateway config. |
| `system[]` | `instructions` | Join text blocks with `\n\n`. If empty, default to `"You are a helpful assistant."`. |
| `messages[]` | `input[]` | Each Anthropic message becomes a Codex `ResponseItem::Message` (`type:"message"`). |
| `messages[].content[].text` | `content[]: {type:"input_text"/"output_text"}` | Role-dependent: user→`input_text`, assistant→`output_text`. |
| `messages[].content[].image` | `content[]: {type:"input_image"}` | User-only: base64 source → `image_url: data:<media_type>;base64,<data>`. |
| `tools[]` | `tools[]` | Tool schema → `{type:"function",name,description,defer_loading:true,parameters:<object-schema-subset>}` |
| `tool_choice` | `tool_choice` | Best-effort mapping. Absent→`"auto"`. |
| `output_config.format=json_schema` | `text.format=json_schema` | Best-effort: carry schema; set `strict:true`. |
| `output_config.effort` | `reasoning.effort` | Map `low/medium/high` 1:1. Map `max/xhigh` → `high`. Unknown → omit and record `client_metadata.anthropic_effort_unmapped`. |
| `max_tokens` | `client_metadata.anthropic_max_tokens` | Backend rejects `max_output_tokens`; record intent as metadata. |
| `temperature` | `temperature` | Best-effort pass-through (backend may ignore). |
| `top_p` | `top_p` | Best-effort pass-through (backend may ignore). |
| `top_k` | `client_metadata.anthropic_top_k` | No direct Codex field; record as metadata. |
| `metadata` | `client_metadata.anthropic_metadata` | JSON-stringify and pass as metadata. |
| `stop_sequences` | (ignored) | Explicitly ignored; see `UNSUPPORTED.md`. |

### Tool loop mapping (requests)

| Anthropic block | Codex backend item | Mapping rule |
|---|---|---|
| `tool_use` | `type:"function_call"` | `{call_id:id,name,arguments:JSON-stringified(input)}` |
| `tool_result` | `type:"function_call_output"` | `{call_id:tool_use_id,output:<string or content-items>}`; we currently send **plain string** output. |

## 4) Side-by-side mapping (streaming)

### Anthropic SSE (what Claude Code expects)

Key event types:

- `message_start`
- `content_block_start`
- `content_block_delta`
- `content_block_stop`
- `message_delta`
- `message_stop`
- `error`

### Codex backend SSE (what we consume)

Key event types we currently bridge:

- `response.output_text.delta` → text deltas
- `response.output_item.added` / `response.output_item.done` with `item.type=="function_call"` → tool call start
- `response.function_call_arguments.delta` → tool argument JSON deltas (buffered server-side)
- `response.completed` → message stop

### Streaming mapping rules

| Codex backend SSE | Anthropic SSE |
|---|---|
| `response.output_text.delta` | `content_block_delta` (index `0`, `text_delta`) |
| `response.output_item.*` (`function_call`) | `content_block_start` (tool_use; unique index per call_id) |
| `response.function_call_arguments.delta` | buffered server-side (not forwarded 1:1) |
| `response.completed` | `content_block_stop` + `message_delta(stop_reason=tool_use/end_turn)` + `message_stop` |

Validation performed:

- We buffer all tool-arg deltas; on completion we parse + validate they form a **JSON object**, apply deterministic tool-arg
  policies (e.g. drop `Read.pages` when empty or non-PDF), then emit a single sanitized `input_json_delta` to Claude Code.
  If parsing/validation fails, we emit an `error` event (so we don’t silently produce an invalid `tool_use.input`).

## 5) Gaps / information loss (current)

Even with the mappings above, there are still areas where fidelity is not fully 1:1:

- Not all Anthropic content-block types are preserved (future: thinking blocks, nested tool-result content items, etc.).
- `top_k` has no native Codex request field; we preserve it in `client_metadata` only.
- If the backend ignores `max_output_tokens` / `temperature` / `top_p`, the gateway cannot force sampling semantics.
- Some Anthropic request knobs (and Claude Code betas) do not have equivalent Codex backend fields.

## 6) Other APIs with better 1:1 support

If API-key billing is acceptable in the future, `https://api.openai.com/v1/responses` provides the authoritative,
documented Responses API surface (and generally better-defined request controls). That is **not** the current mode
of this gateway (subscription-only Codex backend).
