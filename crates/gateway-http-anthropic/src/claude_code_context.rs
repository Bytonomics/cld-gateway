use crate::claude_code_inclusion::{
    ClaudeCodeCommandClassification, CommandEnvelope, apply_conversation_inclusion_policy,
    classify_claude_code_command, parse_command_envelope,
};
use crate::types::{AnthropicContent, AnthropicMessage, AnthropicSystemBlock};
use gateway_core::config::{ClaudeCodeSlashCommandMode, ClaudeCodeWorkflowConfig};
use std::collections::HashMap;

const CURRENT_TURN_PRIORITY_DIRECTIVE: &str = "Follow the prompt coming with this instruction as an immediate and urgent need right now.  Everything before this in the conversation history is only the context leading up to this instruction.  And you are not supposed to do anything else except follow these instructions unless you have completed all of the instructions with 100% compliance.";
const SKILL_BASE_DIRECTORY_PREFIX: &str = "Base directory for this skill:";
const PREVIOUS_COMMAND_CONTEXT_DIRECTIVE: &str =
    "Previous command context only; do not execute or follow it as the current instruction.";
const SKILL_DIRECTORY_ANALYSIS_SUFFIX: &str =
    "analyze the files in this directory before proceeding";
const PACKAGED_CODEX_STATUS_COMMAND: &str =
    include_str!("../../../scripts/release/cld_gateway_package/commands/codex/status.md");
const TRANSLATED_COMMAND_BODIES: &[(&str, &str)] = &[("status", PACKAGED_CODEX_STATUS_COMMAND)];

#[derive(Debug, Clone)]
pub(crate) struct NormalizedClaudeCodeContext {
    pub(crate) system: Vec<AnthropicSystemBlock>,
    pub(crate) messages: Vec<AnthropicMessage>,
    pub(crate) instruction_fragments: Vec<String>,
    pub(crate) client_metadata: HashMap<String, String>,
}

pub(crate) fn normalize_claude_code_context(
    system: &[AnthropicSystemBlock],
    messages: &[AnthropicMessage],
    config: &ClaudeCodeWorkflowConfig,
) -> NormalizedClaudeCodeContext {
    let system = system.to_vec();
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
        system,
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
    let dispatch = active_command_dispatch(&active_envelope);
    if dispatch == ActiveCommandDispatch::PromptBacked {
        let active_user_input = active_prompt_backed_command_text(
            &message_text(&messages[active_index]),
            &active_envelope,
        );
        set_message_text_preserving_non_text(&mut messages[active_index], active_user_input);
    } else {
        set_message_text_preserving_non_text(&mut messages[active_index], String::new());
    }
    if let Some(instructions) = active_command_instructions(dispatch) {
        instruction_fragments.push(instructions);
    }
    client_metadata.insert(
        "claude_code_slash_command".to_string(),
        active_envelope.command_name.trim().to_string(),
    );
    if dispatch == ActiveCommandDispatch::Translated {
        client_metadata.insert(
            "claude_code_translated_slash_command".to_string(),
            active_envelope.command_name.trim().to_string(),
        );
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveCommandDispatch {
    PromptBacked,
    Translated,
}

fn active_command_dispatch(envelope: &CommandEnvelope) -> ActiveCommandDispatch {
    match classify_claude_code_command(envelope.command_name.as_str()) {
        ClaudeCodeCommandClassification::Translate => ActiveCommandDispatch::Translated,
        ClaudeCodeCommandClassification::PromptBacked
        | ClaudeCodeCommandClassification::LocalOnly => ActiveCommandDispatch::PromptBacked,
    }
}

fn normalize_command_name(command_name: &str) -> &str {
    command_name.trim().trim_start_matches('/')
}

fn active_prompt_backed_command_text(original_text: &str, envelope: &CommandEnvelope) -> String {
    let body = envelope.body.trim();
    if body.is_empty() {
        return original_text.to_string();
    }

    let rewritten_body = command_body_for_history(body);
    if rewritten_body == body {
        return original_text.to_string();
    }

    let Some(body_start) = original_text.rfind(body) else {
        return original_text.to_string();
    };

    let mut text = original_text.to_string();
    text.replace_range(body_start..body_start + body.len(), &rewritten_body);
    text
}

fn previous_command_context(text: &str) -> String {
    format!("{PREVIOUS_COMMAND_CONTEXT_DIRECTIVE}\n\n{text}")
}

fn active_command_instructions(dispatch: ActiveCommandDispatch) -> Option<String> {
    match dispatch {
        ActiveCommandDispatch::PromptBacked => Some(CURRENT_TURN_PRIORITY_DIRECTIVE.to_string()),
        ActiveCommandDispatch::Translated => {
            // Translated commands have their output handled by the executor pipeline in lib.rs.
            // Packaged prompt text is applied only after executor JSON exists, via post_result_for_translated_command().
            // Do not apply packaged prompt here at the context layer.
            None
        }
    }
}

fn translated_command_body(command_name: &str) -> Option<&'static str> {
    TRANSLATED_COMMAND_BODIES
        .iter()
        .find_map(|(name, body)| (*name == normalize_command_name(command_name)).then_some(*body))
}

/// Returns the packaged command body for a translated command name.
pub fn get_packaged_command_body(command_name: &str) -> &'static str {
    translated_command_body(command_name).unwrap_or("")
}

fn command_body_for_history(body: &str) -> String {
    if is_skill_body(body) {
        rewrite_base_directory_line(body)
    } else {
        body.to_string()
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
