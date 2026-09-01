package translator

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strconv"
	"strings"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/claudecode"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

const (
	webSearchSourcesInclude     = "web_search_call.action.sources"
	anthropicWebSearchType      = "web_search_20250305"
	openAIWebSearchType         = "web_search"
	readToolLineNumberDirective = "When calling Read, offset and limit must be JSON whole-number integers written in normal base-10 decimal digits only, such as 1, 250, or 1250. Never use decimals, floats, exponents, or scientific notation. Omit offset unless you are copying an exact line number from prior tool output."
	readOffsetDescription       = "Line offset. Use only whole-number base-10 decimal digits like 1, 250, or 1250. Never use decimals, floats, exponents, or scientific notation. Omit unless you know the exact line number from prior tool output."
	readLimitDescription        = "Number of lines to read. Use only whole-number base-10 decimal digits like 100, 250, or 1250. Never use decimals, floats, exponents, or scientific notation."
	defaultInstructions         = "You are a helpful assistant."
)

// ToolCallKindLookup resolves the backend tool-call kind (function, custom,
// tool_search, local_shell) for a call_id, so tool_use/tool_result blocks
// route to the right wire item type. Port of
// ToolTranslationContext.tool_kinds_by_call_id (translate.rs:41,60-65). A
// nil ToolCallKindLookup, or a callID the lookup does not know, resolves to
// ToolCallKindFunction (matches Rust's unwrap_or(CodexToolCallKind::Function)).
type ToolCallKindLookup interface {
	ToolCallKind(callID string) (ToolCallKind, bool)
}

// GenericBackendTranslator implements the request-shaping half of
// BackendTranslator that translate.rs keeps genuinely backend-agnostic:
// Claude Code context normalization, instructions assembly, message-to-item
// conversion, tool/tool_choice/output_config/reasoning-effort mapping. Port
// of translate_request_with_context and its helpers
// (crates/gateway-http-anthropic/src/translate.rs:68-759). Response-event
// mapping is intentionally out of scope for this file; a per-backend
// translator embeds *GenericBackendTranslator and adds
// TranslateResponseEvent/BuildUnaryResponse to satisfy BackendTranslator.
type GenericBackendTranslator struct {
	ClaudeCodeConfig config.ClaudeCodeWorkflowConfig
	ToolCallKinds    ToolCallKindLookup

	// Response-event mapping dependencies (sse_bridge.go). A translator is
	// constructed fresh per request, so these fields carry the per-request
	// context that sse_bridge.rs's map_backend_event/build_unary_messages_
	// response receive as explicit parameters in Rust (request_id, the
	// structured-output schema and context_management value derived from
	// the request, and the client-facing model echoed into the response).
	ToolCalls              state.ToolCallRepo
	RequestID              *string
	StructuredOutputSchema any
	ContextManagementValue any
	ResponseModel          string
	Clock                  state.Clock
	stream                 *bridgeState
}

func (g *GenericBackendTranslator) kindForCall(callID string) ToolCallKind {
	if g.ToolCallKinds == nil {
		return ToolCallKindFunction
	}
	if kind, ok := g.ToolCallKinds.ToolCallKind(callID); ok {
		return kind
	}
	return ToolCallKindFunction
}

// TranslateRequest ports translate_request_with_context
// (translate.rs:68-141).
func (g *GenericBackendTranslator) TranslateRequest(_ context.Context, in *dto.MessagesRequest, meta TranslateMeta) (*backend.Request, error) {
	normalized := claudecode.NormalizeContext(in.System, in.Messages, g.ClaudeCodeConfig)
	messages := append([]dto.Message(nil), normalized.Messages...)
	turnSystemInstructions := extractTurnSystemTextAndFilter(&messages)

	systemText, hasSystemText := extractSystemText(normalized.System)
	baseInstructions, hasBaseInstructions := combineInstructionParts(systemText, hasSystemText, turnSystemInstructions)
	if !hasBaseInstructions {
		baseInstructions = defaultInstructions
	}

	instructionFragments := append([]string(nil), normalized.InstructionFragments...)
	if hasReadTool(in.Tools) {
		instructionFragments = append(instructionFragments, readToolLineNumberDirective)
	}
	var instructions string
	if len(instructionFragments) == 0 {
		instructions = baseInstructions
	} else {
		instructions = strings.Join(instructionFragments, "\n\n") + "\n\n" + baseInstructions
	}

	input, err := g.translateMessagesToBackendItems(messages)
	if err != nil {
		return nil, err
	}

	hostedWebSearch := false
	for _, t := range in.Tools {
		if t.ToolType != nil && *t.ToolType == anthropicWebSearchType {
			hostedWebSearch = true
			break
		}
	}
	tools, err := translateTools(in.Tools)
	if err != nil {
		return nil, err
	}

	var toolChoice string
	if hostedWebSearch {
		toolChoice = "required"
	} else {
		toolChoice = translateToolChoice(in.ToolChoice)
	}

	text := translateOutputConfig(in.OutputConfig)

	clientMetadata := map[string]string{}
	reasoning := translateEffortToBackendReasoning(in.OutputConfig, clientMetadata)
	if in.MaxTokens != nil {
		clientMetadata["anthropic_max_tokens"] = strconv.FormatUint(uint64(*in.MaxTokens), 10)
	}
	if in.TopK != nil {
		clientMetadata["anthropic_top_k"] = strconv.FormatUint(uint64(*in.TopK), 10)
	}
	if in.Temperature != nil {
		clientMetadata["anthropic_temperature"] = formatFloat(*in.Temperature)
	}
	if in.TopP != nil {
		clientMetadata["anthropic_top_p"] = formatFloat(*in.TopP)
	}
	if in.Metadata != nil {
		encoded, err := json.Marshal(in.Metadata)
		if err != nil {
			return nil, fmt.Errorf("metadata must be JSON-serializable: %w", err)
		}
		clientMetadata["anthropic_metadata"] = string(encoded)
	}
	for k, v := range normalized.ClientMetadata {
		clientMetadata[k] = v
	}

	req := &backend.Request{
		Model:             meta.Model,
		Instructions:      instructions,
		Input:             input,
		Tools:             tools.tools,
		ToolChoice:        toolChoice,
		ParallelToolCalls: true,
		Text:              text,
		Reasoning:         reasoning,
		Stream:            in.Stream,
		Include:           tools.include,
		ServiceTier:       meta.ServiceTier,
	}
	if len(clientMetadata) > 0 {
		req.ClientMetadata = clientMetadata
	}
	return req, nil
}

func formatFloat(v float64) string {
	return strconv.FormatFloat(v, 'f', -1, 64)
}

// translateEffortToBackendReasoning ports translate_effort_to_backend_reasoning
// (translate.rs:143-168).
func translateEffortToBackendReasoning(outputConfig *dto.OutputConfig, clientMetadata map[string]string) *map[string]any {
	if outputConfig == nil || outputConfig.Effort == nil {
		return nil
	}
	normalized := strings.ToLower(strings.TrimSpace(*outputConfig.Effort))
	if normalized == "" {
		return nil
	}
	clientMetadata["anthropic_effort"] = normalized

	var mapped string
	switch normalized {
	case "low", "medium", "high", "none", "minimal":
		mapped = normalized
	case "max", "xhigh":
		mapped = "high"
	default:
		clientMetadata["anthropic_effort_unmapped"] = normalized
		return nil
	}

	result := map[string]any{"effort": mapped}
	return &result
}

// extractSystemText ports extract_system_text (translate.rs:170-191).
func extractSystemText(system []dto.SystemBlock) (string, bool) {
	parts := make([]string, 0, len(system))
	for _, block := range system {
		if block.BlockType != "text" || block.Text == nil {
			continue
		}
		trimmed := strings.TrimSpace(*block.Text)
		if trimmed == "" {
			continue
		}
		parts = append(parts, trimmed)
	}
	if len(parts) == 0 {
		return "", false
	}
	return strings.Join(parts, "\n\n"), true
}

// combineInstructionParts ports combine_instruction_parts (translate.rs:193-209).
func combineInstructionParts(base string, hasBase bool, appended []string) (string, bool) {
	parts := make([]string, 0, len(appended)+1)
	if hasBase {
		parts = append(parts, base)
	}
	parts = append(parts, appended...)

	filtered := make([]string, 0, len(parts))
	for _, p := range parts {
		trimmed := strings.TrimSpace(p)
		if trimmed != "" {
			filtered = append(filtered, trimmed)
		}
	}
	if len(filtered) == 0 {
		return "", false
	}
	return strings.Join(filtered, "\n\n"), true
}

// extractTurnSystemTextAndFilter ports extract_turn_system_text_and_filter
// (translate.rs:211-223); messages is replaced in place with system-role
// messages removed, mirroring Rust's Vec::retain.
func extractTurnSystemTextAndFilter(messages *[]dto.Message) []string {
	instructionParts := make([]string, 0)
	filtered := make([]dto.Message, 0, len(*messages))
	for _, m := range *messages {
		if !strings.EqualFold(m.Role, "system") {
			filtered = append(filtered, m)
			continue
		}
		if text, ok := messageTextForInstructions(m); ok {
			instructionParts = append(instructionParts, text)
		}
	}
	*messages = filtered
	return instructionParts
}

// messageTextForInstructions ports message_text_for_instructions
// (translate.rs:225-242).
func messageTextForInstructions(m dto.Message) (string, bool) {
	if m.Content.Text != nil {
		return nonEmptyInstructionText(*m.Content.Text)
	}
	parts := make([]string, 0, len(m.Content.Blocks))
	for _, b := range m.Content.Blocks {
		if b.BlockType != "text" || b.Text == nil {
			continue
		}
		if text, ok := nonEmptyInstructionText(*b.Text); ok {
			parts = append(parts, text)
		}
	}
	if len(parts) == 0 {
		return "", false
	}
	return strings.Join(parts, "\n\n"), true
}

func nonEmptyInstructionText(text string) (string, bool) {
	trimmed := strings.TrimSpace(text)
	if trimmed == "" {
		return "", false
	}
	return trimmed, true
}

// translateMessagesToBackendItems ports translate_messages_to_backend_items
// (translate.rs:249-315).
func (g *GenericBackendTranslator) translateMessagesToBackendItems(messages []dto.Message) ([]map[string]any, error) {
	items := make([]map[string]any, 0, len(messages))
	for _, msg := range messages {
		role := msg.Role

		if msg.Content.Text != nil {
			text := *msg.Content.Text
			if strings.TrimSpace(text) != "" {
				items = append(items, map[string]any{
					"type":    "message",
					"role":    role,
					"content": []any{contentItemForRole(role, text)},
				})
			}
			continue
		}

		messageContent := make([]any, 0, len(msg.Content.Blocks))
		for _, b := range msg.Content.Blocks {
			switch b.BlockType {
			case "text":
				if b.Text != nil && strings.TrimSpace(*b.Text) != "" {
					messageContent = append(messageContent, contentItemForRole(role, *b.Text))
				}
			case "image":
				if role != "user" {
					continue
				}
				if img, ok := imageContentItem(b); ok {
					messageContent = append(messageContent, img)
				}
			case "tool_result":
				if item, ok := g.toolResultItem(b); ok {
					items = append(items, item)
				}
			case "tool_use":
				item, err := g.toolUseItem(b)
				if err != nil {
					return nil, err
				}
				if item != nil {
					items = append(items, item)
				}
			default:
			}
		}
		if len(messageContent) > 0 {
			items = append(items, map[string]any{
				"type":    "message",
				"role":    role,
				"content": messageContent,
			})
		}
	}
	return items, nil
}

// contentItemForRole ports content_item_for_role (translate.rs:317-325).
func contentItemForRole(role, text string) map[string]any {
	itemType := "input_text"
	if role == "assistant" {
		itemType = "output_text"
	}
	return map[string]any{"type": itemType, "text": text}
}

// imageContentItem ports image_content_item (translate.rs:327-339).
func imageContentItem(block dto.ContentBlock) (map[string]any, bool) {
	if block.Source == nil || block.Source.SourceType != "base64" {
		return nil, false
	}
	if block.Source.MediaType == nil || block.Source.Data == nil {
		return nil, false
	}
	url := fmt.Sprintf("data:%s;base64,%s", *block.Source.MediaType, *block.Source.Data)
	return map[string]any{"type": "input_image", "image_url": url}, true
}

// toolUseItem ports tool_use_item (translate.rs:341-392).
func (g *GenericBackendTranslator) toolUseItem(block dto.ContentBlock) (map[string]any, error) {
	if block.ID == nil || block.Name == nil {
		return nil, nil
	}
	callID := *block.ID
	name := *block.Name
	input := block.Input
	if input == nil {
		input = map[string]any{}
	}

	switch g.kindForCall(callID) {
	case ToolCallKindCustom:
		return map[string]any{
			"type":    "custom_tool_call",
			"name":    name,
			"input":   customToolInputText(input),
			"call_id": callID,
		}, nil
	case ToolCallKindToolSearch:
		return map[string]any{
			"type":      "tool_search_call",
			"call_id":   callID,
			"execution": "client",
			"arguments": input,
		}, nil
	case ToolCallKindLocalShell:
		obj, _ := input.(map[string]any)
		status := "completed"
		if s, ok := obj["status"].(string); ok {
			status = s
		}
		var action any = map[string]any{}
		if a, ok := obj["action"]; ok {
			action = a
		}
		return map[string]any{
			"type":    "local_shell_call",
			"call_id": callID,
			"status":  status,
			"action":  action,
		}, nil
	default:
		arguments, err := json.Marshal(input)
		if err != nil {
			return nil, fmt.Errorf("tool_use.input must be JSON-serializable: %w", err)
		}
		return map[string]any{
			"type":      "function_call",
			"name":      name,
			"arguments": string(arguments),
			"call_id":   callID,
		}, nil
	}
}

// customToolInputText ports custom_tool_input_text (translate.rs:394-402).
func customToolInputText(input any) string {
	if obj, ok := input.(map[string]any); ok {
		if s, ok := obj["input"].(string); ok {
			return s
		}
	}
	encoded, err := json.Marshal(input)
	if err != nil {
		return ""
	}
	return string(encoded)
}

// toolResultItem ports tool_result_item (translate.rs:404-423).
func (g *GenericBackendTranslator) toolResultItem(block dto.ContentBlock) (map[string]any, bool) {
	if block.ToolUseID == nil {
		return nil, false
	}
	callID := *block.ToolUseID
	kind := g.kindForCall(callID)
	if kind == ToolCallKindToolSearch {
		return toolSearchOutputItem(callID, block), true
	}

	output := toolResultOutputValue(block)
	return map[string]any{
		"type":    kind.outputType(),
		"call_id": callID,
		"output":  output,
	}, true
}

// outputType ports CodexToolCallKind::output_type
// (crates/gateway-backend-codex/src/types.rs:105-111).
func (k ToolCallKind) outputType() string {
	switch k {
	case ToolCallKindCustom:
		return "custom_tool_call_output"
	case ToolCallKindToolSearch:
		return "tool_search_output"
	default:
		return "function_call_output"
	}
}

// toolSearchOutputItem ports tool_search_output_item (translate.rs:425-433).
func toolSearchOutputItem(callID string, block dto.ContentBlock) map[string]any {
	return map[string]any{
		"type":      ToolCallKindToolSearch.outputType(),
		"call_id":   callID,
		"status":    "completed",
		"execution": "client",
		"tools":     toolSearchTools(block),
	}
}

// toolSearchTools ports tool_search_tools (translate.rs:435-446).
func toolSearchTools(block dto.ContentBlock) any {
	if block.Content == nil {
		return []any{}
	}
	if obj, ok := block.Content.(map[string]any); ok {
		if tools, ok := obj["tools"].([]any); ok {
			return tools
		}
	}
	if arr, ok := block.Content.([]any); ok {
		return arr
	}
	return []any{}
}

// toolResultOutputValue ports tool_result_output_value (translate.rs:448-511).
func toolResultOutputValue(block dto.ContentBlock) any {
	if block.Content == nil {
		if block.Text != nil {
			return *block.Text
		}
		return ""
	}

	switch content := block.Content.(type) {
	case string:
		return content
	case []any:
		out := make([]any, 0, len(content))
		for _, item := range content {
			obj, ok := item.(map[string]any)
			if !ok {
				out = append(out, map[string]any{"type": "input_text", "text": encodeOrEmpty(item)})
				continue
			}
			itemType, _ := obj["type"].(string)
			switch itemType {
			case "text":
				if text, ok := obj["text"].(string); ok && strings.TrimSpace(text) != "" {
					out = append(out, map[string]any{"type": "input_text", "text": text})
				}
			case "image":
				source, _ := obj["source"].(map[string]any)
				sourceType, _ := source["type"].(string)
				if sourceType == "base64" {
					mediaType, mtOK := source["media_type"].(string)
					data, dataOK := source["data"].(string)
					if mtOK && dataOK {
						url := fmt.Sprintf("data:%s;base64,%s", mediaType, data)
						out = append(out, map[string]any{"type": "input_image", "image_url": url})
					}
				}
			default:
				out = append(out, map[string]any{"type": "input_text", "text": encodeOrEmpty(item)})
			}
		}
		if len(out) == 0 {
			return ""
		}
		return out
	default:
		return ""
	}
}

func encodeOrEmpty(v any) string {
	encoded, err := json.Marshal(v)
	if err != nil {
		return ""
	}
	return string(encoded)
}

type translatedTools struct {
	tools   []map[string]any
	include []string
}

// translateTools ports translate_tools (translate.rs:513-548).
func translateTools(tools []dto.Tool) (translatedTools, error) {
	out := make([]map[string]any, 0, len(tools))
	include := make([]string, 0)
	for _, t := range tools {
		if t.ToolType != nil {
			if *t.ToolType != anthropicWebSearchType {
				return translatedTools{}, fmt.Errorf("unsupported Anthropic hosted tool type `%s` for tool `%s`", *t.ToolType, t.Name)
			}
			tool, err := translateHostedWebSearchTool(t)
			if err != nil {
				return translatedTools{}, err
			}
			out = append(out, tool)
			if !containsString(include, webSearchSourcesInclude) {
				include = append(include, webSearchSourcesInclude)
			}
			continue
		}

		parameters, err := toolSchemaParametersForBackend(t.Name, t.InputSchema)
		if err != nil {
			return translatedTools{}, err
		}
		out = append(out, map[string]any{
			"type":        "function",
			"name":        t.Name,
			"description": derefStringOrNil(t.Description),
			"parameters":  parameters,
		})
	}
	return translatedTools{tools: out, include: include}, nil
}

func containsString(values []string, target string) bool {
	for _, v := range values {
		if v == target {
			return true
		}
	}
	return false
}

func derefStringOrNil(s *string) any {
	if s == nil {
		return nil
	}
	return *s
}

// translateHostedWebSearchTool ports translate_hosted_web_search_tool
// (translate.rs:550-600).
func translateHostedWebSearchTool(tool dto.Tool) (map[string]any, error) {
	if tool.Name != openAIWebSearchType {
		return nil, fmt.Errorf("anthropic `%s` tool must be named `%s`, got `%s`", anthropicWebSearchType, openAIWebSearchType, tool.Name)
	}
	if tool.InputSchema != nil {
		return nil, fmt.Errorf("anthropic `%s` tool must not include `input_schema`", anthropicWebSearchType)
	}
	if len(tool.BlockedDomains) > 0 {
		return nil, fmt.Errorf("anthropic `%s` field `blocked_domains` is unsupported by OpenAI web_search translation; remove blocked_domains or use allowed_domains", anthropicWebSearchType)
	}
	if len(tool.Extra) > 0 {
		keys := make([]string, 0, len(tool.Extra))
		for k := range tool.Extra {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		return nil, fmt.Errorf("anthropic `%s` has unsupported field(s): %s", anthropicWebSearchType, strings.Join(keys, ", "))
	}
	if tool.MaxUses != nil && *tool.MaxUses == 0 {
		return nil, fmt.Errorf("anthropic `%s` field `max_uses` must be greater than 0", anthropicWebSearchType)
	}

	obj := map[string]any{
		"type":                openAIWebSearchType,
		"external_web_access": true,
	}
	if len(tool.AllowedDomains) > 0 {
		obj["filters"] = map[string]any{"allowed_domains": append([]string(nil), tool.AllowedDomains...)}
	}
	return obj, nil
}

// toolSchemaParametersForBackend ports tool_schema_parameters_for_backend
// (translate.rs:602-609).
func toolSchemaParametersForBackend(toolName string, schema any) (map[string]any, error) {
	parameters, err := normalizeJSONSchemaParameters(schema)
	if err != nil {
		return nil, err
	}
	applyBackendToolSchemaPolicies(toolName, parameters)
	return parameters, nil
}

// applyBackendToolSchemaPolicies ports apply_backend_tool_schema_policies
// (translate.rs:611-621).
func applyBackendToolSchemaPolicies(toolName string, parameters map[string]any) {
	switch toolName {
	case "Agent":
		applyAgentToolSchemaPolicy(parameters)
	case "Read":
		applyReadToolSchemaPolicy(parameters)
	}
}

// applyAgentToolSchemaPolicy ports apply_agent_tool_schema_policy
// (translate.rs:623-637).
func applyAgentToolSchemaPolicy(obj map[string]any) {
	if properties, ok := obj["properties"].(map[string]any); ok {
		delete(properties, "isolation")
	}
	if required, ok := obj["required"].([]any); ok {
		filtered := make([]any, 0, len(required))
		for _, field := range required {
			if s, ok := field.(string); ok && s == "isolation" {
				continue
			}
			filtered = append(filtered, field)
		}
		obj["required"] = filtered
	}
}

// applyReadToolSchemaPolicy ports apply_read_tool_schema_policy
// (translate.rs:639-649).
func applyReadToolSchemaPolicy(obj map[string]any) {
	properties, ok := obj["properties"].(map[string]any)
	if !ok {
		return
	}
	rewriteReadIntegerProperty(properties, "offset", readOffsetDescription)
	rewriteReadIntegerProperty(properties, "limit", readLimitDescription)
}

// rewriteReadIntegerProperty ports rewrite_read_integer_property
// (translate.rs:651-672).
func rewriteReadIntegerProperty(properties map[string]any, propertyName, description string) {
	property, ok := properties[propertyName].(map[string]any)
	if !ok {
		return
	}
	property["type"] = "integer"
	property["minimum"] = 1
	property["description"] = description
}

// normalizeJSONSchemaParameters ports normalize_json_schema_parameters
// (translate.rs:674-704).
func normalizeJSONSchemaParameters(schema any) (map[string]any, error) {
	obj, ok := schema.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("tool input_schema must be a JSON object")
	}
	ty, _ := obj["type"].(string)
	if ty == "" {
		ty = "object"
	}
	if ty != "object" {
		return nil, fmt.Errorf("tool input_schema.type must be \"object\"")
	}

	properties, ok := obj["properties"]
	if !ok {
		properties = map[string]any{}
	}
	required, ok := obj["required"]
	if !ok {
		required = []any{}
	}
	additional, ok := obj["additionalProperties"]
	if !ok {
		additional = false
	}

	return map[string]any{
		"type":                 "object",
		"properties":           properties,
		"required":             required,
		"additionalProperties": additional,
	}, nil
}

// hasReadTool ports has_read_tool (translate.rs:706-710).
func hasReadTool(tools []dto.Tool) bool {
	for _, t := range tools {
		if t.ToolType == nil && t.Name == "Read" {
			return true
		}
	}
	return false
}

// translateToolChoice ports translate_tool_choice (translate.rs:712-735).
func translateToolChoice(toolChoice *dto.ToolChoice) string {
	if toolChoice == nil || toolChoice.Raw == nil {
		return "auto"
	}
	var v any
	if err := json.Unmarshal(toolChoice.Raw, &v); err != nil {
		return "auto"
	}
	if s, ok := v.(string); ok {
		return s
	}
	if obj, ok := v.(map[string]any); ok {
		if ty, ok := obj["type"].(string); ok {
			if ty == "auto" || ty == "any" {
				return "auto"
			}
		}
		if name, ok := obj["name"].(string); ok {
			return name
		}
	}
	return "auto"
}

// translateOutputConfig ports translate_output_config (translate.rs:737-759).
func translateOutputConfig(outputConfig *dto.OutputConfig) *map[string]any {
	if outputConfig == nil {
		return nil
	}
	format, ok := outputConfig.Format.(map[string]any)
	if !ok {
		return nil
	}
	formatType, _ := format["type"].(string)
	if formatType != "json_schema" {
		return nil
	}
	schema, ok := format["schema"]
	if !ok {
		schema = map[string]any{}
	}
	schema = normalizeOpenAIStrictResponseSchema(schema)

	result := map[string]any{
		"format": map[string]any{
			"type":   "json_schema",
			"strict": true,
			"schema": schema,
			"name":   "anthropic_output_config",
		},
	}
	return &result
}

// normalizeOpenAIStrictResponseSchema, normalizeOpenAIStrictSchemaValue,
// normalizeOpenAIStrictObjectSchema, isObjectSchema and makeSchemaNullable
// port the OpenAI strict-JSON-schema gate consumed by translate_output_config
// (crates/gateway-backend-codex/src/schema_gate.rs:9-13,49-182). That crate
// is Codex-backend-specific, but the request-side gate itself has no
// Codex-only behavior, so it lives here rather than being invented as a
// second file outside this task's scope.
func normalizeOpenAIStrictResponseSchema(schema any) any {
	return normalizeOpenAIStrictSchemaValue(schema)
}

func normalizeOpenAIStrictSchemaValue(schema any) any {
	obj, ok := schema.(map[string]any)
	if !ok {
		return schema
	}

	out := make(map[string]any, len(obj))
	for k, v := range obj {
		out[k] = v
	}

	if isObjectSchema(out) {
		normalizeOpenAIStrictObjectSchema(out)
	}

	for _, key := range []string{"items", "additionalProperties", "contains", "not", "if", "then", "else"} {
		if v, ok := out[key]; ok {
			out[key] = normalizeOpenAIStrictSchemaValue(v)
		}
	}

	for _, key := range []string{"anyOf", "oneOf", "allOf"} {
		if arr, ok := out[key].([]any); ok {
			newArr := make([]any, len(arr))
			for i, v := range arr {
				newArr[i] = normalizeOpenAIStrictSchemaValue(v)
			}
			out[key] = newArr
		}
	}

	for _, key := range []string{"$defs", "definitions"} {
		if defs, ok := out[key].(map[string]any); ok {
			newDefs := make(map[string]any, len(defs))
			for k, v := range defs {
				newDefs[k] = normalizeOpenAIStrictSchemaValue(v)
			}
			out[key] = newDefs
		}
	}

	return out
}

func isObjectSchema(obj map[string]any) bool {
	if _, ok := obj["properties"]; ok {
		return true
	}
	ty, ok := obj["type"].(string)
	return ok && ty == "object"
}

func normalizeOpenAIStrictObjectSchema(obj map[string]any) {
	obj["additionalProperties"] = false

	originalRequired := map[string]bool{}
	if reqArr, ok := obj["required"].([]any); ok {
		for _, r := range reqArr {
			if s, ok := r.(string); ok {
				originalRequired[s] = true
			}
		}
	}

	propertiesRaw, ok := obj["properties"]
	if !ok {
		propertiesRaw = map[string]any{}
	}
	properties, ok := propertiesRaw.(map[string]any)
	if !ok {
		obj["properties"] = propertiesRaw
		return
	}

	propertyNames := make([]string, 0, len(properties))
	for name := range properties {
		propertyNames = append(propertyNames, name)
	}
	sort.Strings(propertyNames)

	newProperties := make(map[string]any, len(properties))
	for _, name := range propertyNames {
		normalized := normalizeOpenAIStrictSchemaValue(properties[name])
		if !originalRequired[name] {
			normalized = makeSchemaNullable(normalized)
		}
		newProperties[name] = normalized
	}
	obj["properties"] = newProperties

	required := make([]any, len(propertyNames))
	for i, name := range propertyNames {
		required[i] = name
	}
	obj["required"] = required
}

func makeSchemaNullable(schema any) any {
	obj, ok := schema.(map[string]any)
	if !ok {
		return schema
	}
	out := make(map[string]any, len(obj))
	for k, v := range obj {
		out[k] = v
	}

	typeVal, hasType := out["type"]
	if !hasType {
		return map[string]any{
			"anyOf": []any{
				out,
				map[string]any{"type": "null"},
			},
		}
	}

	switch ty := typeVal.(type) {
	case string:
		if ty != "null" {
			out["type"] = []any{ty, "null"}
		}
	case []any:
		hasNull := false
		for _, v := range ty {
			if s, ok := v.(string); ok && s == "null" {
				hasNull = true
				break
			}
		}
		if !hasNull {
			out["type"] = append(append([]any{}, ty...), "null")
		}
	default:
		// Some(_) in Rust: leave unchanged (covers explicit JSON null and
		// any other non-string/array "type" value).
	}
	return out
}
