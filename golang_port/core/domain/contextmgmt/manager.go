// Package contextmgmt ports crates/gateway-http-anthropic/src/context_management.rs.
// It operates only on structured request/config fields (edit types, trigger
// thresholds, keep policies, hard limits) - never on prompt text content.
package contextmgmt

import (
	"encoding/json"
	"math"
	"strings"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

const (
	toolResultPlaceholder = "Tool result content cleared by gateway context management."
	thinkingPlaceholder   = "Thinking content cleared by gateway context management."
	defaultToolKeepUses   = 3
)

// Manager applies context-management edits and hard limits to a request's
// message history. Port of context_management.rs:106-153 (ContextManager).
type Manager struct {
	config config.ContextManagementConfig
}

// New constructs a Manager bound to the given config.
func New(cfg config.ContextManagementConfig) *Manager {
	return &Manager{config: cfg}
}

// Apply mutates req.Messages in place per the resolved edit policy and hard
// limits, returning req and a Report describing what was applied/ignored.
func (m *Manager) Apply(req *dto.MessagesRequest) (*dto.MessagesRequest, Report) {
	if !m.config.Enabled {
		return req, Report{}
	}

	policy := resolvePolicy(m.config, req.ContextManagement)
	report := Report{ignoredEditTypes: append([]string{}, policy.ignoredEditTypes...)}

	for _, edit := range policy.edits {
		switch {
		case strings.HasPrefix(edit.EditType, "clear_thinking"):
			if keep, ok := keepThinkingTurns(edit.Keep); ok {
				if applied := clearThinkingTurns(req.Messages, edit.EditType, keep); applied != nil {
					report.appliedEdits = append(report.appliedEdits, *applied)
				}
			}
		case strings.HasPrefix(edit.EditType, "clear_tool_uses"):
			if applied := applyToolUseEdit(req.Messages, edit); applied != nil {
				report.appliedEdits = append(report.appliedEdits, *applied)
			}
		default:
			report.ignoredEditTypes = append(report.ignoredEditTypes, edit.EditType)
		}
	}

	report.appliedEdits = append(report.appliedEdits, applyHardLimits(req.Messages, m.config.HardLimits)...)

	return req, report
}

// Report is the outcome of Apply: which edits were applied (with clear
// counts) and which requested/configured edit types were ignored.
type Report struct {
	appliedEdits     []appliedContextEdit
	ignoredEditTypes []string
}

// IsEmpty reports whether nothing was applied and nothing was ignored.
func (r Report) IsEmpty() bool {
	return len(r.appliedEdits) == 0 && len(r.ignoredEditTypes) == 0
}

// ResponseValue is the value to surface on the API response, or nil when no
// edits were applied.
func (r Report) ResponseValue() map[string]any {
	if len(r.appliedEdits) == 0 {
		return nil
	}
	return map[string]any{"applied_edits": r.appliedEditValues()}
}

// MetadataValue is the value to surface in observability metadata, or nil
// when the report is empty.
func (r Report) MetadataValue() map[string]any {
	if r.IsEmpty() {
		return nil
	}
	return map[string]any{
		"applied_edits":      r.appliedEditValues(),
		"ignored_edit_types": r.ignoredEditTypes,
	}
}

func (r Report) appliedEditValues() []map[string]any {
	values := make([]map[string]any, len(r.appliedEdits))
	for i, edit := range r.appliedEdits {
		values[i] = edit.responseValue()
	}
	return values
}

type appliedContextEdit struct {
	editType             string
	clearedToolUses      int
	clearedThinkingTurns int
	clearedInputTokens   int
	clearedChars         int
}

func newToolUsesEdit(editType string, clearedToolUses, clearedChars int) appliedContextEdit {
	return appliedContextEdit{
		editType:           editType,
		clearedToolUses:    clearedToolUses,
		clearedInputTokens: estimateTokens(clearedChars),
		clearedChars:       clearedChars,
	}
}

func newThinkingEdit(editType string, clearedThinkingTurns, clearedChars int) appliedContextEdit {
	return appliedContextEdit{
		editType:             editType,
		clearedThinkingTurns: clearedThinkingTurns,
		clearedInputTokens:   estimateTokens(clearedChars),
		clearedChars:         clearedChars,
	}
}

func (a appliedContextEdit) responseValue() map[string]any {
	value := map[string]any{
		"type":                 a.editType,
		"cleared_input_tokens": a.clearedInputTokens,
		"cleared_chars":        a.clearedChars,
	}
	if a.clearedToolUses > 0 {
		value["cleared_tool_uses"] = a.clearedToolUses
	}
	if a.clearedThinkingTurns > 0 {
		value["cleared_thinking_turns"] = a.clearedThinkingTurns
	}
	return value
}

// effectivePolicy is the resolved set of edits to apply and any edit
// descriptors that could not be parsed. Port of EffectiveContextPolicy.
type effectivePolicy struct {
	edits            []dto.ContextEdit
	ignoredEditTypes []string
}

// resolvePolicy ports ContextManagementPolicyResolver::resolve
// (context_management.rs:169-189). "follow_request" honors client-sent
// edits when present, else falls back to configured default_edits;
// "override_request" always uses configured override_edits regardless of
// what the client sent.
func resolvePolicy(cfg config.ContextManagementConfig, requestContext *dto.ContextManagement) effectivePolicy {
	if cfg.Mode == "override_request" {
		var values []map[string]any
		if cfg.OverrideEdits != nil {
			values = *cfg.OverrideEdits
		}
		return configPolicyFromValues(values)
	}

	if requestContext != nil && len(requestContext.Edits) > 0 {
		return effectivePolicy{edits: requestContext.Edits}
	}
	return configPolicyFromValues(cfg.DefaultEdits)
}

func configPolicyFromValues(values []map[string]any) effectivePolicy {
	var edits []dto.ContextEdit
	var ignored []string

	for _, value := range values {
		edit, ok := parseContextEdit(value)
		if ok {
			edits = append(edits, edit)
			continue
		}
		ignored = append(ignored, invalidEditType(value))
	}

	return effectivePolicy{edits: edits, ignoredEditTypes: ignored}
}

func parseContextEdit(value map[string]any) (dto.ContextEdit, bool) {
	editType, ok := value["type"].(string)
	if !ok || editType == "" {
		return dto.ContextEdit{}, false
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return dto.ContextEdit{}, false
	}
	var edit dto.ContextEdit
	if err := json.Unmarshal(encoded, &edit); err != nil {
		return dto.ContextEdit{}, false
	}
	return edit, true
}

func invalidEditType(value map[string]any) string {
	if editType, ok := value["type"].(string); ok && editType != "" {
		return editType
	}
	return "<invalid_context_edit>"
}

type blockRef struct {
	messageIndex int
	blockIndex   int
}

type toolInteraction struct {
	name        *string
	toolUse     blockRef
	toolResults []blockRef
}

type toolClearPolicy struct {
	editType        string
	keep            int
	excludeTools    map[string]bool
	clearToolInputs bool
	clearAtLeast    *dto.ContextThreshold
}

func applyToolUseEdit(messages []dto.Message, edit dto.ContextEdit) *appliedContextEdit {
	interactions := collectToolInteractions(messages)
	if len(interactions) == 0 || !toolTriggerIsActive(messages, interactions, edit) {
		return nil
	}

	keep, ok := keepToolUses(edit.Keep)
	if !ok {
		keep = defaultToolKeepUses
	}
	excludeTools := map[string]bool{}
	for _, tool := range edit.ExcludeTools {
		excludeTools[tool] = true
	}

	policy := toolClearPolicy{
		editType:        edit.EditType,
		keep:            keep,
		excludeTools:    excludeTools,
		clearToolInputs: edit.ClearToolInputs,
		clearAtLeast:    edit.ClearAtLeast,
	}
	return clearToolInteractions(messages, interactions, policy)
}

func applyHardLimits(messages []dto.Message, hardLimits config.ContextManagementHardLimits) []appliedContextEdit {
	var applied []appliedContextEdit

	if hardLimits.MaxToolUsesToKeep != nil {
		interactions := collectToolInteractions(messages)
		policy := toolClearPolicy{
			editType:     "clear_tool_uses_gateway_hard_limit",
			keep:         *hardLimits.MaxToolUsesToKeep,
			excludeTools: map[string]bool{},
		}
		if edit := clearToolInteractions(messages, interactions, policy); edit != nil {
			applied = append(applied, *edit)
		}
	}

	if hardLimits.MaxThinkingTurnsToKeep != nil {
		if edit := clearThinkingTurns(messages, "clear_thinking_gateway_hard_limit", *hardLimits.MaxThinkingTurnsToKeep); edit != nil {
			applied = append(applied, *edit)
		}
	}

	if hardLimits.MaxToolResultChars != nil {
		if edit := clearOversizedToolResults(messages, *hardLimits.MaxToolResultChars); edit != nil {
			applied = append(applied, *edit)
		}
	}

	return applied
}

func collectToolInteractions(messages []dto.Message) []toolInteraction {
	var interactions []toolInteraction
	byCallID := map[string]int{}

	for mi := range messages {
		blocks := messages[mi].Content.Blocks
		for bi := range blocks {
			block := &blocks[bi]
			switch block.BlockType {
			case "tool_use":
				if block.ID == nil {
					continue
				}
				index := len(interactions)
				interactions = append(interactions, toolInteraction{
					name:    block.Name,
					toolUse: blockRef{messageIndex: mi, blockIndex: bi},
				})
				byCallID[*block.ID] = index
			case "tool_result":
				if block.ToolUseID == nil {
					continue
				}
				if index, ok := byCallID[*block.ToolUseID]; ok {
					interactions[index].toolResults = append(interactions[index].toolResults, blockRef{messageIndex: mi, blockIndex: bi})
				}
			}
		}
	}

	return interactions
}

func toolTriggerIsActive(messages []dto.Message, interactions []toolInteraction, edit dto.ContextEdit) bool {
	trigger := edit.Trigger
	if trigger == nil {
		return true
	}

	switch trigger.ThresholdType {
	case "tool_uses":
		return len(interactions) > thresholdToInt(trigger)
	case "input_tokens":
		return estimatedMessageTokens(messages) > thresholdToInt(trigger)
	default:
		return false
	}
}

func clearToolInteractions(messages []dto.Message, interactions []toolInteraction, policy toolClearPolicy) *appliedContextEdit {
	if policy.keep >= len(interactions) {
		return nil
	}

	var eligible []int
	for index, interaction := range interactions {
		if interaction.name == nil || !policy.excludeTools[*interaction.name] {
			eligible = append(eligible, index)
		}
	}
	if policy.keep >= len(eligible) {
		return nil
	}

	clearCount := len(eligible) - policy.keep
	candidates := eligible[:clearCount]

	clearableChars := 0
	for _, index := range candidates {
		clearableChars += toolInteractionClearableChars(messages, interactions[index], policy)
	}

	if policy.clearAtLeast != nil && policy.clearAtLeast.ThresholdType == "input_tokens" {
		if estimateTokens(clearableChars) < thresholdToInt(policy.clearAtLeast) {
			return nil
		}
	}

	clearedToolUses := 0
	clearedChars := 0
	for _, index := range candidates {
		changed := clearToolInteraction(messages, interactions[index], policy)
		if changed > 0 {
			clearedToolUses++
			clearedChars += changed
		}
	}

	if clearedToolUses == 0 {
		return nil
	}
	edit := newToolUsesEdit(policy.editType, clearedToolUses, clearedChars)
	return &edit
}

func clearToolInteraction(messages []dto.Message, interaction toolInteraction, policy toolClearPolicy) int {
	clearedChars := 0

	for _, ref := range interaction.toolResults {
		block := blockAt(messages, ref)
		if block == nil {
			continue
		}
		clearedChars += toolResultChars(block)
		block.Content = toolResultPlaceholder
		block.Text = nil
	}

	if policy.clearToolInputs {
		if block := blockAt(messages, interaction.toolUse); block != nil {
			clearedChars += jsonChars(block.Input)
			block.Input = map[string]any{}
		}
	}

	return clearedChars
}

func toolInteractionClearableChars(messages []dto.Message, interaction toolInteraction, policy toolClearPolicy) int {
	resultChars := 0
	for _, ref := range interaction.toolResults {
		if block := blockAt(messages, ref); block != nil {
			resultChars += toolResultChars(block)
		}
	}

	if !policy.clearToolInputs {
		return resultChars
	}

	block := blockAt(messages, interaction.toolUse)
	if block == nil {
		return resultChars
	}
	return resultChars + jsonChars(block.Input)
}

func clearOversizedToolResults(messages []dto.Message, maxToolResultChars int) *appliedContextEdit {
	clearedToolUses := 0
	clearedChars := 0

	for mi := range messages {
		blocks := messages[mi].Content.Blocks
		for bi := range blocks {
			block := &blocks[bi]
			if block.BlockType != "tool_result" {
				continue
			}
			chars := toolResultChars(block)
			if chars <= maxToolResultChars {
				continue
			}
			block.Content = toolResultPlaceholder
			block.Text = nil
			clearedToolUses++
			clearedChars += chars
		}
	}

	if clearedToolUses == 0 {
		return nil
	}
	edit := newToolUsesEdit("clear_tool_uses_gateway_hard_limit_chars", clearedToolUses, clearedChars)
	return &edit
}

func clearThinkingTurns(messages []dto.Message, editType string, keepTurns int) *appliedContextEdit {
	var thinkingTurns []int
	for mi := range messages {
		if messages[mi].Role == "assistant" && messageHasThinking(&messages[mi]) {
			thinkingTurns = append(thinkingTurns, mi)
		}
	}
	if keepTurns >= len(thinkingTurns) {
		return nil
	}

	clearCount := len(thinkingTurns) - keepTurns
	clearedTurns := 0
	clearedChars := 0

	for _, mi := range thinkingTurns[:clearCount] {
		blocks := messages[mi].Content.Blocks
		changed := false
		for bi := range blocks {
			block := &blocks[bi]
			if !isThinkingBlockType(block.BlockType) {
				continue
			}
			clearedChars += thinkingChars(block)
			if block.Extra == nil {
				block.Extra = map[string]json.RawMessage{}
			}
			placeholder, _ := json.Marshal(thinkingPlaceholder)
			block.Extra["thinking"] = placeholder
			block.Text = nil
			changed = true
		}
		if changed {
			clearedTurns++
		}
	}

	if clearedTurns == 0 {
		return nil
	}
	edit := newThinkingEdit(editType, clearedTurns, clearedChars)
	return &edit
}

func keepToolUses(keep any) (int, bool) {
	return keepObjectValue(keep, "tool_uses")
}

// keepThinkingTurns ports keep_thinking_turns (context_management.rs:583-589).
// A false return means "keep all, apply no clearing" - either explicitly
// requested (`keep: "all"`) or an unparseable keep value; an absent keep
// field defaults to keeping the single most recent thinking turn.
func keepThinkingTurns(keep any) (int, bool) {
	if keep == nil {
		return 1, true
	}
	if value, ok := keep.(string); ok && value == "all" {
		return 0, false
	}
	return keepObjectValue(keep, "thinking_turns")
}

func keepObjectValue(keep any, expectedType string) (int, bool) {
	object, ok := keep.(map[string]any)
	if !ok {
		return 0, false
	}
	keepType, ok := object["type"].(string)
	if !ok || keepType != expectedType {
		return 0, false
	}
	rawValue, ok := object["value"]
	if !ok {
		return 0, false
	}
	value, ok := rawValue.(float64)
	if !ok || value < 0 {
		return 0, false
	}
	return int(value), true
}

func messageHasThinking(message *dto.Message) bool {
	for _, block := range message.Content.Blocks {
		if isThinkingBlockType(block.BlockType) {
			return true
		}
	}
	return false
}

func isThinkingBlockType(blockType string) bool {
	return blockType == "thinking" || blockType == "redacted_thinking"
}

func estimatedMessageTokens(messages []dto.Message) int {
	total := 0
	for i := range messages {
		total += messageChars(&messages[i])
	}
	return estimateTokens(total)
}

func messageChars(message *dto.Message) int {
	if message.Content.Text != nil {
		return runeLen(message.Content.Text)
	}
	total := 0
	for i := range message.Content.Blocks {
		block := &message.Content.Blocks[i]
		total += runeLen(block.Text) + jsonChars(block.Input) + toolResultChars(block) + thinkingChars(block)
	}
	return total
}

func estimateTokens(chars int) int {
	return (chars + 3) / 4
}

func thresholdToInt(threshold *dto.ContextThreshold) int {
	if threshold.Value > uint64(math.MaxInt) {
		return math.MaxInt
	}
	return int(threshold.Value)
}

func blockAt(messages []dto.Message, ref blockRef) *dto.ContentBlock {
	if ref.messageIndex < 0 || ref.messageIndex >= len(messages) {
		return nil
	}
	blocks := messages[ref.messageIndex].Content.Blocks
	if ref.blockIndex < 0 || ref.blockIndex >= len(blocks) {
		return nil
	}
	return &blocks[ref.blockIndex]
}

func toolResultChars(block *dto.ContentBlock) int {
	return jsonChars(block.Content) + runeLen(block.Text)
}

func thinkingChars(block *dto.ContentBlock) int {
	if block.Extra == nil {
		return 0
	}
	raw, ok := block.Extra["thinking"]
	if !ok {
		return 0
	}
	var text string
	if err := json.Unmarshal(raw, &text); err != nil {
		return 0
	}
	return len([]rune(text))
}

func jsonChars(value any) int {
	if value == nil {
		return 0
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return 0
	}
	return len([]rune(string(encoded)))
}

func runeLen(text *string) int {
	if text == nil {
		return 0
	}
	return len([]rune(*text))
}
