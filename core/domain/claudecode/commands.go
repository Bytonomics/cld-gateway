package claudecode

import "strings"

// Classification is the result of ClassifyCommand, port of
// ClaudeCodeCommandClassification (claude_code_inclusion.rs:213-218).
type Classification int

const (
	PromptBacked Classification = iota
	LocalOnly
	Translate
)

type internalCommandClassification int

const (
	internalLocalOnly internalCommandClassification = iota
	internalTranslate
)

type internalCommandSpec struct {
	name           string
	classification internalCommandClassification
	stdoutMarkers  []string
}

// internalCommands ports INTERNAL_COMMANDS (claude_code_inclusion.rs:13-133).
var internalCommands = []internalCommandSpec{
	{name: "add-dir", classification: internalLocalOnly},
	{name: "agents", classification: internalLocalOnly},
	{name: "branch", classification: internalLocalOnly, stdoutMarkers: []string{
		"Branched conversation", "You are now in the branch", "claude -r ",
	}},
	{name: "color", classification: internalLocalOnly},
	{name: "config", classification: internalLocalOnly},
	{name: "copy", classification: internalLocalOnly},
	{name: "compact", classification: internalLocalOnly, stdoutMarkers: []string{"Compacted"}},
	{name: "effort", classification: internalLocalOnly},
	{name: "export", classification: internalLocalOnly},
	{name: "hooks", classification: internalLocalOnly},
	{name: "ide", classification: internalLocalOnly},
	{name: "login", classification: internalLocalOnly},
	{name: "logout", classification: internalLocalOnly},
	{name: "mcp", classification: internalLocalOnly},
	{name: "mobile", classification: internalLocalOnly},
	{name: "model", classification: internalLocalOnly},
	{name: "permissions", classification: internalLocalOnly},
	{name: "plugin", classification: internalLocalOnly},
	{name: "rename", classification: internalLocalOnly, stdoutMarkers: []string{"Session renamed to:"}},
	{name: "resume", classification: internalLocalOnly},
	{name: "sandbox", classification: internalLocalOnly},
	{name: "skills", classification: internalLocalOnly},
	{name: "status", classification: internalTranslate},
}

var commandsByName = func() map[string]*internalCommandSpec {
	m := make(map[string]*internalCommandSpec, len(internalCommands))
	for i := range internalCommands {
		m[internalCommands[i].name] = &internalCommands[i]
	}
	return m
}()

// ClassifyCommand ports classify_claude_code_command
// (claude_code_inclusion.rs:400-408).
func ClassifyCommand(name string) Classification {
	spec, ok := commandSpecByName(normalizeCommandName(name))
	if !ok {
		return PromptBacked
	}
	switch spec.classification {
	case internalLocalOnly:
		return LocalOnly
	case internalTranslate:
		return Translate
	default:
		return PromptBacked
	}
}

func commandSpecByName(name string) (*internalCommandSpec, bool) {
	spec, ok := commandsByName[name]
	return spec, ok
}

func localOnlyCommandSpec(name string) (*internalCommandSpec, bool) {
	spec, ok := commandSpecByName(normalizeCommandName(name))
	if !ok || spec.classification != internalLocalOnly {
		return nil, false
	}
	return spec, true
}

func localOnlyStdoutSpec(stdout string) (*internalCommandSpec, bool) {
	for i := range internalCommands {
		spec := &internalCommands[i]
		if spec.classification != internalLocalOnly || len(spec.stdoutMarkers) == 0 {
			continue
		}
		allPresent := true
		for _, marker := range spec.stdoutMarkers {
			if !strings.Contains(stdout, marker) {
				allPresent = false
				break
			}
		}
		if allPresent {
			return spec, true
		}
	}
	return nil, false
}

// gatewayPluginNamespace is the packaged "gateway" plugin's marketplace
// name (core/domain/claudecode/assets/commands/gateway/.claude-plugin/
// marketplace.json), which Claude Code prefixes onto every command it owns
// once it's installed as a real plugin (e.g. "/status" arrives on the wire
// as "/gateway:status", not "/status" - confirmed from a live
// <command-name> envelope tag). Every classification/lookup in this
// package must strip that prefix the same way it strips a leading "/", or
// a plugin-owned command silently falls through to the PromptBacked
// default instead of being recognized as Translate/LocalOnly.
const gatewayPluginNamespace = "gateway:"

func normalizeCommandName(name string) string {
	name = strings.TrimPrefix(strings.TrimSpace(name), "/")
	name = strings.TrimPrefix(name, gatewayPluginNamespace)
	return name
}
