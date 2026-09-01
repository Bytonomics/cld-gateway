# AI_SLOP.md

Heuristics in this repo that make decisions from literal prompt text.
Rule: deterministic checks on structured command tags are fine. Any decision
that reads system/user prompt wording is a bug. This slop was created by Codex.

Status: kept for reference. Do not port to Go. Remove from Rust when convenient.

## Slop 1 — Read-only side-question detection

- File: `crates/gateway-http-anthropic/src/claude_code_inclusion.rs:8-12`
- Used at: `claude_code_inclusion.rs:318` via `is_read_only_request` (`:337-339`)
- Matches all three literal phrases in user text:
  - `This is a side question from the user`
  - `separate, lightweight agent`
  - `The main agent is NOT interrupted`
- Effect: marks request `read_only`, which becomes `LocalControl` request kind.
- Bug: pasting those sentences into a normal request silently demotes it.

## Slop 2 — Transient internal-request classifier

- File: `crates/gateway-http-anthropic/src/lib.rs:4444-4466`
- Function: `classify_transient_internal_request`
- Matches literal text in messages:
  - `<transcript>`
  - `specific action under review`
  - `claude.md configuration`
  - `<block>`
  - `authorize`
  - `permission`
- Effect: returns `PermissionClassifier` request kind.
- Bug: `authorize` and `permission` are common words. Short non-streaming
  requests that quote a transcript and the word "permission" misclassify.

## Slop 3 — Subagent detection from system prompt

- File: `crates/gateway-http-anthropic/src/lib.rs:4488-4489`
- Function: `classify_conversation_request`
- Matches: `system_text.contains("cc_is_subagent=true")`
- Effect: returns `SubagentOffshoot` request kind.
- Bug: reads system prompt text. The signal belongs in request metadata.

## Slop 4 — SDK/skills-list detection from prompt text

- File: `crates/gateway-http-anthropic/src/lib.rs:4504-4509`
- Function: `classify_conversation_request`
- Matches:
  - system text contains `claude agent sdk`
  - message text contains `the following skills are available for use with the skill tool`
- Effect: returns `UnknownOffshoot` request kind.
- Bug: pure prompt-wording sniffing.

## Why this matters

All four feed `classify_conversation_request` (`lib.rs:4468-4513`), which sets
`ConversationRequestKind`. That kind is part of `ConversationTransportIdentity.key()`
(`lib.rs:217-226`). One misclassification routes the turn to the wrong branch,
persistence scope, and WebSocket session.

## Replacement direction (owner decisions, 2026-08-22)

Rules (mirrored in CLAUDE.md, binding):
1. NEVER depend on prompt text — not even if it seems like the only way.
2. NEVER depend on message size/shape heuristics either (small/large
   message count, short request, role-mix proxies) — same dependence in
   a different costume.

Per-case dispositions:
- Slop 1 (READ_ONLY_MARKERS): REPLACE with the deterministic metadata
  check `gateway_conversation_inclusion == "read_only"` (already exists).
- Slop 3 (cc_is_subagent): KEEP the detector in the Go port, MARKED AS
  BUG in code and docs — owner will debug later with access to actual
  prompts. Why we decide subagent at all: subagent turns must not contend
  for the main-turn lease or write the main branch; they get their own
  offshoot branch and checkpoints.
- Slop 4 (sdk/skills phrases): same disposition as Slop 3 — keep, marked
  as bug, debug later with real prompts.
- Slop 2 (transient classifier text): REPLACE with deterministic checks.
- Shape heuristics (message count/roles as intent proxies in
  `classify_transient_internal_request` and the HookEvaluator check):
  also forbidden per rule 2 — revisit when porting; not a replacement
  basis for 3/4.

## BUG: skill body first-line text check ("Base directory for this skill:")

- File: `crates/gateway-http-anthropic/src/claude_code_context.rs:209-216`,
  used by `is_skill_body` / `rewrite_base_directory_line`.
- What it decides: whether a packaged command body is a skill body whose
  first line should be rewritten.
- Why it is a bug: skill content arrives as XML-structured data from
  Claude Code; a plain `contains`/prefix text check is the wrong tool.
  The deterministic basis is to PARSE the structure (the command envelope
  / skill payload as XML or its tag fields), not to grep prose.
- Disposition: KEEP in the Go port, MARKED AS BUG in code and docs;
  replace with real XML parsing when the skill payload's structure is
  confirmed against live traffic.

## Explicitly not slop (do not "fix" these)

- `parse_command_envelope` — structured `<command-message>`/`<command-name>`/`<command-args>` tags
- local-command stdout detection — `<local-command-stdout>` tag plus per-command stdout markers
- `gateway_conversation_inclusion == "read_only"` — metadata, not text
- `claude_code_context.rs` directives — injection, not classification
