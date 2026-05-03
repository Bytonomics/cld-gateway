# Anthropic-Compatible Proxy Backed by Codex / ChatGPT OAuth: Design Doc

## Goal

Build a local proxy that:

- authenticates the user via Codex / ChatGPT OAuth
- exposes an API surface compatible with Anthropic-style clients
- internally routes requests to OpenAI-backed authenticated model access
- preserves tool-calling behavior closely enough for clients such as OpenCode
- can act as a drop-in endpoint for tools expecting Anthropic-compatible chat + tool use semantics

This document is strictly about a **proxy architecture**, not a native coding CLI.

---

# 1. Problem Definition

You want a proxy that behaves like this:

## External interface

- looks like Anthropic enough that Anthropic-oriented clients can talk to it
- supports messages-style request/response flow
- supports streaming
- supports tool calls in Anthropic-compatible format
- supports tool results coming back from the client in Anthropic-compatible format
- ideally supports the subset of Anthropic semantics that OpenCode or similar tools actually rely on

## Internal behavior

- authenticates through Codex / ChatGPT OAuth, not API keys
- uses that authenticated OpenAI/Codex session to obtain model responses
- maps Anthropic-style chat/tool flows to OpenAI/Codex-side equivalents
- tracks state needed to bridge the two protocols

## Key requirement

This is not just an LLM text proxy. It must preserve the **tool invocation loop** so that the client can continue operating as an agentic coding tool.

---

# 2. Product Shape

The product is a **local HTTP proxy server**.

## Responsibilities

1. Start and maintain authenticated Codex / ChatGPT session state
2. Expose Anthropic-compatible endpoints
3. Translate requests from Anthropic schema to OpenAI/Codex-side schema
4. Translate tool-use outputs back into Anthropic-compatible blocks
5. Handle tool-result turns from the client and continue the conversation
6. Stream partial outputs in a client-compatible way
7. Optionally map model names from Anthropic-style names to your chosen Codex-backed models

## Non-goals for v1

- full Anthropic API parity
- billing/account management
- generic multi-provider routing
- full compatibility with every Anthropic client on day one

The target is **the Anthropic subset needed by OpenCode or similar clients**.

---

# 3. High-Level Architecture

```text
Anthropic-compatible client (e.g. OpenCode)
            |
            v
   Local Proxy HTTP Server
            |
   -------------------------
   | Auth Manager          |
   | Session Manager       |
   | Request Translator    |
   | Tool Call Translator  |
   | Streaming Translator  |
   | State Store           |
   -------------------------
            |
            v
   Codex / ChatGPT OAuth-authenticated OpenAI path
```

## Main subsystems

- Auth Manager
- Session Manager
- Anthropic API Adapter
- OpenAI/Codex Backend Adapter
- Tool Translation Layer
- Streaming Layer
- Conversation State Store
- Model Mapper
- Diagnostics / Logging

---

# 4. Authentication Design

## Objective

The proxy must log the user in through **Codex / ChatGPT OAuth** and then reuse that authenticated session for backend requests.

## Likely practical mechanism

The most concrete public clues from current tools are:

- Codex supports browser-based login via `codex login`
- Cline explicitly imports credentials from `~/.codex/auth.json`
- Roo explicitly signs in through OpenAI Codex

So the proxy should be designed around **Codex-authenticated local session state**.

## Proposed auth flow

1. On startup, proxy checks whether Codex auth session exists locally
2. If not authenticated, proxy instructs the operator to complete Codex login
3. Proxy loads Codex-authenticated local session metadata
4. Proxy establishes its own runtime session over that authenticated identity
5. Proxy refreshes or rehydrates session state as required

## Auth subsystem responsibilities

- discover Codex auth/session files or token source
- validate current login state
- detect expiration or invalid session
- surface a local status endpoint such as `/auth/status`
- support a local command or admin endpoint to trigger login bootstrap

## Important design constraint

Do not make auth assumptions throughout the rest of the system. The rest of the proxy should depend only on an abstract authenticated backend client.

---

# 5. Anthropic-Compatible API Surface

## Minimum endpoints to support

For a first practical version, implement the subset used by agentic coding clients.

### Likely core endpoint

- `POST /v1/messages`

### Optional but useful

- `GET /v1/models`
- health endpoint such as `/health`
- auth status endpoint such as `/auth/status`

## `POST /v1/messages` must support

- system prompt input
- user/assistant/tool message history
- tools array
- tool\_choice or equivalent behavior if client sends it
- streaming and non-streaming modes
- multi-turn continuation with tool results

## Response shape

The proxy should emit Anthropic-style responses closely enough that OpenCode does not need custom logic.

That means preserving concepts such as:

- assistant content blocks
- `tool_use` blocks
- stop reasons related to tool use
- message IDs if required by client expectations
- streaming event shapes if the client consumes SSE-like Anthropic events

---

# 6. Protocol Translation

This is the core of the project.

## 6.1 Input translation

Translate from Anthropic-style message schema into the OpenAI/Codex-side schema.

### Required mappings

- Anthropic system prompt -> backend system/developer instruction shape
- Anthropic message list -> backend message/input list
- Anthropic tools schema -> backend tools/functions schema
- Anthropic tool choice -> backend tool selection directive
- Anthropic max\_tokens / model / temperature -> backend equivalents if supported

## 6.2 Output translation

Translate backend responses into Anthropic-compatible response blocks.

### Required mappings

- plain text output -> Anthropic `text` content block
- backend tool/function call -> Anthropic `tool_use` content block
- backend stop reason -> Anthropic-compatible stop reason
- backend IDs -> proxy-generated Anthropic-like IDs where needed

## 6.3 Tool result translation

When the client sends tool results back:

- parse Anthropic `tool_result` blocks
- map them into backend tool-result continuation format
- continue the same conversation statefully

This is essential. Without this, the proxy is not a real agent proxy.

---

# 7. Tool Call Compatibility

## Primary requirement

The proxy must preserve enough of Anthropic’s tool-calling semantics that a client like OpenCode can continue its normal loop:

1. client sends messages + tool definitions
2. model decides to call a tool
3. proxy returns Anthropic-compatible `tool_use`
4. client executes the tool
5. client sends back Anthropic-compatible `tool_result`
6. proxy continues the conversation with the backend model

## Translation responsibilities

### Anthropic tool definition -> backend tool definition

Map:

- tool name
- description
- JSON schema / input schema

### Backend function/tool call -> Anthropic `tool_use`

Must produce:

- stable tool-use ID
- tool name
- structured input payload

### Anthropic `tool_result` -> backend tool result

Must map the result back to the exact prior tool call context.

## State you must track

For every tool call:

- conversation ID
- turn ID
- anthropic-style tool-use ID
- backend tool-call ID if one exists
- tool name
- input payload
- completion status

Without this state mapping, multi-turn tool usage will break.

---

# 8. Streaming Design

## Requirement

Many coding clients rely on incremental output and sometimes on event-structured streams.

## Proxy streaming responsibilities

- accept streaming request from client
- consume backend streaming response
- translate backend deltas into Anthropic-compatible stream events
- emit tool-use deltas or final tool-use block in client-compatible order
- terminate stream cleanly with correct stop event / stop reason

## Difficulty

Streaming translation is usually harder than plain response translation because:

- event boundaries differ between protocols
- tool-call emission timing may differ
- partial JSON arguments may arrive incrementally
- the client may assume Anthropic-style event ordering

## Practical v1 guidance

If OpenCode supports non-streaming fallback, first make non-streaming tool calls correct. Then add streaming once the state machine is stable.

---

# 9. Conversation State Model

Because the two protocols are not identical, the proxy must maintain its own bridge state.

## Required entities

### Session

- id
- auth\_state\_ref
- backend\_identity\_ref
- started\_at
- expires\_at

### Conversation

- id
- client\_conversation\_key if any
- backend\_conversation\_key if any
- created\_at
- updated\_at

### Turn

- id
- conversation\_id
- request\_payload
- translated\_backend\_payload
- response\_payload
- stop\_reason
- created\_at

### ToolInvocation

- id
- conversation\_id
- turn\_id
- anthropic\_tool\_use\_id
- backend\_tool\_call\_id
- tool\_name
- input\_json
- output\_json
- state

## Persistence

Use a small local persistent store. Suitable options:

- SQLite
- BoltDB / bbolt
- plain JSON files for early prototype only

SQLite is the most practical choice for debugging and replay.

---

# 10. Model Mapping

The client may request Anthropic model names even though the backend is OpenAI/Codex-authenticated.

## Therefore the proxy needs model aliasing

Example conceptually:

- `claude-3-5-sonnet` -> chosen Codex/OpenAI-backed model alias
- `claude-3-7-sonnet` -> another backend alias

## Responsibilities

- accept client-requested Anthropic model name
- map it to configured backend target
- optionally expose these aliases through `/v1/models`

## Recommendation

Keep this mapping configurable.

Example config shape:

```yaml
model_aliases:
  claude-3-5-sonnet: codex-default
  claude-3-7-sonnet: codex-high
  claude-3-haiku: codex-fast
```

---

# 11. OpenCode Compatibility Target

## Goal

Be compatible enough that OpenCode can treat this proxy as an Anthropic-like backend.

## What to verify in OpenCode repo

- exact endpoint(s) it calls for Anthropic provider
- exact request shape it sends
- whether it depends on streaming by default
- whether it expects Anthropic `tool_use` / `tool_result` literally
- whether it requires model listing
- whether it performs strict provider validation
- whether it assumes specific stop reasons or event names

## Compatibility strategy

Do not aim for abstract Anthropic compatibility first. Aim for **OpenCode’s real Anthropic integration path** first.

That means your proxy contract should be tested directly against OpenCode behavior.

---

# 12. Suggested HTTP Endpoints

## Public proxy endpoints

- `POST /v1/messages`
- `GET /v1/models`
- `GET /health`
- `GET /auth/status`
- `POST /admin/reload-config`

## Optional admin endpoints

- `POST /admin/login/check`
- `GET /admin/conversations/:id`
- `GET /admin/tool-invocations/:id`
- `POST /admin/replay/:turnId`

These admin endpoints help you debug protocol mismatches.

---

# 13. Internal Modules

## 13.1 Auth Manager

Responsibilities:

- check Codex login state
- load authenticated local session material
- refresh/recover session
- present backend-ready auth context

## 13.2 Anthropic Adapter

Responsibilities:

- parse `/v1/messages`
- validate Anthropic-compatible payloads
- normalize input into internal canonical form

## 13.3 Backend Adapter

Responsibilities:

- call the authenticated backend using Codex/OpenAI session context
- expose a normalized internal response format
- hide backend-specific quirks from the rest of the system

## 13.4 Tool Bridge

Responsibilities:

- map tool schemas
- map outgoing tool calls
- map incoming tool results
- preserve IDs and ordering

## 13.5 Stream Bridge

Responsibilities:

- translate event stream from backend to Anthropic-style SSE or chunk format
- manage partial tool-call argument assembly

## 13.6 State Store

Responsibilities:

- persist conversation state
- persist tool mapping state
- enable replay/debugging

## 13.7 Config Manager

Responsibilities:

- load model alias config
- load backend mode config
- load compatibility toggles

---

# 14. Critical Technical Unknowns

These are the exact areas you need to inspect in the open-source repos.

## 14.1 Codex auth reuse details

You need to verify:

- where Codex stores auth/session state
- what fields exist in `~/.codex/auth.json`
- whether additional files are involved
- how refresh is handled
- whether reuse requires Codex CLI to remain installed

## 14.2 Cline implementation details

You need to verify:

- where Cline imports Codex credentials
- whether it shells out to Codex login or only reuses stored auth
- whether it talks directly to Codex-side APIs after auth import
- how it refreshes or validates session state

## 14.3 Roo implementation details

You need to verify:

- how Roo launches the Codex sign-in flow
- whether Roo stores separate tokens or simply interoperates with Codex session state
- how backend requests are routed after sign-in

## 14.4 OpenCode Anthropic expectations

You need to verify:

- exact request schema
- exact stream expectations
- exact tool-call loop requirements
- failure behavior when stop reasons or event names differ

---

# 15. Recommended Repository Investigation Targets

## Repos to inspect

- `https://github.com/openai/codex`
- `https://github.com/cline/cline`
- `https://github.com/RooCodeInc/Roo-Code`
- `https://github.com/anomalyco/opencode`
- `https://github.com/numman-ali/opencode-openai-codex-auth`
- `https://github.com/openclaw/openclaw`

## Search terms to grep first

- `auth.json`
- `~/.codex`
- `codex login`
- `oauth`
- `chatgpt`
- `plus/pro`
- `tool_use`
- `tool_result`
- `anthropic`
- `messages`
- `stream`
- `sse`
- `function_call`
- `tool_call`

---

# 16. Suggested MVP

## MVP objective

A local proxy that works with one target client, preferably OpenCode’s Anthropic provider path, including tool calls.

## MVP features

- Codex-authenticated startup bootstrap
- `POST /v1/messages`
- non-streaming response mode first
- Anthropic-compatible text responses
- Anthropic-compatible outgoing `tool_use`
- Anthropic-compatible incoming `tool_result`
- conversation state persistence
- model alias mapping
- structured debug logging

## Deferred until after MVP

- full streaming parity
- support for every Anthropic endpoint
- multi-client compatibility matrix
- advanced admin UI

---

# 17. Failure Modes

## 17.1 Auth-state fragility

If you rely on Codex local session files and their format changes, the proxy can break.

## 17.2 Tool ID mismatch

If Anthropic tool-use IDs and backend tool-call IDs are not bridged correctly, tool continuation will fail.

## 17.3 Stream mismatch

Even if non-streaming works, streaming can fail because of delta ordering or event-shape assumptions.

## 17.4 Schema mismatch

OpenCode may rely on Anthropic-specific fields you did not initially copy.

## 17.5 Backend semantic mismatch

OpenAI/Codex-side tool calling may not align perfectly with Anthropic sequencing, so the proxy may need extra normalization logic.

---

# 18. Suggested Implementation Order

## Phase 1: auth and backend probing

- verify Codex login and discover local session artifacts
- build tiny auth-status utility inside the proxy
- build backend adapter skeleton

## Phase 2: plain Anthropic message translation

- implement `POST /v1/messages`
- support system + user + assistant history
- return plain text response correctly

## Phase 3: tool-call translation

- accept Anthropic tools array
- translate backend tool call into Anthropic `tool_use`
- accept Anthropic `tool_result`
- continue conversation successfully

## Phase 4: OpenCode validation

- point OpenCode at the proxy
- fix schema mismatches until basic agent loop works

## Phase 5: streaming

- add Anthropic-compatible stream output
- align event ordering with client expectations

---

# 19. Concrete Success Criteria

The proxy is successful when:

1. user authenticates through Codex / ChatGPT OAuth
2. OpenCode can be pointed at the proxy as if it were Anthropic
3. OpenCode can send messages with tools
4. proxy returns a valid Anthropic-style `tool_use`
5. OpenCode executes the tool and sends back `tool_result`
6. proxy continues the conversation correctly
7. no API key is required

---

# 20. Immediate Next Steps

1. Clone the listed repos.
2. Confirm how Codex auth state is stored and reused.
3. Inspect OpenCode’s Anthropic client implementation first, because that defines the real compatibility target.
4. Build a tiny local proxy skeleton with:
   - `/health`
   - `/auth/status`
   - stubbed `/v1/messages`
5. Implement non-streaming text translation first.
6. Then implement tool-call and tool-result bridging.

---

# Final Scope Statement

This project is a **Codex/ChatGPT OAuth-authenticated Anthropic-compatible proxy**, not a native CLI. Its purpose is to let Anthropic-oriented agent clients, especially OpenCode, talk to an OpenAI-backed authenticated model path while preserving tool-calling behavior closely enough for real coding-agent workflows.

