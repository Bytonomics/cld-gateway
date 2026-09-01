#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};

use gateway_core::config::{
    ContextManagementConfig, ContextManagementHardLimits, ContextManagementMode,
};

use crate::types::{
    AnthropicContent, AnthropicContextEdit, AnthropicContextManagement, AnthropicContextThreshold,
    AnthropicMessage,
};

const TOOL_RESULT_PLACEHOLDER: &str = "Tool result content cleared by gateway context management.";
const THINKING_PLACEHOLDER: &str = "Thinking content cleared by gateway context management.";
const DEFAULT_TOOL_KEEP_USES: usize = 3;

#[derive(Debug, Clone, Default)]
pub(crate) struct ContextManagementReport {
    applied_edits: Vec<AppliedContextEdit>,
    ignored_edit_types: Vec<String>,
}

impl ContextManagementReport {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.applied_edits.is_empty() && self.ignored_edit_types.is_empty()
    }

    #[must_use]
    pub(crate) fn response_value(&self) -> Option<serde_json::Value> {
        if self.applied_edits.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "applied_edits": self
                .applied_edits
                .iter()
                .map(AppliedContextEdit::response_value)
                .collect::<Vec<_>>()
        }))
    }

    #[must_use]
    pub(crate) fn metadata_value(&self) -> Option<serde_json::Value> {
        if self.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "applied_edits": self
                .applied_edits
                .iter()
                .map(AppliedContextEdit::response_value)
                .collect::<Vec<_>>(),
            "ignored_edit_types": self.ignored_edit_types.clone()
        }))
    }
}

#[derive(Debug, Clone)]
struct AppliedContextEdit {
    edit_type: String,
    cleared_tool_uses: usize,
    cleared_thinking_turns: usize,
    cleared_input_tokens: usize,
    cleared_chars: usize,
}

impl AppliedContextEdit {
    fn tool_uses(edit_type: &str, cleared_tool_uses: usize, cleared_chars: usize) -> Self {
        Self {
            edit_type: edit_type.to_string(),
            cleared_tool_uses,
            cleared_thinking_turns: 0,
            cleared_input_tokens: estimate_tokens(cleared_chars),
            cleared_chars,
        }
    }

    fn thinking(edit_type: &str, cleared_thinking_turns: usize, cleared_chars: usize) -> Self {
        Self {
            edit_type: edit_type.to_string(),
            cleared_tool_uses: 0,
            cleared_thinking_turns,
            cleared_input_tokens: estimate_tokens(cleared_chars),
            cleared_chars,
        }
    }

    fn response_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "type": self.edit_type.clone(),
            "cleared_input_tokens": self.cleared_input_tokens,
            "cleared_chars": self.cleared_chars
        });
        if self.cleared_tool_uses > 0 {
            value["cleared_tool_uses"] = serde_json::Value::from(self.cleared_tool_uses);
        }
        if self.cleared_thinking_turns > 0 {
            value["cleared_thinking_turns"] = serde_json::Value::from(self.cleared_thinking_turns);
        }
        value
    }
}

pub(crate) struct ContextManager {
    config: ContextManagementConfig,
}

impl ContextManager {
    #[must_use]
    pub(crate) fn new(config: &ContextManagementConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    pub(crate) fn apply(
        &self,
        request_context: Option<&AnthropicContextManagement>,
        messages: &mut [AnthropicMessage],
    ) -> ContextManagementReport {
        if !self.config.enabled {
            return ContextManagementReport::default();
        }

        let resolver = ContextManagementPolicyResolver::new(&self.config);
        let policy = resolver.resolve(request_context);
        let editors: [&dyn ContextEditor; 2] = [&ThinkingContextEditor, &ToolUseContextEditor];

        let mut report = ContextManagementReport::default();
        report.ignored_edit_types.extend(policy.ignored_edit_types);

        for edit in &policy.edits {
            if let Some(editor) = editors
                .iter()
                .find(|editor| editor.supports(&edit.edit_type))
            {
                if let Some(applied) = editor.apply(messages, edit) {
                    report.applied_edits.push(applied);
                }
            } else {
                report.ignored_edit_types.push(edit.edit_type.clone());
            }
        }

        let hard_limit_editor = HardLimitContextEditor::new(&self.config.hard_limits);
        report
            .applied_edits
            .extend(hard_limit_editor.apply(messages));
        report
    }
}

struct EffectiveContextPolicy {
    edits: Vec<AnthropicContextEdit>,
    ignored_edit_types: Vec<String>,
}

struct ContextManagementPolicyResolver<'a> {
    config: &'a ContextManagementConfig,
}

impl<'a> ContextManagementPolicyResolver<'a> {
    fn new(config: &'a ContextManagementConfig) -> Self {
        Self { config }
    }

    fn resolve(
        &self,
        request_context: Option<&AnthropicContextManagement>,
    ) -> EffectiveContextPolicy {
        match self.config.mode {
            ContextManagementMode::FollowRequest => {
                if let Some(request_context) = request_context
                    && !request_context.edits.is_empty()
                {
                    return EffectiveContextPolicy {
                        edits: request_context.edits.clone(),
                        ignored_edit_types: Vec::new(),
                    };
                }
                Self::config_policy_from_values(&self.config.default_edits)
            }
            ContextManagementMode::OverrideRequest => Self::config_policy_from_values(
                self.config.override_edits.as_deref().unwrap_or_default(),
            ),
        }
    }

    fn config_policy_from_values(values: &[serde_json::Value]) -> EffectiveContextPolicy {
        let mut edits = Vec::new();
        let mut ignored_edit_types = Vec::new();

        for value in values {
            if let Ok(edit) = serde_json::from_value::<AnthropicContextEdit>(value.clone()) {
                edits.push(edit);
            } else {
                let edit_type = value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<invalid_context_edit>")
                    .to_string();
                ignored_edit_types.push(edit_type);
            }
        }

        EffectiveContextPolicy {
            edits,
            ignored_edit_types,
        }
    }
}

trait ContextEditor {
    fn supports(&self, edit_type: &str) -> bool;
    fn apply(
        &self,
        messages: &mut [AnthropicMessage],
        edit: &AnthropicContextEdit,
    ) -> Option<AppliedContextEdit>;
}

struct ToolUseContextEditor;

impl ContextEditor for ToolUseContextEditor {
    fn supports(&self, edit_type: &str) -> bool {
        edit_type.starts_with("clear_tool_uses")
    }

    fn apply(
        &self,
        messages: &mut [AnthropicMessage],
        edit: &AnthropicContextEdit,
    ) -> Option<AppliedContextEdit> {
        let interactions = collect_tool_interactions(messages);
        if interactions.is_empty() || !tool_trigger_is_active(messages, &interactions, edit) {
            return None;
        }

        let keep = keep_tool_uses(edit.keep.as_ref()).unwrap_or(DEFAULT_TOOL_KEEP_USES);
        let policy = ToolClearPolicy {
            edit_type: edit.edit_type.clone(),
            keep,
            exclude_tools: edit.exclude_tools.iter().cloned().collect(),
            clear_tool_inputs: edit.clear_tool_inputs,
            clear_at_least: edit.clear_at_least.clone(),
        };
        clear_tool_interactions(messages, &interactions, &policy)
    }
}

struct ThinkingContextEditor;

impl ContextEditor for ThinkingContextEditor {
    fn supports(&self, edit_type: &str) -> bool {
        edit_type.starts_with("clear_thinking")
    }

    fn apply(
        &self,
        messages: &mut [AnthropicMessage],
        edit: &AnthropicContextEdit,
    ) -> Option<AppliedContextEdit> {
        let keep = keep_thinking_turns(edit.keep.as_ref())?;
        clear_thinking_turns(messages, &edit.edit_type, keep)
    }
}

struct HardLimitContextEditor<'a> {
    hard_limits: &'a ContextManagementHardLimits,
}

impl<'a> HardLimitContextEditor<'a> {
    fn new(hard_limits: &'a ContextManagementHardLimits) -> Self {
        Self { hard_limits }
    }

    fn apply(&self, messages: &mut [AnthropicMessage]) -> Vec<AppliedContextEdit> {
        let mut applied = Vec::new();

        if let Some(max_tool_uses_to_keep) = self.hard_limits.max_tool_uses_to_keep {
            let interactions = collect_tool_interactions(messages);
            let policy = ToolClearPolicy {
                edit_type: "clear_tool_uses_gateway_hard_limit".to_string(),
                keep: max_tool_uses_to_keep,
                exclude_tools: HashSet::new(),
                clear_tool_inputs: false,
                clear_at_least: None,
            };
            if let Some(edit) = clear_tool_interactions(messages, &interactions, &policy) {
                applied.push(edit);
            }
        }

        if let Some(max_thinking_turns_to_keep) = self.hard_limits.max_thinking_turns_to_keep
            && let Some(edit) = clear_thinking_turns(
                messages,
                "clear_thinking_gateway_hard_limit",
                max_thinking_turns_to_keep,
            )
        {
            applied.push(edit);
        }

        if let Some(max_tool_result_chars) = self.hard_limits.max_tool_result_chars
            && let Some(edit) = clear_oversized_tool_results(messages, max_tool_result_chars)
        {
            applied.push(edit);
        }

        applied
    }
}

#[derive(Debug, Clone)]
struct ToolInteraction {
    name: Option<String>,
    tool_use: (usize, usize),
    tool_results: Vec<(usize, usize)>,
}

#[derive(Debug)]
struct ToolClearPolicy {
    edit_type: String,
    keep: usize,
    exclude_tools: HashSet<String>,
    clear_tool_inputs: bool,
    clear_at_least: Option<AnthropicContextThreshold>,
}

fn collect_tool_interactions(messages: &[AnthropicMessage]) -> Vec<ToolInteraction> {
    let mut interactions: Vec<ToolInteraction> = Vec::new();
    let mut by_call_id: BTreeMap<String, usize> = BTreeMap::new();

    for (message_index, message) in messages.iter().enumerate() {
        let AnthropicContent::Blocks(blocks) = &message.content else {
            continue;
        };
        for (block_index, block) in blocks.iter().enumerate() {
            match block.block_type.as_str() {
                "tool_use" => {
                    let Some(call_id) = block.id.as_deref() else {
                        continue;
                    };
                    let interaction_index = interactions.len();
                    interactions.push(ToolInteraction {
                        name: block.name.clone(),
                        tool_use: (message_index, block_index),
                        tool_results: Vec::new(),
                    });
                    by_call_id.insert(call_id.to_string(), interaction_index);
                }
                "tool_result" => {
                    let Some(call_id) = block.tool_use_id.as_deref() else {
                        continue;
                    };
                    if let Some(interaction_index) = by_call_id.get(call_id).copied() {
                        interactions[interaction_index]
                            .tool_results
                            .push((message_index, block_index));
                    }
                }
                _ => {}
            }
        }
    }

    interactions
}

fn tool_trigger_is_active(
    messages: &[AnthropicMessage],
    interactions: &[ToolInteraction],
    edit: &AnthropicContextEdit,
) -> bool {
    let Some(trigger) = edit.trigger.as_ref() else {
        return true;
    };

    match trigger.threshold_type.as_str() {
        "tool_uses" => interactions.len() > threshold_to_usize(trigger),
        "input_tokens" => estimated_message_tokens(messages) > threshold_to_usize(trigger),
        _ => false,
    }
}

fn clear_tool_interactions(
    messages: &mut [AnthropicMessage],
    interactions: &[ToolInteraction],
    policy: &ToolClearPolicy,
) -> Option<AppliedContextEdit> {
    if policy.keep >= interactions.len() {
        return None;
    }

    let eligible = interactions
        .iter()
        .enumerate()
        .filter(|(_, interaction)| {
            interaction
                .name
                .as_ref()
                .is_none_or(|name| !policy.exclude_tools.contains(name))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if policy.keep >= eligible.len() {
        return None;
    }

    let clear_count = eligible.len() - policy.keep;
    let candidates = eligible.into_iter().take(clear_count).collect::<Vec<_>>();
    let clearable_chars = candidates
        .iter()
        .map(|index| tool_interaction_clearable_chars(messages, &interactions[*index], policy))
        .sum::<usize>();

    if let Some(clear_at_least) = policy.clear_at_least.as_ref()
        && clear_at_least.threshold_type == "input_tokens"
        && estimate_tokens(clearable_chars) < threshold_to_usize(clear_at_least)
    {
        return None;
    }

    let mut cleared_tool_uses = 0usize;
    let mut cleared_chars = 0usize;
    for index in candidates {
        let interaction = &interactions[index];
        let changed = clear_tool_interaction(messages, interaction, policy);
        if changed > 0 {
            cleared_tool_uses = cleared_tool_uses.saturating_add(1);
            cleared_chars = cleared_chars.saturating_add(changed);
        }
    }

    (cleared_tool_uses > 0)
        .then(|| AppliedContextEdit::tool_uses(&policy.edit_type, cleared_tool_uses, cleared_chars))
}

fn clear_tool_interaction(
    messages: &mut [AnthropicMessage],
    interaction: &ToolInteraction,
    policy: &ToolClearPolicy,
) -> usize {
    let mut cleared_chars = 0usize;

    for (message_index, block_index) in &interaction.tool_results {
        let Some(block) = mutable_block(messages, *message_index, *block_index) else {
            continue;
        };
        cleared_chars = cleared_chars.saturating_add(tool_result_chars(block));
        block.content = Some(serde_json::Value::String(
            TOOL_RESULT_PLACEHOLDER.to_string(),
        ));
        block.text = None;
    }

    if policy.clear_tool_inputs {
        let (message_index, block_index) = interaction.tool_use;
        if let Some(block) = mutable_block(messages, message_index, block_index) {
            cleared_chars = cleared_chars.saturating_add(json_chars(block.input.as_ref()));
            block.input = Some(serde_json::Value::Object(serde_json::Map::new()));
        }
    }

    cleared_chars
}

fn tool_interaction_clearable_chars(
    messages: &[AnthropicMessage],
    interaction: &ToolInteraction,
    policy: &ToolClearPolicy,
) -> usize {
    let result_chars = interaction
        .tool_results
        .iter()
        .filter_map(|(message_index, block_index)| block(messages, *message_index, *block_index))
        .map(tool_result_chars)
        .sum::<usize>();

    if !policy.clear_tool_inputs {
        return result_chars;
    }

    let (message_index, block_index) = interaction.tool_use;
    result_chars.saturating_add(
        block(messages, message_index, block_index)
            .map_or(0, |tool_use| json_chars(tool_use.input.as_ref())),
    )
}

fn clear_oversized_tool_results(
    messages: &mut [AnthropicMessage],
    max_tool_result_chars: usize,
) -> Option<AppliedContextEdit> {
    let mut cleared_tool_uses = 0usize;
    let mut cleared_chars = 0usize;

    for message in messages {
        let AnthropicContent::Blocks(blocks) = &mut message.content else {
            continue;
        };
        for block in blocks {
            if block.block_type != "tool_result" {
                continue;
            }
            let chars = tool_result_chars(block);
            if chars <= max_tool_result_chars {
                continue;
            }
            block.content = Some(serde_json::Value::String(
                TOOL_RESULT_PLACEHOLDER.to_string(),
            ));
            block.text = None;
            cleared_tool_uses = cleared_tool_uses.saturating_add(1);
            cleared_chars = cleared_chars.saturating_add(chars);
        }
    }

    (cleared_tool_uses > 0).then(|| {
        AppliedContextEdit::tool_uses(
            "clear_tool_uses_gateway_hard_limit_chars",
            cleared_tool_uses,
            cleared_chars,
        )
    })
}

fn clear_thinking_turns(
    messages: &mut [AnthropicMessage],
    edit_type: &str,
    keep_turns: usize,
) -> Option<AppliedContextEdit> {
    let thinking_turns = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == "assistant" && message_has_thinking(message))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    if keep_turns >= thinking_turns.len() {
        return None;
    }

    let clear_count = thinking_turns.len() - keep_turns;
    let mut cleared_turns = 0usize;
    let mut cleared_chars = 0usize;

    for message_index in thinking_turns.into_iter().take(clear_count) {
        let Some(message) = messages.get_mut(message_index) else {
            continue;
        };
        let AnthropicContent::Blocks(blocks) = &mut message.content else {
            continue;
        };
        let mut changed = false;
        for block in blocks {
            if !is_thinking_block_type(&block.block_type) {
                continue;
            }
            cleared_chars = cleared_chars.saturating_add(thinking_chars(block));
            block.extra.insert(
                "thinking".to_string(),
                serde_json::Value::String(THINKING_PLACEHOLDER.to_string()),
            );
            block.text = None;
            changed = true;
        }
        if changed {
            cleared_turns = cleared_turns.saturating_add(1);
        }
    }

    (cleared_turns > 0)
        .then(|| AppliedContextEdit::thinking(edit_type, cleared_turns, cleared_chars))
}

fn keep_tool_uses(keep: Option<&serde_json::Value>) -> Option<usize> {
    keep_object_value(keep, "tool_uses")
}

fn keep_thinking_turns(keep: Option<&serde_json::Value>) -> Option<usize> {
    match keep {
        Some(serde_json::Value::String(value)) if value == "all" => None,
        Some(_) => keep_object_value(keep, "thinking_turns"),
        None => Some(1),
    }
}

fn keep_object_value(keep: Option<&serde_json::Value>, expected_type: &str) -> Option<usize> {
    let keep = keep?;
    let keep_type = keep.get("type").and_then(serde_json::Value::as_str)?;
    if keep_type != expected_type {
        return None;
    }
    keep.get("value")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn message_has_thinking(message: &AnthropicMessage) -> bool {
    let AnthropicContent::Blocks(blocks) = &message.content else {
        return false;
    };
    blocks
        .iter()
        .any(|block| is_thinking_block_type(&block.block_type))
}

fn is_thinking_block_type(block_type: &str) -> bool {
    matches!(block_type, "thinking" | "redacted_thinking")
}

fn estimated_message_tokens(messages: &[AnthropicMessage]) -> usize {
    estimate_tokens(
        messages
            .iter()
            .map(message_chars)
            .fold(0usize, usize::saturating_add),
    )
}

fn message_chars(message: &AnthropicMessage) -> usize {
    match &message.content {
        AnthropicContent::Text(text) => text.chars().count(),
        AnthropicContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .text
                    .as_ref()
                    .map_or(0, |text| text.chars().count())
                    .saturating_add(json_chars(block.input.as_ref()))
                    .saturating_add(tool_result_chars(block))
                    .saturating_add(thinking_chars(block))
            })
            .fold(0usize, usize::saturating_add),
    }
}

fn estimate_tokens(chars: usize) -> usize {
    chars.saturating_add(3) / 4
}

fn threshold_to_usize(threshold: &AnthropicContextThreshold) -> usize {
    usize::try_from(threshold.value).unwrap_or(usize::MAX)
}

fn mutable_block(
    messages: &mut [AnthropicMessage],
    message_index: usize,
    block_index: usize,
) -> Option<&mut crate::types::AnthropicContentBlock> {
    let message = messages.get_mut(message_index)?;
    let AnthropicContent::Blocks(blocks) = &mut message.content else {
        return None;
    };
    blocks.get_mut(block_index)
}

fn block(
    messages: &[AnthropicMessage],
    message_index: usize,
    block_index: usize,
) -> Option<&crate::types::AnthropicContentBlock> {
    let message = messages.get(message_index)?;
    let AnthropicContent::Blocks(blocks) = &message.content else {
        return None;
    };
    blocks.get(block_index)
}

fn tool_result_chars(block: &crate::types::AnthropicContentBlock) -> usize {
    json_chars(block.content.as_ref())
        .saturating_add(block.text.as_ref().map_or(0, |text| text.chars().count()))
}

fn thinking_chars(block: &crate::types::AnthropicContentBlock) -> usize {
    block
        .extra
        .get("thinking")
        .and_then(serde_json::Value::as_str)
        .map_or(0, |thinking| thinking.chars().count())
}

fn json_chars(value: Option<&serde_json::Value>) -> usize {
    value.map_or(0, |value| {
        serde_json::to_string(value).map_or(0, |encoded| encoded.chars().count())
    })
}

#[cfg(test)]
mod tests {
    use gateway_core::config::{
        ContextManagementConfig, ContextManagementHardLimits, ContextManagementMode,
    };

    use super::*;
    use crate::types::AnthropicContentBlock;

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> AnthropicContentBlock {
        AnthropicContentBlock {
            block_type: "tool_use".to_string(),
            text: None,
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            input: Some(input),
            tool_use_id: None,
            content: None,
            is_error: None,
            source: None,
            extra: BTreeMap::new(),
        }
    }

    fn tool_result(id: &str, content: &str) -> AnthropicContentBlock {
        AnthropicContentBlock {
            block_type: "tool_result".to_string(),
            text: None,
            id: None,
            name: None,
            input: None,
            tool_use_id: Some(id.to_string()),
            content: Some(serde_json::Value::String(content.to_string())),
            is_error: Some(false),
            source: None,
            extra: BTreeMap::new(),
        }
    }

    fn thinking(content: &str) -> AnthropicContentBlock {
        AnthropicContentBlock {
            block_type: "thinking".to_string(),
            text: None,
            id: None,
            name: None,
            input: None,
            tool_use_id: None,
            content: None,
            is_error: None,
            source: None,
            extra: BTreeMap::from([(
                "thinking".to_string(),
                serde_json::Value::String(content.to_string()),
            )]),
        }
    }

    fn message(role: &str, blocks: Vec<AnthropicContentBlock>) -> AnthropicMessage {
        AnthropicMessage {
            role: role.to_string(),
            content: AnthropicContent::Blocks(blocks),
        }
    }

    fn edit(value: serde_json::Value) -> AnthropicContextEdit {
        serde_json::from_value(value).expect("parse edit")
    }

    #[test]
    fn clears_old_tool_results_and_preserves_recent_pair() {
        let mut messages = vec![
            message(
                "assistant",
                vec![tool_use(
                    "call_old",
                    "Read",
                    serde_json::json!({"file_path":"a"}),
                )],
            ),
            message("user", vec![tool_result("call_old", "old file content")]),
            message(
                "assistant",
                vec![tool_use(
                    "call_new",
                    "Read",
                    serde_json::json!({"file_path":"b"}),
                )],
            ),
            message("user", vec![tool_result("call_new", "new file content")]),
        ];
        let config = ContextManagementConfig::default();
        let manager = ContextManager::new(&config);
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 1}
            }))],
        };

        let report = manager.apply(Some(&request_context), &mut messages);

        assert!(report.response_value().is_some());
        let AnthropicContent::Blocks(old_result_blocks) = &messages[1].content else {
            panic!("blocks");
        };
        assert_eq!(
            old_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some(TOOL_RESULT_PLACEHOLDER)
        );
        let AnthropicContent::Blocks(new_result_blocks) = &messages[3].content else {
            panic!("blocks");
        };
        assert_eq!(
            new_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some("new file content")
        );
    }

    #[test]
    fn clear_tool_inputs_only_when_requested() {
        let mut messages = vec![
            message(
                "assistant",
                vec![tool_use(
                    "call_old",
                    "Read",
                    serde_json::json!({"file_path":"a"}),
                )],
            ),
            message("user", vec![tool_result("call_old", "old file content")]),
            message(
                "assistant",
                vec![tool_use(
                    "call_new",
                    "Read",
                    serde_json::json!({"file_path":"b"}),
                )],
            ),
            message("user", vec![tool_result("call_new", "new file content")]),
        ];
        let manager = ContextManager::new(&ContextManagementConfig::default());
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 1},
                "clear_tool_inputs": true
            }))],
        };

        manager.apply(Some(&request_context), &mut messages);

        let AnthropicContent::Blocks(old_tool_blocks) = &messages[0].content else {
            panic!("blocks");
        };
        assert_eq!(old_tool_blocks[0].input, Some(serde_json::json!({})));
        let AnthropicContent::Blocks(new_tool_blocks) = &messages[2].content else {
            panic!("blocks");
        };
        assert_eq!(
            new_tool_blocks[0].input,
            Some(serde_json::json!({"file_path":"b"}))
        );
    }

    #[test]
    fn excluded_tools_are_not_cleared() {
        let mut messages = vec![
            message(
                "assistant",
                vec![tool_use(
                    "call_search",
                    "web_search",
                    serde_json::json!({"query":"a"}),
                )],
            ),
            message("user", vec![tool_result("call_search", "search result")]),
            message(
                "assistant",
                vec![tool_use(
                    "call_read",
                    "Read",
                    serde_json::json!({"file_path":"b"}),
                )],
            ),
            message("user", vec![tool_result("call_read", "file content")]),
        ];
        let manager = ContextManager::new(&ContextManagementConfig::default());
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 0},
                "exclude_tools": ["web_search"]
            }))],
        };

        manager.apply(Some(&request_context), &mut messages);

        let AnthropicContent::Blocks(search_result_blocks) = &messages[1].content else {
            panic!("blocks");
        };
        assert_eq!(
            search_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some("search result")
        );
        let AnthropicContent::Blocks(read_result_blocks) = &messages[3].content else {
            panic!("blocks");
        };
        assert_eq!(
            read_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some(TOOL_RESULT_PLACEHOLDER)
        );
    }

    #[test]
    fn clears_old_thinking_turns() {
        let mut messages = vec![
            message("assistant", vec![thinking("old thought")]),
            message("assistant", vec![thinking("new thought")]),
        ];
        let manager = ContextManager::new(&ContextManagementConfig::default());
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_thinking_20251015",
                "keep": {"type": "thinking_turns", "value": 1}
            }))],
        };

        let report = manager.apply(Some(&request_context), &mut messages);

        assert!(report.response_value().is_some());
        let AnthropicContent::Blocks(old_blocks) = &messages[0].content else {
            panic!("blocks");
        };
        assert_eq!(
            old_blocks[0]
                .extra
                .get("thinking")
                .and_then(serde_json::Value::as_str),
            Some(THINKING_PLACEHOLDER)
        );
        let AnthropicContent::Blocks(new_blocks) = &messages[1].content else {
            panic!("blocks");
        };
        assert_eq!(
            new_blocks[0]
                .extra
                .get("thinking")
                .and_then(serde_json::Value::as_str),
            Some("new thought")
        );
    }

    #[test]
    fn config_override_ignores_request_edits() {
        let mut messages = vec![
            message(
                "assistant",
                vec![tool_use(
                    "call_old",
                    "Read",
                    serde_json::json!({"file_path":"a"}),
                )],
            ),
            message("user", vec![tool_result("call_old", "old file content")]),
            message(
                "assistant",
                vec![tool_use(
                    "call_new",
                    "Read",
                    serde_json::json!({"file_path":"b"}),
                )],
            ),
            message("user", vec![tool_result("call_new", "new file content")]),
        ];
        let config = ContextManagementConfig {
            mode: ContextManagementMode::OverrideRequest,
            override_edits: Some(vec![serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 0}
            })]),
            ..ContextManagementConfig::default()
        };
        let manager = ContextManager::new(&config);
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 2}
            }))],
        };

        manager.apply(Some(&request_context), &mut messages);

        let AnthropicContent::Blocks(new_result_blocks) = &messages[3].content else {
            panic!("blocks");
        };
        assert_eq!(
            new_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some(TOOL_RESULT_PLACEHOLDER)
        );
    }

    #[test]
    fn disabled_config_does_not_prune() {
        let mut messages = vec![
            message(
                "assistant",
                vec![tool_use(
                    "call_old",
                    "Read",
                    serde_json::json!({"file_path":"a"}),
                )],
            ),
            message("user", vec![tool_result("call_old", "old file content")]),
            message(
                "assistant",
                vec![tool_use(
                    "call_new",
                    "Read",
                    serde_json::json!({"file_path":"b"}),
                )],
            ),
            message("user", vec![tool_result("call_new", "new file content")]),
        ];
        let config = ContextManagementConfig {
            enabled: false,
            ..ContextManagementConfig::default()
        };
        let manager = ContextManager::new(&config);
        let request_context = AnthropicContextManagement {
            edits: vec![edit(serde_json::json!({
                "type": "clear_tool_uses_20250919",
                "trigger": {"type": "tool_uses", "value": 1},
                "keep": {"type": "tool_uses", "value": 0}
            }))],
        };

        manager.apply(Some(&request_context), &mut messages);

        let AnthropicContent::Blocks(old_result_blocks) = &messages[1].content else {
            panic!("blocks");
        };
        assert_eq!(
            old_result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some("old file content")
        );
    }

    #[test]
    fn hard_limit_clears_oversized_tool_result() {
        let mut messages = vec![message(
            "user",
            vec![tool_result("call_old", "very long content")],
        )];
        let config = ContextManagementConfig {
            hard_limits: ContextManagementHardLimits {
                max_tool_result_chars: Some(5),
                ..ContextManagementHardLimits::default()
            },
            ..ContextManagementConfig::default()
        };
        let manager = ContextManager::new(&config);

        let report = manager.apply(None, &mut messages);

        assert!(report.response_value().is_some());
        let AnthropicContent::Blocks(result_blocks) = &messages[0].content else {
            panic!("blocks");
        };
        assert_eq!(
            result_blocks[0]
                .content
                .as_ref()
                .and_then(serde_json::Value::as_str),
            Some(TOOL_RESULT_PLACEHOLDER)
        );
    }
}
