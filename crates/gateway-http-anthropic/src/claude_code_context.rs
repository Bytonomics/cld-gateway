use crate::types::{AnthropicContent, AnthropicMessage};
use gateway_core::config::{
    ClaudeCodeSkillMode, ClaudeCodeSlashCommandMode, ClaudeCodeWorkflowConfig,
};
use std::collections::HashMap;

const COMMAND_MESSAGE_TAG: &str = "command-message";
const COMMAND_NAME_TAG: &str = "command-name";
const COMMAND_ARGS_TAG: &str = "command-args";
const SKILL_INSTRUCTIONS_TAG: &str = "skill-instructions";
const SKILL_ARGS_TAG: &str = "skill-args";
const SKILL_TAG: &str = "skill";
const STRICT_INSTRUCTION_DIRECTIVE: &str = "Follow these instructions strictly, while strictly following the order of statements, without ignoring or paraphrasing anything. Missing anything will cause catastrophic failure";

#[derive(Debug, Clone)]
pub(crate) struct NormalizedClaudeCodeContext {
    pub(crate) messages: Vec<AnthropicMessage>,
    pub(crate) instruction_fragments: Vec<String>,
    pub(crate) client_metadata: HashMap<String, String>,
}

pub(crate) fn normalize_claude_code_context(
    messages: &[AnthropicMessage],
    config: &ClaudeCodeWorkflowConfig,
) -> NormalizedClaudeCodeContext {
    let mut normalized = messages.to_vec();
    let mut instruction_fragments = Vec::new();
    let mut client_metadata = HashMap::new();

    if slash_commands_enabled(config) {
        normalize_slash_commands(
            &mut normalized,
            &mut instruction_fragments,
            &mut client_metadata,
        );
    }

    if skills_enabled(config) && instruction_fragments.is_empty() {
        normalize_active_skill(
            &mut normalized,
            &mut instruction_fragments,
            &mut client_metadata,
        );
    }

    NormalizedClaudeCodeContext {
        messages: normalized,
        instruction_fragments,
        client_metadata,
    }
}

fn normalize_slash_commands(
    messages: &mut [AnthropicMessage],
    instruction_fragments: &mut Vec<String>,
    client_metadata: &mut HashMap<String, String>,
) {
    let latest_user_instruction = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            if message.role != "user" {
                return None;
            }
            let text = message_text(message);
            (!text.trim().is_empty()).then_some((index, text))
        });
    let active = latest_user_instruction
        .and_then(|(index, text)| parse_command_envelope(&text).map(|envelope| (index, envelope)));

    for (index, message) in messages.iter_mut().enumerate() {
        if active
            .as_ref()
            .is_some_and(|(active_index, _)| index == *active_index)
            || message.role != "user"
        {
            continue;
        }

        if parse_command_envelope(&message_text(message)).is_some() {
            set_message_text_preserving_non_text(message, String::new());
        }
    }

    let Some((active_index, active_envelope)) = active else {
        return;
    };

    let active_user_input = active_command_user_input(&active_envelope);
    set_message_text_preserving_non_text(&mut messages[active_index], active_user_input);
    if let Some(instructions) = active_command_instructions(&active_envelope) {
        instruction_fragments.push(instructions);
    }
    client_metadata.insert(
        "claude_code_slash_command".to_string(),
        active_envelope.command_name.trim().to_string(),
    );
}

fn normalize_active_skill(
    messages: &mut [AnthropicMessage],
    instruction_fragments: &mut Vec<String>,
    client_metadata: &mut HashMap<String, String>,
) {
    let active_index = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| (message.role == "user").then_some(index))
        .and_then(|index| {
            parse_skill_envelope(&message_text(&messages[index])).map(|envelope| (index, envelope))
        });

    let Some((active_index, active_envelope)) = active_index else {
        return;
    };

    let active_user_input = if active_envelope.args.trim().is_empty() {
        active_envelope.remainder.trim().to_string()
    } else {
        active_envelope.args.trim().to_string()
    };
    set_message_text_preserving_non_text(&mut messages[active_index], active_user_input);
    if let Some(instructions) = active_skill_instructions(&active_envelope) {
        instruction_fragments.push(instructions);
    }
    client_metadata.insert("claude_code_skill".to_string(), "expanded".to_string());
}

fn slash_commands_enabled(config: &ClaudeCodeWorkflowConfig) -> bool {
    config.slash_commands.enabled
        && matches!(
            config.slash_commands.mode,
            ClaudeCodeSlashCommandMode::PromoteLatest
        )
}

fn skills_enabled(config: &ClaudeCodeWorkflowConfig) -> bool {
    config.skills.enabled && matches!(config.skills.mode, ClaudeCodeSkillMode::PromoteActive)
}

#[derive(Debug, Clone)]
struct CommandEnvelope {
    command_message: String,
    command_name: String,
    command_args: String,
    body: String,
}

#[derive(Debug, Clone)]
struct SkillEnvelope {
    instructions: String,
    args: String,
    remainder: String,
}

fn parse_command_envelope(text: &str) -> Option<CommandEnvelope> {
    let command_name_match = last_tag_match(text, COMMAND_NAME_TAG)?;
    let command_name = command_name_match.value;
    let command_message = text
        .get(..command_name_match.start)
        .and_then(|prefix| last_tag_match(prefix, COMMAND_MESSAGE_TAG))
        .map_or_else(String::new, |tag| tag.value);
    let command_args_match = text
        .get(command_name_match.end..)
        .and_then(|suffix| first_tag_match(suffix, COMMAND_ARGS_TAG))
        .map(|tag| tag.with_offset(command_name_match.end));
    let command_args = command_args_match
        .as_ref()
        .map_or_else(String::new, |tag| tag.value.clone());
    let body_start = command_args_match
        .as_ref()
        .map_or(command_name_match.end, |tag| tag.end);
    let body = text
        .get(body_start..)
        .unwrap_or_default()
        .trim()
        .to_string();

    Some(CommandEnvelope {
        command_message,
        command_name,
        command_args,
        body,
    })
}

fn parse_skill_envelope(text: &str) -> Option<SkillEnvelope> {
    let instructions =
        tag_value(text, SKILL_INSTRUCTIONS_TAG).or_else(|| tag_value(text, SKILL_TAG))?;
    let args = tag_value(text, SKILL_ARGS_TAG).unwrap_or_default();
    let body_end = tag_end_index(text, SKILL_INSTRUCTIONS_TAG)
        .or_else(|| tag_end_index(text, SKILL_TAG))
        .unwrap_or_default();
    let remainder = text.get(body_end..).unwrap_or_default().trim().to_string();

    Some(SkillEnvelope {
        instructions,
        args,
        remainder,
    })
}

fn active_command_user_input(envelope: &CommandEnvelope) -> String {
    let args = envelope.command_args.trim();
    if !args.is_empty() {
        return args.to_string();
    }

    let command_message = envelope.command_message.trim();
    let command_name = envelope.command_name.trim();
    if !command_message.is_empty() {
        command_message.to_string()
    } else if !command_name.is_empty() {
        command_name.to_string()
    } else {
        String::new()
    }
}

fn active_command_instructions(envelope: &CommandEnvelope) -> Option<String> {
    let body = envelope.body.trim();
    (!body.is_empty()).then(|| strict_instructions(body))
}

fn active_skill_instructions(envelope: &SkillEnvelope) -> Option<String> {
    let body = envelope.instructions.trim();
    (!body.is_empty()).then(|| strict_instructions(body))
}

fn strict_instructions(body: &str) -> String {
    format!("{STRICT_INSTRUCTION_DIRECTIVE}\n\n{body}")
}

fn message_text(message: &AnthropicMessage) -> String {
    match &message.content {
        AnthropicContent::Text(text) => text.clone(),
        AnthropicContent::Blocks(blocks) => blocks
            .iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn set_message_text_preserving_non_text(message: &mut AnthropicMessage, text: String) {
    match &mut message.content {
        AnthropicContent::Text(_) => {
            message.content = AnthropicContent::Text(text);
        }
        AnthropicContent::Blocks(blocks) => {
            let mut preserved_blocks = blocks
                .iter()
                .filter(|block| block.block_type != "text")
                .cloned()
                .collect::<Vec<_>>();
            if !text.trim().is_empty() {
                preserved_blocks.push(text_block(text));
            }
            message.content = AnthropicContent::Blocks(preserved_blocks);
        }
    }
}

fn text_block(text: String) -> crate::types::AnthropicContentBlock {
    crate::types::AnthropicContentBlock {
        block_type: "text".to_string(),
        text: Some(text),
        id: None,
        name: None,
        input: None,
        tool_use_id: None,
        content: None,
        is_error: None,
        source: None,
        extra: std::collections::BTreeMap::default(),
    }
}

#[derive(Debug, Clone)]
struct TagMatch {
    start: usize,
    end: usize,
    value: String,
}

impl TagMatch {
    fn with_offset(self, offset: usize) -> Self {
        Self {
            start: self.start + offset,
            end: self.end + offset,
            value: self.value,
        }
    }
}

fn first_tag_match(text: &str, tag: &str) -> Option<TagMatch> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let value_start = start + open.len();
    let value_end = text.get(value_start..)?.find(&close)? + value_start;
    let end = value_end + close.len();
    Some(TagMatch {
        start,
        end,
        value: text.get(value_start..value_end)?.trim().to_string(),
    })
}

fn last_tag_match(text: &str, tag: &str) -> Option<TagMatch> {
    let mut remaining = text;
    let mut offset = 0;
    let mut latest = None;

    while let Some(tag_match) = first_tag_match(remaining, tag) {
        let absolute = tag_match.with_offset(offset);
        offset = absolute.end;
        remaining = text.get(offset..).unwrap_or_default();
        latest = Some(absolute);
    }

    latest
}

fn tag_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text.get(start..)?.find(&close)? + start;
    text.get(start..end).map(|value| value.trim().to_string())
}

fn tag_end_index(text: &str, tag: &str) -> Option<usize> {
    let close = format!("</{tag}>");
    text.find(&close).map(|index| index + close.len())
}
