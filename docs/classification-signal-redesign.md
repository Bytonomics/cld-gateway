---
type: plan
title: "Plan: Classification-Signal Redesign"
status: draft
tags:
  - classification
  - parked
  - go-port
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# TODO: Classification-signal redesign (parked)

| Section | What it covers |
|---------|----------------|
| [Context](#context) | Why the four prompt-text classifiers can't be ported as-is |
| [Open concerns raised by the owner](#open-concerns-raised-by-the-owner) | What isn't settled yet about the slop inventory |
| [Work to do (when unparked)](#work-to-do-when-unparked) | The re-audit and replacement-signal design steps |
| [Trigger to unpark](#trigger-to-unpark) | The condition that must be met before this work starts |

Status: parked — do not implement until the trigger below is met. The Go port's own
architecture design is now approved (`docs/ARCHITECTURE_v2.md`) and the current, structural-only
interim classifier lives at `core/domain/conversation/classifier.go`
(`core/domain/conversation/kind.go` for the `Kind` enum); this document governs only the
*replacement signals* for the four prompt-text detectors below, which remains open per the
[trigger to unpark](#trigger-to-unpark).

## Context

The frozen Rust gateway (`old_rust/`) classifies every `/v1/messages` request into a
`ConversationRequestKind` (VisibleMain, SubagentOffshoot,
PermissionClassifier, HookEvaluator, LocalControl, StatusOrAuxiliary,
UnknownOffshoot). That kind feeds branch selection, persistence scope, and
transport identity.

Four of the classifiers read literal prompt text — documented in
`docs/AI_SLOP.md`. Those must not be ported. But the *replacement* signals are
not settled, because the slop inventory itself is not fully trusted.

## Open concerns raised by the owner

1. The prompt-dependence audit may be incomplete — there may be more
   prompt-text-driven decisions than the four already found.
2. Some of the four flagged cases may be misclassified as prompt dependence
   when they are actually acceptable structural checks.

## Work to do (when unparked)

1. Re-audit the whole `old_rust/crates/gateway-http-anthropic` surface for any
   decision that reads message/system text — not just the four known sites.
   Include `claude_code_context.rs`, `claude_code_inclusion.rs`,
   `translate.rs`, `context_management.rs`, and `lib.rs` classifiers.
2. For each hit, classify it as: prompt-text bug (remove), structured-tag
   check (keep), stdout-marker check (keep), or metadata check (keep).
3. Design the structural replacement signals for the removed ones:
   - metadata fields (the `gateway_conversation_inclusion` pattern)
   - message tags (`<command-name>`, `<local-command-stdout>`)
   - request shape (stream flag, output_config presence, message count/roles)
   - possible opt-in headers if a future Claude Code version can send them
4. Decide the kind-enum granularity: keep all seven kinds with new signals,
   or collapse kinds whose only detectors were the slop.
5. Record the final signal map in this repo before porting classification
   to Go.

## Trigger to unpark

Owner confirms the golang_port architecture interview is complete and the
slop inventory has been re-verified.
