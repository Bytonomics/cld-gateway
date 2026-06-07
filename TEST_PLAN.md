# Claude Code → OpenAI Gateway — Manual Test Plan

Goal: manually verify that Claude Code can talk to your gateway as if it were Anthropic, while the gateway translates requests/responses to/from OpenAI. This plan emphasizes **tool use**, **streaming**, and **edge-case fidelity**.

## Status (✅ done / ❌ not done)

> Update this table as you run each test in Claude Code.

| Area | Test | Status | Notes |
|---|---|---:|---|
| Harness/tools (this session) | Create TEST_PLAN.md via Write tool | ✅ | `Write` succeeded (file created). |
| Harness/tools (this session) | Verify file contents via Read tool | ✅ | `Read` succeeded for `TEST_PLAN.md`. |
| Harness/tools (this session) | Read huge JSONL line | ❌ | `Read` fails when a single line exceeds 25k token cap. |
| Gateway proxy | 1.1 Non-streaming single turn | ✅ | Observed request_id `d508df76-c84c-412f-94b2-9691ad7cfa91` → `POST /v1/messages?beta=true` returned 200; response `content-type: text/event-stream`. |
| Gateway proxy | 1.2 Multi-turn conversation | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.1 Tool call: list files | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.2 Tool call w/ args: grep/search + open files | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.3 Multiple tool calls in one assistant turn | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.4 Tool error propagation + recovery | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.5 Large tool output handling | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.6 Forced tool usage | ❌ | Not run yet through the gateway. |
| Gateway proxy | 2.7 Tool suppression (no tools) | ❌ | Not run yet through the gateway. |
| Gateway proxy | 3.1 Streaming text | ❌ | Not run yet through the gateway. |
| Gateway proxy | 3.2 Streaming tool calls | ❌ | Not run yet through the gateway. |
| Gateway proxy | 4.1 Auth error mapping | ❌ | Not run yet through the gateway. |
| Gateway proxy | 4.2 Rate limit mapping | ❌ | Not run yet through the gateway. |
| Gateway proxy | 4.3 Upstream 5xx mapping | ❌ | Not run yet through the gateway. |
| Gateway proxy | 4.4 Mid-stream disconnect handling | ❌ | Not run yet through the gateway. |
| Gateway proxy | 5.1 Usage present/well-typed | ❌ | Not run yet through the gateway. |
| Gateway proxy | 5-B Context overflow behavior | ❌ | Not run yet through the gateway. |
| Gateway proxy | 6.1 Vision image input | ❌ | Only if you support vision in the gateway. |
| Gateway proxy | 7.1 Concurrency (two sessions) | ❌ | Not run yet through the gateway. |

## Session progress log (what we have actually tested so far)

### Completed
- ✅ Created `/Users/tushar/Documents/Bytonomics/gateway/TEST_PLAN.md` using `Write`.
- ✅ Verified `Read` works on normal-sized text files by reading `TEST_PLAN.md`.

### Not completed / failures observed (this session)
- ❌ `Read` fails on extremely large single-line JSONL entries (exceeded 25k token cap), even with `limit: 1`.
- ❌ `Read` tool call validation fails if `pages` is present but invalid (e.g., `pages: ""`). (This is a harness-level tool validation issue, not a gateway/OpenAI HTTP issue.)

---

## 0) Prerequisites / Setup

### 0.1 Confirm Claude Code is routed through your gateway
- Configure Claude Code to use your gateway base URL and auth method (whatever your gateway requires).
- Ensure you have **gateway logs** enabled with:
  - inbound request: method, path, headers (redacted), body
  - outbound OpenAI request: path, headers (redacted), body
  - inbound OpenAI response: status, headers, body / stream events
  - outbound response back to Claude Code: status, headers, body / stream events
  - request id correlation (a single id across the whole chain)

### 0.2 Baseline: record the “contract” you intend to support
Document (in your own notes) what you currently support:
- Anthropic-style endpoints supported (e.g., `/v1/messages`)
- Streaming supported? (yes/no)
- Tool calling supported? (yes/no)
- Vision/image blocks supported? (yes/no)
- Known limitations (payload size, max tokens, etc.)

## 1) Basic non-tool chat (sanity)

### Test 1.1 — Non-streaming single turn
**Prompt in Claude Code:**
> Reply with exactly 3 bullet points describing what you can do.

**Verify in Claude Code:**
- You get an assistant reply (not an error)

**Verify in gateway logs:**
- Anthropic request shape received
- OpenAI request shape produced
- Response translated back correctly

### Test 1.2 — Multi-turn conversation
**Prompts:**
1) `My favorite language is Go. Say “ok” only.`
2) `What’s my favorite language?`

**Verify:**
- Conversation state is preserved via messages history (or your mapping)

## 2) Tool use (manual end-to-end)

The purpose is to validate the full tool loop:
1) Claude Code sends tools schema to gateway
2) Gateway maps tools schema to OpenAI tool schema
3) Model emits tool call(s)
4) Gateway maps tool call(s) back to Anthropic tool_use format
5) Claude Code executes tool(s) locally and returns tool_result
6) Gateway maps tool_result to OpenAI tool output format
7) Model consumes tool output and produces final text

### What to log/inspect for EVERY tool test
- Inbound Anthropic request contains a `tools` list (names, JSON schema)
- Outbound OpenAI request contains mapped `tools`
- Outbound response to Claude Code contains **tool request**, not plain text
- Tool call id mapping is consistent:
  - Anthropic tool_use `id` ↔ OpenAI tool call id
- Follow-up request from Claude Code includes `tool_result` referencing correct id

### Extra (recommended): tool-call invariants (proxy correctness)
Validate these invariants in your gateway logs for each tool-use turn:
- **Tool schema fidelity:** names/descriptions/JSON schemas survive translation (no dropped required fields).
- **ID mapping:** every tool call has a stable id, and every tool_result references the correct id.
- **Ordering:** tool_use blocks remain in the same order as emitted.
- **Multi-tool support:** if the model emits N tool calls, Claude Code is able to execute N tools and your gateway forwards all N results.
- **Error shape:** tool failures are returned as tool_result content (not as a gateway 5xx).
- **No hallucinated tools:** the model must never call a tool that was not advertised in the inbound `tools` list.

### Test 2.1 — Single tool call: list files
**Prompt:**
> Use tools to list files in the current directory. Then tell me the 5 most relevant files for understanding this project.

**Pass criteria:**
- Claude Code actually runs a tool
- The assistant reply references tool output

**Common failures:**
- Tool call returned as text (Claude Code doesn’t execute anything)
- Tool ids don’t match; model ignores tool_result

### Test 2.2 — Tool call with arguments: grep/search
**Prompt:**
> Use tools to search for “OPENAI”, “ANTHROPIC”, and “base_url” in this repo. Open the most relevant 2 files and summarize how routing works.

**Pass criteria:**
- Search tool invoked with correct arguments
- Subsequent file-read tools invoked
- Summary matches actual file contents

### Test 2.3 — Multiple tool calls in one assistant turn
**Prompt:**
> Do these via tools: (1) list files, (2) search for “gateway”, (3) search for “tool_choice”. Combine results.

**Pass criteria:**
- If the model returns multiple tool calls at once, Claude Code executes all
- Your gateway returns all tool calls (doesn’t drop after first)

### Test 2.4 — Tool error propagation + recovery
**Prompt:**
> Use a tool to read a file that does not exist: `./__definitely_not_real__`. Then recover by listing files and reading a real file named similarly (or closest match).

**Pass criteria:**
- Tool error is returned to the model as a tool_result (not as a 500)
- Model recovers and continues

### Test 2.5 — Large tool output handling
**Prompt:**
> Find the largest text file in the repo using tools, read it, and summarize the most important sections. If it’s too large, chunk reads and continue.

**Pass criteria:**
- No gateway crash / JSON parse errors
- Model can iteratively request more tool reads

### Test 2.6 — Forced tool usage (tool_choice semantics)
**Prompt:**
> You must use tools for this: determine the current working directory and list files. Do not answer without tool output.

**Pass criteria:**
- Model issues tool calls (doesn’t answer directly)

### Test 2.7 — Tool suppression (no tools)
**Prompt:**
> Do NOT use any tools. Explain what “tool use” means in Claude Code at a high level.

**Pass criteria:**
- No tool calls are emitted

## 3) Streaming (SSE) fidelity

### Test 3.1 — Streaming text
**Prompt:**
> Stream your response. Write a 30-line numbered list (1..30), one item per line.

**Pass criteria:**
- Claude Code shows incremental output (not buffered)
- Stream ends cleanly (no hang)

**Gateway checks:**
- Correct SSE framing
- No invalid partial JSON per SSE event

### Test 3.2 — Streaming tool calls
**Prompt:**
> Stream your response. First decide what tool you need, then call it to list files, then continue streaming a summary.

**Pass criteria:**
- Tool call is emitted during streaming in a way Claude Code accepts
- Tool_result is processed and streaming continues

## 4) Error mapping

Run each test and confirm Claude Code receives a meaningful error (status + message), and your gateway logs show correct translation.

### Test 4.1 — Auth error
- Temporarily use an invalid key/token.

**Pass criteria:**
- Claude Code gets 401/403 style error (not a confusing 500)

### Test 4.2 — Rate limit
- Trigger or simulate 429 from upstream.

**Pass criteria:**
- Claude Code gets 429 with a useful message; optional Retry-After preserved

### Test 4.3 — Upstream 5xx
- Simulate OpenAI 500/502.

**Pass criteria:**
- Claude Code gets a mapped error; gateway doesn’t crash

### Test 4.4 — Mid-stream disconnect
- Kill upstream connection during streaming.

**Pass criteria:**
- Claude Code doesn’t hang forever; gateway terminates stream with error

## 5) Usage / token accounting

### Test 5.1 — Usage present
**Prompt:**
> Reply with a short paragraph.

**Pass criteria:**
- If your gateway emits usage, it is consistent and well-typed
- If you don’t emit usage, ensure Claude Code doesn’t break

### Test 5-B — Context overflow behavior
**Prompt:**
> Here is a very long text: [paste large text]. Summarize.

**Pass criteria:**
- Predictable error or truncation behavior; no malformed JSON

## 6) Vision (only if supported)

### Test 6.1 — Image input
- Provide an image to Claude Code (if that workflow is available).

**Pass criteria:**
- Image block mapping works end-to-end
- Model response references image content

## 7) Concurrency / parallel requests

### Test 7.1 — Two sessions at once
- Run two Claude Code prompts in parallel terminals.

**Pass criteria:**
- No cross-talk between streams
- Tool ids don’t collide across requests

## 8) Completion criteria (“proxy is complete”)

Declare “complete” when:
- Non-streaming chat works reliably
- Streaming text works reliably
- Tool calling loop works end-to-end for:
  - single tool call
  - multi-step tool usage
  - multiple tool calls
  - tool error + recovery
  - large tool outputs
- Error mapping is predictable for 401/403, 429, 5xx, and mid-stream disconnect
- Logs/trace ids allow debugging any failure

---

## Notes section (fill during testing)
- Observed endpoints from Claude Code:
- Observed Anthropic request fields that appear:
- Any mismatches / failures:
- Fixes made and re-test results:
