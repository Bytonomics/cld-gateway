// Package claudecode handles the Claude Code client protocol: structured
// command envelope tags (<command-name>, <command-args>, <command-message>,
// <local-command-stdout>) and the conversation-inclusion policy built on
// them. Port of crates/gateway-http-anthropic/src/claude_code_inclusion.rs.
package claudecode

import (
	"strings"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

const (
	commandMessageTag     = "command-message"
	commandNameTag        = "command-name"
	commandArgsTag        = "command-args"
	localCommandStdoutTag = "local-command-stdout"
)

// CommandEnvelope is a parsed <command-name>/<command-args> tag pair
// emitted by the Claude Code client. Port of CommandEnvelope
// (claude_code_inclusion.rs:233-237).
type CommandEnvelope struct {
	Name string
	Body string
}

// ParseCommandEnvelope ports parse_command_envelope
// (claude_code_inclusion.rs:294-311).
func ParseCommandEnvelope(text string) (*CommandEnvelope, bool) {
	nameMatch, ok := lastTagMatch(text, commandNameTag)
	if !ok {
		return nil, false
	}
	bodyStart := nameMatch.End
	if suffix := text[nameMatch.End:]; suffix != "" {
		if argsMatch, ok := firstTagMatch(suffix, commandArgsTag); ok {
			argsMatch = argsMatch.withOffset(nameMatch.End)
			bodyStart = argsMatch.End
		}
	}
	body := strings.TrimSpace(text[bodyStart:])
	return &CommandEnvelope{Name: nameMatch.Value, Body: body}, true
}

// conversationInclusion mirrors ConversationInclusion
// (claude_code_inclusion.rs:135-151).
type conversationInclusion int

const (
	inclusionReadWrite conversationInclusion = iota
	inclusionReadOnly
	inclusionLocalOnly
)

func (c conversationInclusion) asStr() string {
	switch c {
	case inclusionReadOnly:
		return "read_only"
	case inclusionLocalOnly:
		return "local_only"
	default:
		return "read_write"
	}
}

// InclusionResult is the pinned report returned by ApplyInclusionPolicy
// (FILEMAP.md core/domain/claudecode/envelope.go). The unexported fields
// carry the extra state ExtendClientMetadata needs (distinguishing a
// read_only turn from a fully local_only one, and the active-command
// subset) without widening the pinned exported surface.
type InclusionResult struct {
	ReadOnly          bool
	LocalOnlyCommands []string

	turnInclusion           conversationInclusion
	activeLocalOnlyCommands []string
}

// ExtendClientMetadata ports
// ConversationInclusionReport::extend_client_metadata
// (claude_code_inclusion.rs:161-180).
func (r InclusionResult) ExtendClientMetadata(clientMetadata map[string]string) {
	if r.turnInclusion != inclusionReadWrite {
		clientMetadata["gateway_conversation_inclusion"] = r.turnInclusion.asStr()
	}
	if len(r.LocalOnlyCommands) > 0 {
		clientMetadata["gateway_local_only_commands"] = strings.Join(r.LocalOnlyCommands, ",")
	}
	if len(r.activeLocalOnlyCommands) > 0 {
		clientMetadata["gateway_active_local_only_commands"] = strings.Join(r.activeLocalOnlyCommands, ",")
	}
}

// ApplyInclusionPolicy ports apply_conversation_inclusion_policy
// (claude_code_inclusion.rs:239-292). It redacts local-only command/stdout
// spans from user message text in place (msgs shares its backing array
// with the caller's slice) and reports the local-only commands found.
//
// SLOP 1 OVERRIDE (AI_SLOP.md, binding): the Rust READ_ONLY_MARKERS
// text-sniffing check (is_read_only_request, claude_code_inclusion.rs:
// 337-339) is deleted per CLAUDE.md's ban on prompt-text dependence.
// InclusionResult.ReadOnly is never set from message text here; a
// read-only turn must be derived from a client metadata field (e.g. an
// already-set gateway_conversation_inclusion == "read_only") at the call
// site, not by scanning text for marker phrases.
func ApplyInclusionPolicy(msgs []dto.Message) InclusionResult {
	result := InclusionResult{}

	latestUserIndex := -1
	for i := range msgs {
		if msgs[i].Role == "user" && strings.TrimSpace(messageText(msgs[i])) != "" {
			latestUserIndex = i
		}
	}

	for i := range msgs {
		if msgs[i].Role != "user" {
			continue
		}
		var messageLocalOnlyCommands []string
		msgs[i].Content = applyContentInclusionPolicy(msgs[i].Content, &result, &messageLocalOnlyCommands)

		if i == latestUserIndex && strings.TrimSpace(messageText(msgs[i])) == "" {
			for _, cmd := range messageLocalOnlyCommands {
				markActiveLocalOnlyCommand(&result, cmd)
			}
		}
	}

	markLocalOnlyTurnIfEmpty(&result, msgs)
	return result
}

func applyContentInclusionPolicy(content dto.Content, result *InclusionResult, messageLocalOnlyCommands *[]string) dto.Content {
	if content.Text != nil {
		newText := applyTextInclusionPolicy(*content.Text, result, messageLocalOnlyCommands)
		content.Text = &newText
		return content
	}

	blocks := make([]dto.ContentBlock, 0, len(content.Blocks))
	for _, block := range content.Blocks {
		if block.BlockType == "text" && block.Text != nil {
			newText := applyTextInclusionPolicy(*block.Text, result, messageLocalOnlyCommands)
			block.Text = &newText
		}
		if block.BlockType != "text" || (block.Text != nil && strings.TrimSpace(*block.Text) != "") {
			blocks = append(blocks, block)
		}
	}
	content.Blocks = blocks
	return content
}

func applyTextInclusionPolicy(text string, result *InclusionResult, messageLocalOnlyCommands *[]string) string {
	included := text
	for {
		m, ok := localOnlyCommandSpan(included)
		if !ok {
			break
		}
		markLocalOnlyCommand(result, m.commandName)
		*messageLocalOnlyCommands = append(*messageLocalOnlyCommands, m.commandName)
		included = included[:m.start] + included[m.end:]
	}
	for {
		m, ok := localOnlyStdoutSpan(included)
		if !ok {
			break
		}
		markLocalOnlyCommand(result, m.commandName)
		*messageLocalOnlyCommands = append(*messageLocalOnlyCommands, m.commandName)
		included = included[:m.start] + included[m.end:]
	}
	return strings.TrimSpace(included)
}

func markLocalOnlyCommand(result *InclusionResult, commandName string) {
	name := "/" + normalizeCommandName(commandName)
	for _, c := range result.LocalOnlyCommands {
		if c == name {
			return
		}
	}
	result.LocalOnlyCommands = append(result.LocalOnlyCommands, name)
}

func markActiveLocalOnlyCommand(result *InclusionResult, commandName string) {
	name := "/" + normalizeCommandName(commandName)
	for _, c := range result.activeLocalOnlyCommands {
		if c == name {
			return
		}
	}
	result.activeLocalOnlyCommands = append(result.activeLocalOnlyCommands, name)
}

func markLocalOnlyTurnIfEmpty(result *InclusionResult, msgs []dto.Message) {
	if len(result.LocalOnlyCommands) == 0 {
		return
	}
	for _, m := range msgs {
		if m.Role == "user" && strings.TrimSpace(messageText(m)) != "" {
			return
		}
	}
	result.turnInclusion = inclusionLocalOnly
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

type localOnlyMatch struct {
	commandName string
	start, end  int
}

func localOnlyCommandSpan(text string) (localOnlyMatch, bool) {
	remaining := text
	offset := 0
	for {
		nameMatch, ok := firstTagMatch(remaining, commandNameTag)
		if !ok {
			return localOnlyMatch{}, false
		}
		nameMatch = nameMatch.withOffset(offset)
		if spec, ok := localOnlyCommandSpec(nameMatch.Value); ok {
			commandStart := nameMatch.Start
			if prefix := text[:nameMatch.Start]; prefix != "" {
				if tag, ok := lastTagMatch(prefix, commandMessageTag); ok {
					commandStart = tag.Start
				}
			}
			commandEnd := nameMatch.End
			if suffix := text[nameMatch.End:]; suffix != "" {
				if tag, ok := firstTagMatch(suffix, commandArgsTag); ok {
					tag = tag.withOffset(nameMatch.End)
					commandEnd = tag.End
				}
			}
			return localOnlyMatch{commandName: spec.name, start: commandStart, end: commandEnd}, true
		}
		offset = nameMatch.End
		remaining = text[offset:]
	}
}

func localOnlyStdoutSpan(text string) (localOnlyMatch, bool) {
	remaining := text
	offset := 0
	for {
		stdoutMatch, ok := firstTagMatch(remaining, localCommandStdoutTag)
		if !ok {
			return localOnlyMatch{}, false
		}
		stdoutMatch = stdoutMatch.withOffset(offset)
		if spec, ok := localOnlyStdoutSpec(stdoutMatch.Value); ok {
			return localOnlyMatch{commandName: spec.name, start: stdoutMatch.Start, end: stdoutMatch.End}, true
		}
		offset = stdoutMatch.End
		remaining = text[offset:]
	}
}

type tagMatch struct {
	Start, End int
	Value      string
}

func (t tagMatch) withOffset(offset int) tagMatch {
	return tagMatch{Start: t.Start + offset, End: t.End + offset, Value: t.Value}
}

func firstTagMatch(text, tag string) (tagMatch, bool) {
	open := "<" + tag + ">"
	closeTag := "</" + tag + ">"
	start := strings.Index(text, open)
	if start == -1 {
		return tagMatch{}, false
	}
	valueStart := start + len(open)
	rel := strings.Index(text[valueStart:], closeTag)
	if rel == -1 {
		return tagMatch{}, false
	}
	valueEnd := valueStart + rel
	end := valueEnd + len(closeTag)
	return tagMatch{Start: start, End: end, Value: strings.TrimSpace(text[valueStart:valueEnd])}, true
}

func lastTagMatch(text, tag string) (tagMatch, bool) {
	remaining := text
	offset := 0
	found := false
	var latest tagMatch
	for {
		m, ok := firstTagMatch(remaining, tag)
		if !ok {
			break
		}
		abs := m.withOffset(offset)
		offset = abs.End
		remaining = text[offset:]
		latest = abs
		found = true
	}
	return latest, found
}
