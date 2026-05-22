# Anthropic ↔ OpenAI Mapping (Gateway)

This document is the “source of truth” for:

- What payloads the gateway **accepts** on `POST /v1/messages` (Anthropic Messages-shaped).
- What payloads the gateway **sends upstream** (currently: ChatGPT Codex backend `/backend-api/codex/responses`).
- What a **1:1 mapping target** looks like if/when we move upstream to the public OpenAI **Responses API**.
- What does **not** map cleanly (gaps), and why.

Status note (2026-05-22):

- The gateway’s current implementation is **text-first**: it extracts `system` text into upstream `instructions`, and extracts only **user text** from `messages`.
- Claude Code sends **far more** than that (structured outputs, tool calling, multi-block content, etc.). Those are currently gaps and must be implemented for real “native Claude Code” fidelity.

---

## 1) Anthropic Messages payload sample (example + explanation)

### 1.1 Example request (representative Claude Code request)

This is representative of what Claude Code sends to the gateway (values abbreviated).

```jsonc
{
  // Required: model identifier (Claude Code decides this string)
  "model": "gpt-5.2",

  // Required: max tokens for the completion in Anthropic’s naming
  "max_tokens": 32000,

  // Required: conversation turns
  "messages": [
    {
      // "user" | "assistant"
      "role": "user",

      // Claude Code uses block arrays (not a single string)
      "content": [
        { "type": "text", "text": "hi" }
      ]
    }
  ],

  // Anthropic has a dedicated top-level system field (no "system" role in messages)
  "system": [
    { "type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude." },
    { "type": "text", "text": "Generate a concise title… Return JSON with {\"title\": ...}." }
  ],

  // Sampling (optional)
  "temperature": 1,

  // Tools (optional)
  "tools": [],

  // Request metadata (optional)
  "metadata": { "user_id": "{\"device_id\":\"…\",\"session_id\":\"…\"}" },

  // Claude Code frequently requests structured outputs (beta surface)
  "output_config": {
    "effort": "medium",
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "additionalProperties": false,
        "properties": { "title": { "type": "string" } },
        "required": ["title"]
      }
    }
  }

  // Sometimes present:
  // "stream": true
  // "tool_choice": { ... }
  // "top_p": 1
  // "top_k": 0
  // "stop_sequences": [ ... ]
  // "thinking": { ... } (beta)
}
```

### 1.2 What each major Anthropic field means (quick reference)

Top-level fields:

| Field | Meaning |
|---|---|
| `model` | Requested model name (client-facing). |
| `messages` | The conversation history as an array of turns. |
| `system` | System/instructions content blocks applied “above” messages. |
| `max_tokens` | Output token cap (Anthropic naming). |
| `stream` | If `true`, response is SSE streaming (events). |
| `temperature`, `top_p`, `top_k` | Sampling controls. |
| `stop_sequences` | Stop strings. |
| `tools` | Tool definitions usable by the model. |
| `tool_choice` | Tool selection policy (auto/required/specific tool). |
| `metadata` | User/session metadata; often `user_id`. |
| `output_config` | Output formatting request (e.g. JSON schema). |
| `thinking` | “Thinking/reasoning” configuration (beta; varies by feature set). |

Message + content:

| Path | Meaning |
|---|---|
| `messages[].role` | Who authored the turn (`user` / `assistant`). |
| `messages[].content` | Either a string or an array of “content blocks”. Claude Code uses blocks. |
| `system[]` | Array of system content blocks (most commonly `type:"text"`). |

Content blocks (conceptual):

| Block `type` (examples) | Meaning |
|---|---|
| `text` | Plain text. |
| `tool_use` | Model invokes a tool (name + input). |
| `tool_result` | Tool output returned to model. |
| `image` | Image input block (when supported). |
| `thinking` | Model reasoning/thinking content (beta; shape varies). |

---

## 2) OpenAI API payload sample (example + explanation)

### 2.1 Example OpenAI **Responses** request (closest 1:1 target)

This is what we would ideally produce when translating Anthropic Messages → OpenAI Responses.

```jsonc
{
  // Required
  "model": "gpt-5.2",

  // System-level instructions (closest equivalent to Anthropic top-level system)
  "instructions": "You are Claude Code, Anthropic's official CLI for Claude.\n\nGenerate a concise title… Return JSON with {\"title\": ...}.",

  // Full conversation history as input items (multi-part inputs are supported)
  "input": [
    {
      "role": "user",
      "content": [
        { "type": "input_text", "text": "hi" }
      ]
    }
  ],

  // Closest equivalent to Anthropic max_tokens
  "max_output_tokens": 32000,

  // Sampling (optional)
  "temperature": 1,

  // Tools (optional)
  "tools": [],
  "tool_choice": "auto",
  "parallel_tool_calls": true,

  // Structured outputs: JSON schema (optional)
  "text": {
    "format": {
      "type": "json_schema",
      "name": "session_title",
      "schema": {
        "type": "object",
        "additionalProperties": false,
        "properties": { "title": { "type": "string" } },
        "required": ["title"]
      },
      "strict": true
    }
  },

  // Metadata (optional)
  "metadata": {
    "anthropic_metadata.user_id": "{\"device_id\":\"…\",\"session_id\":\"…\"}"
  },

  // Streaming (optional)
  "stream": true,

  // Storage policy (important; see Day-15 runtime error)
  "store": false
}
```

### 2.2 What each major OpenAI Responses field means (quick reference)

| Field | Meaning |
|---|---|
| `model` | Backend model identifier. |
| `instructions` | System-level instructions. |
| `input` | Conversation input items (multi-part inputs supported). |
| `max_output_tokens` | Output token cap (OpenAI naming). |
| `temperature`, `top_p` | Sampling controls. |
| `tools` | Tool definitions. |
| `tool_choice` | Tool selection policy. |
| `parallel_tool_calls` | Whether tools can be called in parallel. |
| `text.format` | Output formatting (text vs JSON schema). |
| `metadata` | Opaque metadata. |
| `stream` | If true: SSE streaming events. |
| `store` | Whether OpenAI stores the response (policy + backend contracts may require `false` for some internal endpoints). |
| `previous_response_id` | Conversation threading (when using response IDs). |

---

## 3) Side-by-side detailed mapping + gaps

### 3.1 High-level mapping table (Anthropic → OpenAI Responses)

This table is the intended 1:1 translation target.

| Anthropic (Messages) | OpenAI (Responses) | Mapping rule | Status in gateway today |
|---|---|---|---|
| `model` | `model` | alias/passthrough | Implemented via model map |
| `system[]` | `instructions` | join system text blocks | Implemented (text only) |
| `messages[]` | `input[]` | preserve full history and content blocks | Not implemented (text-only extraction) |
| `max_tokens` | `max_output_tokens` | rename | Not implemented |
| `stream` | `stream` | passthrough | Implemented (SSE adapter exists) |
| `temperature` | `temperature` | passthrough | Not implemented |
| `top_p` | `top_p` | passthrough | Not implemented |
| `top_k` | (no direct field) | compatibility policy | Gap |
| `stop_sequences` | (no direct single field) | compatibility policy | Gap |
| `tools[]` | `tools[]` | schema translation | Not implemented |
| `tool_choice` | `tool_choice` | translate union types | Not implemented |
| `output_config.format` | `text.format` | translate json_schema | Not implemented |
| `metadata` | `metadata` / `user` | propagate user/session | Not implemented |
| `thinking` | reasoning config | translate if possible | Not implemented |

### 3.2 What we actually do today (Anthropic → ChatGPT Codex backend)

Today, `/v1/messages` goes to an internal endpoint that is “Responses-like” but not identical. The gateway currently constructs:

```jsonc
{
  "model": "<resolved backend model>",
  "instructions": "<system text joined>",

  // Required for this backend: it rejects if omitted or not false
  "store": false,

  // We always consume SSE from the backend
  "stream": true,

  "input": [
    {
      "role": "user",
      "content": [{ "type": "input_text", "text": "<extracted user text>" }]
    }
  ]
}
```

What is lost today:

- All non-text blocks (images, tool blocks, thinking blocks, nested tool results).
- All message turns except user text.
- Structured output (`output_config`).
- Tool schema/choice (`tools`, `tool_choice`).
- Sampling, stopping, metadata, max token caps.

---

## 4) Gap deep-dive (what does not map, and why)

This section is the “why” behind the gaps, and what we must do to fix them.

### 4.1 Content blocks + indices (streaming correctness)

Problem:

- Anthropic streaming is *block-indexed* (`index` identifies which content block is being updated).
- OpenAI streaming is *output-indexed* (typically `output_index` + `content_index`).
- Our current adapter hardcodes `index: 0` and treats everything as text deltas.

Fix:

- Track upstream output parts and map them to Anthropic content blocks with stable indices.
- Preserve content block lifecycle: start/delta/stop per block, not “one giant text”.

### 4.2 Tools (tool_use/tool_result)

Problem:

- Anthropic and OpenAI both support tool calling, but the wire formats differ.
- Claude Code relies on tools and tool results to work like a “native session”.
- We currently drop tool blocks entirely.

Fix:

- Translate tool schemas (Anthropic tool schema → OpenAI tool schema).
- Translate tool invocation events (arguments deltas, tool result blocks) both directions.

### 4.3 Structured outputs (JSON schema)

Problem:

- Claude Code uses `output_config.format.type = "json_schema"` for some tasks (session title generation is a common example).
- If we don’t forward a comparable structured-output constraint upstream, we will get format drift, backend errors, or unusable outputs.

Fix:

- Map Anthropic `output_config.format` to OpenAI `text.format` with `json_schema`.
- Translate structured-output validation errors back into Anthropic-style errors.

### 4.4 Sampling / stopping / token caps

Problem:

- Anthropic has `top_k`; OpenAI does not have a direct equivalent in Responses.
- `stop_sequences` does not have a single universal Responses field that matches Anthropic semantics 1:1 in all cases.

Fix:

- Explicit compatibility policy:
  - `top_k`: ignore with explicit warning/telemetry, or approximate via other controls if safe.
  - `stop_sequences`: emulate via instructions and/or post-processing only if that doesn’t break correctness; otherwise declare unsupported.

### 4.5 Errors + usage fidelity (including “why do we only surface errors[0]?”)

Problem:

- The gateway currently does not preserve full structured error arrays and per-field errors; it tends to return one flattened error message.
- This makes debugging Claude Code flows harder.

Fix:

- Preserve:
  - upstream status + error type,
  - complete error detail payload (including multiple error entries),
  - request id / proxy request id,
  - and map to Anthropic `error` shapes without collapsing.

---

## 5) Any other API with better support?

Yes:

1) **OpenAI Responses API (public)** is the best long-term target for faithful mapping:
   - Supports multi-part inputs/outputs, tools, structured outputs, and rich streaming events.
   - It’s the closest conceptual match to Anthropic Messages.

2) **OpenAI Chat Completions API** is generally a worse fit for Claude Code-style fidelity:
   - It’s less event-rich and tends to be more “single text stream”-oriented.
   - Tool + structured-output support exists, but the semantics and streaming detail are not as alignment-friendly as Responses.

3) The current **ChatGPT Codex backend** is a pragmatic bootstrap but has quirks:
   - Requires `instructions` and `store:false` (as we’ve already hit in runtime errors).
   - It’s not guaranteed to expose the full stable surface we need for “Anthropic 1:1”.

Recommendation:

- Keep the current Codex backend path as the short-term adapter while we implement missing mappings, but plan a migration of `/v1/messages` to OpenAI `POST /v1/responses` for full fidelity once we can authenticate with an API key (or otherwise obtain an OpenAI API credential) and once the mapping layer is complete.
