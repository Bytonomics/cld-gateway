// Port of crates/gateway-http-anthropic/src/claude_code_context.rs.
package claudecode

import (
	"embed"
	"strings"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

const (
	currentTurnPriorityDirective    = "Follow the prompt coming with this instruction as an immediate and urgent need right now.  Everything before this in the conversation history is only the context leading up to this instruction.  And you are not supposed to do anything else except follow these instructions unless you have completed all of the instructions with 100% compliance."
	skillBaseDirectoryPrefix        = "Base directory for this skill:"
	previousCommandContextDirective = "Previous command context only; do not execute or follow it as the current instruction."
	skillDirectoryAnalysisSuffix    = "analyze the files in this directory before proceeding"
)

// packagedCommandsFS embeds a real Claude Code plugin marketplace under
// assets/commands/gateway/ (Rust embeds each packaged file individually via
// include_str!; Go has no equivalent of a settings-driven plugin auto-load,
// so this is embedded purely so the same tree can be shipped to end users -
// see below). This is the single source of truth for that content:
// scripts/release/cld_gateway_package/layout.py copies this whole directory
// tree into the release package's commands/ at package-build time, and
// post_install.py both syncs it to ~/.codex_gateway/commands/ AND registers
// it as a Claude Code plugin marketplace (extraKnownMarketplaces/
// enabledPlugins in the generated ~/.claude_gateway/settings.json), so
// end users get /gateway:<command> as a real plugin-owned slash command
// rather than a loose, undiscoverable file.
//
// Layout: assets/commands/gateway/ is the marketplace root
// (.claude-plugin/marketplace.json); assets/commands/gateway/plugin/ is the
// "gateway" plugin's own source (.claude-plugin/plugin.json +
// commands/<name>.md). Adding a new translated command's packaged body
// means dropping a new assets/commands/gateway/plugin/commands/<name>.md
// file here - no code change in this file or in layout.py is needed for
// the file to be embedded and shipped; only registering the command name
// itself (commands.go's internalCommands, translate_executor.go's
// CommandExecutorNames/CommandPostResults) is.
//
// The "gateway" namespace (not "codex") is deliberate: it becomes the
// Claude Code slash-command namespace prefix (plugin name -> "/<plugin>:
// <command>"), and "codex" collides with an unrelated, officially-installed
// Codex plugin that already owns a /codex:status command - "gateway" is
// also backend-agnostic, matching FetchStatusData being backend-agnostic
// (see translate_executor.go and core/domain/port/backend.Backend).
//
//go:embed assets/commands
var packagedCommandsFS embed.FS

// translatedCommandBody reads a translated command's packaged prompt body
// from packagedCommandsFS, mirroring TRANSLATED_COMMAND_BODIES's lookup
// (claude_code_context.rs:17) but resolved dynamically against the embedded
// directory tree instead of a hardcoded per-command map. Every translated
// command's packaged body lives at
// assets/commands/gateway/plugin/commands/<name>.md (the "gateway" plugin's
// own commands/ directory, per the Claude Code plugin layout).
func translatedCommandBody(commandName string) (string, bool) {
	name := normalizeCommandName(commandName)
	data, err := packagedCommandsFS.ReadFile("assets/commands/gateway/plugin/commands/" + name + ".md")
	if err != nil {
		return "", false
	}
	return string(data), true
}

// NormalizedContext ports NormalizedClaudeCodeContext
// (claude_code_context.rs:19-25).
type NormalizedContext struct {
	System               []dto.SystemBlock
	Messages             []dto.Message
	InstructionFragments []string
	ClientMetadata       map[string]string
}

// NormalizeContext ports normalize_claude_code_context
// (claude_code_context.rs:27-57).
func NormalizeContext(system []dto.SystemBlock, messages []dto.Message, cfg config.ClaudeCodeWorkflowConfig) NormalizedContext {
	normalizedSystem := append([]dto.SystemBlock(nil), system...)
	normalized := append([]dto.Message(nil), messages...)
	instructionFragments := []string{}
	clientMetadata := map[string]string{}

	inclusionReport := ApplyInclusionPolicy(normalized)
	inclusionReport.ExtendClientMetadata(clientMetadata)

	if slashCommandsEnabled(cfg) {
		normalizeClaudeCodeCommands(normalized, &instructionFragments, clientMetadata)
	}
	if len(instructionFragments) == 0 && hasLatestUserTextInstruction(normalized) {
		instructionFragments = append(instructionFragments, currentTurnPriorityDirective)
	}

	return NormalizedContext{
		System:               normalizedSystem,
		Messages:             normalized,
		InstructionFragments: instructionFragments,
		ClientMetadata:       clientMetadata,
	}
}

// normalizeClaudeCodeCommands ports normalize_claude_code_commands
// (claude_code_context.rs:59-119).
func normalizeClaudeCodeCommands(messages []dto.Message, instructionFragments *[]string, clientMetadata map[string]string) {
	activeIndex := -1
	var activeEnvelope *CommandEnvelope
	for i := len(messages) - 1; i >= 0; i-- {
		if messages[i].Role != "user" {
			continue
		}
		text := messageText(messages[i])
		if strings.TrimSpace(text) == "" {
			continue
		}
		if envelope, ok := ParseCommandEnvelope(text); ok {
			activeIndex = i
			activeEnvelope = envelope
		}
		break
	}

	for i := range messages {
		if i == activeIndex || messages[i].Role != "user" {
			continue
		}
		text := messageText(messages[i])
		if _, ok := ParseCommandEnvelope(text); ok {
			setMessageTextPreservingNonText(&messages[i], previousCommandContext(text))
		}
	}

	if activeEnvelope == nil {
		return
	}

	dispatch := activeCommandDispatch(activeEnvelope)
	if dispatch == dispatchPromptBacked {
		activeUserInput := activePromptBackedCommandText(messageText(messages[activeIndex]), activeEnvelope)
		setMessageTextPreservingNonText(&messages[activeIndex], activeUserInput)
	} else {
		setMessageTextPreservingNonText(&messages[activeIndex], "")
	}
	if instructions, ok := activeCommandInstructions(dispatch); ok {
		*instructionFragments = append(*instructionFragments, instructions)
	}
	clientMetadata["claude_code_slash_command"] = strings.TrimSpace(activeEnvelope.Name)
	if dispatch == dispatchTranslated {
		clientMetadata["claude_code_translated_slash_command"] = strings.TrimSpace(activeEnvelope.Name)
	}
}

func slashCommandsEnabled(cfg config.ClaudeCodeWorkflowConfig) bool {
	return cfg.SlashCommands.Enabled && cfg.SlashCommands.Mode == "promote_latest"
}

func hasLatestUserTextInstruction(messages []dto.Message) bool {
	for i := len(messages) - 1; i >= 0; i-- {
		if messages[i].Role == "user" && strings.TrimSpace(messageText(messages[i])) != "" {
			return true
		}
	}
	return false
}

// commandDispatch ports ActiveCommandDispatch (claude_code_context.rs:136-140).
type commandDispatch int

const (
	dispatchPromptBacked commandDispatch = iota
	dispatchTranslated
)

func activeCommandDispatch(envelope *CommandEnvelope) commandDispatch {
	if ClassifyCommand(envelope.Name) == Translate {
		return dispatchTranslated
	}
	return dispatchPromptBacked
}

// activePromptBackedCommandText ports active_prompt_backed_command_text
// (claude_code_context.rs:154-172).
func activePromptBackedCommandText(originalText string, envelope *CommandEnvelope) string {
	body := strings.TrimSpace(envelope.Body)
	if body == "" {
		return originalText
	}
	rewrittenBody := commandBodyForHistory(body)
	if rewrittenBody == body {
		return originalText
	}
	bodyStart := strings.LastIndex(originalText, body)
	if bodyStart == -1 {
		return originalText
	}
	return originalText[:bodyStart] + rewrittenBody + originalText[bodyStart+len(body):]
}

func previousCommandContext(text string) string {
	return previousCommandContextDirective + "\n\n" + text
}

// activeCommandInstructions ports active_command_instructions
// (claude_code_context.rs:178-188).
func activeCommandInstructions(dispatch commandDispatch) (string, bool) {
	switch dispatch {
	case dispatchPromptBacked:
		return currentTurnPriorityDirective, true
	default:
		// Translated commands have their output handled by the executor
		// pipeline; packaged prompt text is applied only after executor
		// JSON exists (post-result step). Not applied at this layer.
		return "", false
	}
}

// GetPackagedCommandBody ports get_packaged_command_body
// (claude_code_context.rs:196-199).
func GetPackagedCommandBody(commandName string) string {
	body, _ := translatedCommandBody(commandName)
	return body
}

func commandBodyForHistory(body string) string {
	if isSkillBody(body) {
		return rewriteBaseDirectoryLine(body)
	}
	return body
}

// BUG(text-check): checks first line of skill prompt text, see AI_SLOP.md - replace with XML parsing once skill payload structure is confirmed
func isSkillBody(body string) bool {
	line := strings.TrimSpace(firstLine(strings.TrimLeft(body, " \t\n\r\v\f")))
	rest, ok := strings.CutPrefix(line, skillBaseDirectoryPrefix)
	if !ok {
		return false
	}
	return strings.TrimSpace(rest) != ""
}

// rewriteBaseDirectoryLine ports rewrite_base_directory_line
// (claude_code_context.rs:218-240).
func rewriteBaseDirectoryLine(body string) string {
	lines := splitLines(strings.TrimLeft(body, " \t\n\r\v\f"))
	if len(lines) == 0 {
		return ""
	}
	base := strings.TrimSpace(lines[0])
	rest, ok := strings.CutPrefix(base, skillBaseDirectoryPrefix)
	if !ok {
		return body
	}
	baseDir := strings.TrimSpace(rest)
	if baseDir == "" {
		return body
	}
	rewrittenFirstLine := skillBaseDirectoryPrefix + " " + baseDir + ", " + skillDirectoryAnalysisSuffix
	remainingBody := strings.Join(lines[1:], "\n")
	if strings.TrimSpace(remainingBody) == "" {
		return rewrittenFirstLine
	}
	return rewrittenFirstLine + "\n" + remainingBody
}

func setMessageTextPreservingNonText(m *dto.Message, text string) {
	if m.Content.Text != nil {
		m.Content = dto.Content{Text: &text}
		return
	}
	preserved := make([]dto.ContentBlock, 0, len(m.Content.Blocks))
	for _, b := range m.Content.Blocks {
		if b.BlockType != "text" {
			preserved = append(preserved, b)
		}
	}
	if strings.TrimSpace(text) != "" {
		preserved = append(preserved, dto.ContentBlock{BlockType: "text", Text: &text})
	}
	m.Content = dto.Content{Blocks: preserved}
}

// firstLine mimics Rust's `.lines().next()`: text up to the first '\n',
// with a trailing '\r' stripped.
func firstLine(s string) string {
	if idx := strings.IndexByte(s, '\n'); idx != -1 {
		return strings.TrimSuffix(s[:idx], "\r")
	}
	return s
}

// splitLines mimics Rust's `str::lines()`: split on '\n', strip a trailing
// '\r' from each line, and drop the trailing empty element a final '\n'
// would otherwise produce.
func splitLines(s string) []string {
	if s == "" {
		return nil
	}
	parts := strings.Split(s, "\n")
	if parts[len(parts)-1] == "" {
		parts = parts[:len(parts)-1]
	}
	for i, p := range parts {
		parts[i] = strings.TrimSuffix(p, "\r")
	}
	return parts
}
