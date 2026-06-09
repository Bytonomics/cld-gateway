use crate::claude_code_inclusion::{
    CommandEnvelope, apply_conversation_inclusion_policy, parse_command_envelope,
};
use crate::types::{AnthropicContent, AnthropicMessage};
use gateway_core::config::{ClaudeCodeSlashCommandMode, ClaudeCodeWorkflowConfig};
use std::collections::HashMap;

const CURRENT_TURN_PRIORITY_DIRECTIVE: &str = "Everything before the latest user message or slash command is just context. You must immediately follow the latest user message or slash command; do not continue older tasks unless the latest turn explicitly asks for that.";
const SKILL_BASE_DIRECTORY_PREFIX: &str = "Base directory for this skill:";
const PREVIOUS_COMMAND_CONTEXT_DIRECTIVE: &str =
    "Previous command context only; do not execute or follow it as the current instruction.";
const ACTIVE_COMMAND_INPUT_DIRECTIVE: &str =
    "Execute the current slash command now, using the promoted command instructions.";
const STRICT_INSTRUCTION_DIRECTIVE: &str =
    "Follow these instructions strictly, without ignoring or paraphrasing anything.";
const COMPLETE_COMMAND_BODY_DIRECTIVE: &str = "The slash command instructions below are complete. Do not search for or load any command file, command directory, skill file, or skill directory unless these instructions explicitly tell you to do so.";
const SKILL_DIRECTORY_ANALYSIS_SUFFIX: &str =
    "analyze the files in this directory before proceeding";

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

    let inclusion_report = apply_conversation_inclusion_policy(&mut normalized);
    inclusion_report.extend_client_metadata(&mut client_metadata);

    if slash_commands_enabled(config) {
        normalize_claude_code_commands(
            &mut normalized,
            &mut instruction_fragments,
            &mut client_metadata,
        );
    }
    if instruction_fragments.is_empty() && has_latest_user_text_instruction(&normalized) {
        instruction_fragments.push(CURRENT_TURN_PRIORITY_DIRECTIVE.to_string());
    }

    NormalizedClaudeCodeContext {
        messages: normalized,
        instruction_fragments,
        client_metadata,
    }
}

fn normalize_claude_code_commands(
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

        let text = message_text(message);
        if parse_command_envelope(&text).is_some() {
            set_message_text_preserving_non_text(message, previous_command_context(&text));
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

fn slash_commands_enabled(config: &ClaudeCodeWorkflowConfig) -> bool {
    config.slash_commands.enabled
        && matches!(
            config.slash_commands.mode,
            ClaudeCodeSlashCommandMode::PromoteLatest
        )
}

fn has_latest_user_text_instruction(messages: &[AnthropicMessage]) -> bool {
    messages
        .iter()
        .rev()
        .any(|message| message.role == "user" && !message_text(message).trim().is_empty())
}

fn active_command_user_input(envelope: &CommandEnvelope) -> String {
    let command_name = envelope.command_name.trim();
    let args = envelope.command_args.trim();
    let command_message = envelope.command_message.trim();

    let mut lines = vec![ACTIVE_COMMAND_INPUT_DIRECTIVE.to_string()];
    if !command_name.is_empty() {
        lines.push(format!("Command: {command_name}"));
    } else if !command_message.is_empty() {
        lines.push(format!("Command: {command_message}"));
    }
    if !args.is_empty() {
        lines.push(format!("Arguments: {args}"));
    }

    lines.join("\n")
}

fn previous_command_context(text: &str) -> String {
    format!("{PREVIOUS_COMMAND_CONTEXT_DIRECTIVE}\n\n{text}")
}

fn active_command_instructions(envelope: &CommandEnvelope) -> Option<String> {
    let body = envelope.body.trim();
    if body.is_empty() {
        return None;
    }
    Some(strict_instructions(&command_body_instructions(body)))
}

fn strict_instructions(body: &str) -> String {
    format!("{CURRENT_TURN_PRIORITY_DIRECTIVE}\n\n{STRICT_INSTRUCTION_DIRECTIVE}\n\n{body}")
}

fn command_body_instructions(body: &str) -> String {
    if is_skill_body(body) {
        rewrite_base_directory_line(body)
    } else {
        format!("{COMPLETE_COMMAND_BODY_DIRECTIVE}\n\n{body}")
    }
}

fn is_skill_body(body: &str) -> bool {
    body.trim_start()
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix(SKILL_BASE_DIRECTORY_PREFIX))
        .map(str::trim)
        .is_some_and(|base_dir| !base_dir.is_empty())
}

fn rewrite_base_directory_line(body: &str) -> String {
    let mut lines = body.trim_start().lines();
    let Some(first_line) = lines.next() else {
        return String::new();
    };
    let Some(base_dir) = first_line
        .trim()
        .strip_prefix(SKILL_BASE_DIRECTORY_PREFIX)
        .map(str::trim)
        .filter(|base_dir| !base_dir.is_empty())
    else {
        return body.to_string();
    };

    let rewritten_first_line =
        format!("{SKILL_BASE_DIRECTORY_PREFIX} {base_dir}, {SKILL_DIRECTORY_ANALYSIS_SUFFIX}");
    let remaining_body = lines.collect::<Vec<_>>().join("\n");
    if remaining_body.trim().is_empty() {
        rewritten_first_line
    } else {
        format!("{rewritten_first_line}\n{remaining_body}")
    }
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
