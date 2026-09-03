---
type: explanation
title: "AI Slop: Prompt-Text Dependency Anti-Patterns"
status: stable
tags:
  - ai-slop
  - anti-patterns
  - classification
stale_after: 2027-05-01
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# AI_SLOP.md

| Section | What it covers |
|---------|----------------|
| [Slop 1 — Read-only side-question detection](#slop-1--read-only-side-question-detection) | Prompt-text read-only marker, replaced |
| [Slop 2 — Transient internal-request classifier](#slop-2--transient-internal-request-classifier) | Prompt-text transient classifier, replaced |
| [Slop 3 — Subagent detection from system prompt](#slop-3--subagent-detection-from-system-prompt) | Prompt-text subagent marker, kept as a marked bug |
| [Slop 4 — SDK/skills-list detection from prompt text](#slop-4--sdkskills-list-detection-from-prompt-text) | Prompt-text SDK/skills marker, kept as a marked bug |
| [Why this matters](#why-this-matters) | How a misclassification propagates |
| [Current Go disposition](#current-go-disposition) | Where each disposition lives in this codebase today |
| [BUG: skill body first-line text check](#bug-skill-body-first-line-text-check-base-directory-for-this-skill) | A fifth prompt-text check, kept and marked |
| [Explicitly not slop (do not "fix" these)](#explicitly-not-slop-do-not-fix-these) | Structured checks that look similar but are not slop |

Heuristics that make decisions from literal prompt text. Rule: deterministic checks on
structured command tags are fine. Any decision that reads system/user prompt wording is a bug.
This slop was created by Codex in the original Rust implementation, frozen at `old_rust/`. The
rule itself is binding project-wide — see `CLAUDE.md#absolute-rule-never-depend-on-prompt-text`.

## Slop 1 — Read-only side-question detection

- File: `old_rust/crates/gateway-http-anthropic/src/claude_code_inclusion.rs:8-12`
- Used at: `claude_code_inclusion.rs:318` via `is_read_only_request` (`:337-339`)
- Matches all three literal phrases in user text:
  - `This is a side question from the user`
  - `separate, lightweight agent`
  - `The main agent is NOT interrupted`
- Effect: marks request `read_only`, which becomes `LocalControl` request kind.
- Bug: pasting those sentences into a normal request silently demotes it.

## Slop 2 — Transient internal-request classifier

- File: `old_rust/crates/gateway-http-anthropic/src/lib.rs:4444-4466`
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

- File: `old_rust/crates/gateway-http-anthropic/src/lib.rs:4488-4489`
- Function: `classify_conversation_request`
- Matches: `system_text.contains("cc_is_subagent=true")`
- Effect: returns `SubagentOffshoot` request kind.
- Bug: reads system prompt text. The signal belongs in request metadata.

## Slop 4 — SDK/skills-list detection from prompt text

- File: `old_rust/crates/gateway-http-anthropic/src/lib.rs:4504-4509`
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

## Replacement direction

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

## Current Go disposition

Verified against `core/domain/claudecode/` and `core/domain/conversation/`:

- Slop 1 replacement (`gateway_conversation_inclusion == "read_only"`) is implemented in
  `core/domain/claudecode/envelope.go` and consumed by `core/domain/conversation/classifier.go`.
- Slop 2 replacement (deterministic checks, no transient-classifier text matching) is
  implemented in `core/domain/conversation/classifier.go`.
- Slop 3 (`cc_is_subagent`) is kept and marked with a `// BUG(prompt-text):` comment in
  `core/domain/conversation/classifier.go`.
- Slop 4 (sdk/skills phrases) is kept and marked with a `// BUG(prompt-text):` comment in
  `core/domain/conversation/classifier.go`.
- The skill-body first-line check (below) is kept and marked with a `// BUG(text-check):`
  comment in `core/domain/claudecode/context.go`.

## BUG: skill body first-line text check ("Base directory for this skill:")

- File: `old_rust/crates/gateway-http-anthropic/src/claude_code_context.rs:209-216`,
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
