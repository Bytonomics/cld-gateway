package conversation

import (
	"strings"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// Classifier assigns a Kind to a request. Structural signals only per
// CLAUDE.md/AI_SLOP.md: client-emitted metadata and explicit request
// fields, not prompt text content or message-count/role-mix shape - except
// where AI_SLOP.md explicitly disposes a Rust text-sniffing check as KEPT
// (marked BUG(prompt-text) at the call site below).
type Classifier interface {
	Classify(req *dto.MessagesRequest, meta map[string]string) Kind
}

// StructuralClassifier is the default Classifier, porting
// classify_conversation_request (lib.rs:4468-4513) under the AI_SLOP.md
// dispositions:
//   - the read-only check now reads ONLY client_metadata (already
//     deterministic in Rust; kept as-is).
//   - classify_transient_internal_request (Slop 2, PermissionClassifier)
//     is DELETED per its "REPLACE with deterministic checks" disposition;
//     no deterministic replacement has been defined yet
//     (classification-signal-redesign.md, PARKED), so this classifier never
//     returns PermissionClassifier.
//   - HookEvaluator classification is currently UNIMPLEMENTED, pending a
//     deterministic structural/metadata signal from the parked
//     classification-signal-redesign work.
//   - the cc_is_subagent=true system-text check (Slop 3) and the SDK/skills
//     phrase check (Slop 4) are KEPT, each marked BUG(prompt-text) at the
//     call site, per their AI_SLOP.md dispositions. Slop 4's original Rust
//     gating also mixed in a messages.len() <= 2 shape heuristic; that part
//     is dropped entirely (not ported, not even marked) per CLAUDE.md rule
//     2, which forbids message-count/role-mix heuristics outright.
type StructuralClassifier struct{}

var _ Classifier = StructuralClassifier{}

func (StructuralClassifier) Classify(req *dto.MessagesRequest, meta map[string]string) Kind {
	systemText := strings.ToLower(systemPromptText(req.System))
	joinedText := strings.ToLower(joinedMessageText(req.Messages))

	if meta["gateway_conversation_inclusion"] == "read_only" {
		return LocalControl
	}

	// BUG(prompt-text): reads system prompt text ("cc_is_subagent=true").
	// KEPT per AI_SLOP.md Slop 3 disposition; revisit with real prompt
	// samples. The signal belongs in request metadata, not prompt text.
	if strings.Contains(systemText, "cc_is_subagent=true") {
		return SubagentOffshoot
	}

	// BUG(prompt-text): reads system/message prompt text (SDK/skills-list
	// phrases). KEPT per AI_SLOP.md Slop 4 disposition; revisit with real
	// prompt samples. The original Rust check also gated this on
	// messages.len() <= 2 (a message-count shape heuristic); that part is
	// dropped, not ported in any form, per CLAUDE.md rule 2 - the marked-bug
	// exception in AI_SLOP.md covers the prompt-text scan only, not shape
	// heuristics.
	if strings.Contains(systemText, "claude agent sdk") ||
		strings.Contains(joinedText, "the following skills are available for use with the skill tool") {
		return UnknownOffshoot
	}

	return VisibleMain
}

func systemPromptText(system []dto.SystemBlock) string {
	var b strings.Builder
	for _, block := range system {
		if block.Text != nil {
			if b.Len() > 0 {
				b.WriteString("\n")
			}
			b.WriteString(*block.Text)
		}
	}
	return b.String()
}

func joinedMessageText(messages []dto.Message) string {
	parts := make([]string, 0, len(messages))
	for _, m := range messages {
		parts = append(parts, messageText(m))
	}
	return strings.Join(parts, "\n")
}

func messageText(m dto.Message) string {
	if m.Content.Text != nil {
		return *m.Content.Text
	}
	parts := make([]string, 0, len(m.Content.Blocks))
	for _, b := range m.Content.Blocks {
		if b.BlockType == "text" && b.Text != nil {
			parts = append(parts, *b.Text)
		}
	}
	return strings.Join(parts, "\n\n")
}
