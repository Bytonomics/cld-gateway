use crate::types::{AnthropicContent, AnthropicMessage};
use std::{collections::HashMap, sync::OnceLock};

const COMMAND_MESSAGE_TAG: &str = "command-message";
const COMMAND_NAME_TAG: &str = "command-name";
const COMMAND_ARGS_TAG: &str = "command-args";
const LOCAL_COMMAND_STDOUT_TAG: &str = "local-command-stdout";
const READ_ONLY_MARKERS: &[&str] = &[
    "This is a side question from the user",
    "separate, lightweight agent",
    "The main agent is NOT interrupted",
];
const INTERNAL_COMMANDS: &[InternalCommandSpec] = &[
    InternalCommandSpec {
        name: "add-dir",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "agents",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "branch",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[
            "Branched conversation",
            "You are now in the branch",
            "claude -r ",
        ],
    },
    InternalCommandSpec {
        name: "color",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "config",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "copy",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "effort",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "export",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "hooks",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "ide",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "login",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "logout",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "mcp",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "mobile",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "model",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "permissions",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "plugin",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "rename",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &["Session renamed to:"],
    },
    InternalCommandSpec {
        name: "resume",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "sandbox",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "skills",
        classification: InternalCommandClassification::LocalOnly,
        stdout_markers: &[],
    },
    InternalCommandSpec {
        name: "status",
        classification: InternalCommandClassification::Translate,
        stdout_markers: &[],
    },
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ConversationInclusion {
    #[default]
    ReadWrite,
    ReadOnly,
    LocalOnly,
}

impl ConversationInclusion {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read_write",
            Self::ReadOnly => "read_only",
            Self::LocalOnly => "local_only",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConversationInclusionReport {
    turn_inclusion: ConversationInclusion,
    local_only_commands: Vec<String>,
}

impl ConversationInclusionReport {
    pub(crate) fn extend_client_metadata(&self, client_metadata: &mut HashMap<String, String>) {
        if self.turn_inclusion != ConversationInclusion::ReadWrite {
            client_metadata.insert(
                "gateway_conversation_inclusion".to_string(),
                self.turn_inclusion.as_str().to_string(),
            );
        }
        if !self.local_only_commands.is_empty() {
            client_metadata.insert(
                "gateway_local_only_commands".to_string(),
                self.local_only_commands.join(","),
            );
        }
    }

    fn mark_read_only(&mut self) {
        if self.turn_inclusion == ConversationInclusion::ReadWrite {
            self.turn_inclusion = ConversationInclusion::ReadOnly;
        }
    }

    fn mark_local_only_command(&mut self, command_name: &str) {
        let command_name = format!("/{}", normalize_command_name(command_name));
        if !self.local_only_commands.contains(&command_name) {
            self.local_only_commands.push(command_name);
        }
    }

    fn mark_local_only_turn_if_empty(&mut self, messages: &[AnthropicMessage]) {
        if !self.local_only_commands.is_empty()
            && !messages
                .iter()
                .any(|message| message.role == "user" && !message_text(message).trim().is_empty())
        {
            self.turn_inclusion = ConversationInclusion::LocalOnly;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeCodeCommandClassification {
    PromptBacked,
    LocalOnly,
    Translate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalCommandClassification {
    LocalOnly,
    Translate,
}

#[derive(Debug)]
struct InternalCommandSpec {
    name: &'static str,
    classification: InternalCommandClassification,
    stdout_markers: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub(crate) struct CommandEnvelope {
    pub(crate) command_name: String,
    pub(crate) body: String,
}

pub(crate) fn apply_conversation_inclusion_policy(
    messages: &mut [AnthropicMessage],
) -> ConversationInclusionReport {
    let mut report = ConversationInclusionReport::default();

    for message in messages.iter_mut().filter(|message| message.role == "user") {
        match &mut message.content {
            AnthropicContent::Text(text) => {
                *text = apply_text_inclusion_policy(text, &mut report);
            }
            AnthropicContent::Blocks(blocks) => {
                for block in blocks.iter_mut().filter(|block| block.block_type == "text") {
                    if let Some(text) = &mut block.text {
                        *text = apply_text_inclusion_policy(text, &mut report);
                    }
                }
                blocks.retain(|block| {
                    block.block_type != "text"
                        || block
                            .text
                            .as_deref()
                            .is_some_and(|text| !text.trim().is_empty())
                });
            }
        }
    }

    report.mark_local_only_turn_if_empty(messages);
    report
}

pub(crate) fn parse_command_envelope(text: &str) -> Option<CommandEnvelope> {
    let command_name_match = last_tag_match(text, COMMAND_NAME_TAG)?;
    let command_name = command_name_match.value;
    let command_args_match = text
        .get(command_name_match.end..)
        .and_then(|suffix| first_tag_match(suffix, COMMAND_ARGS_TAG))
        .map(|tag| tag.with_offset(command_name_match.end));
    let body_start = command_args_match
        .as_ref()
        .map_or(command_name_match.end, |tag| tag.end);
    let body = text
        .get(body_start..)
        .unwrap_or_default()
        .trim()
        .to_string();

    Some(CommandEnvelope { command_name, body })
}

fn apply_text_inclusion_policy(text: &str, report: &mut ConversationInclusionReport) -> String {
    if is_read_only_request(text) {
        report.mark_read_only();
    }

    let mut included = text.to_string();
    while let Some(local_only_match) = local_only_command_span(&included) {
        report.mark_local_only_command(local_only_match.command_name);
        included.replace_range(local_only_match.start..local_only_match.end, "");
    }
    while let Some(local_only_match) = local_only_stdout_span(&included) {
        report.mark_local_only_command(local_only_match.command_name);
        included.replace_range(local_only_match.start..local_only_match.end, "");
    }

    included.trim().to_string()
}

fn is_read_only_request(text: &str) -> bool {
    READ_ONLY_MARKERS.iter().all(|marker| text.contains(marker))
}

#[derive(Debug, Clone, Copy)]
struct LocalOnlyMatch {
    command_name: &'static str,
    start: usize,
    end: usize,
}

fn local_only_command_span(text: &str) -> Option<LocalOnlyMatch> {
    let mut remaining = text;
    let mut offset = 0;

    while let Some(command_name_match) = first_tag_match(remaining, COMMAND_NAME_TAG) {
        let command_name_match = command_name_match.with_offset(offset);
        if let Some(spec) = local_only_command_spec(&command_name_match.value) {
            let command_start = text
                .get(..command_name_match.start)
                .and_then(|prefix| last_tag_match(prefix, COMMAND_MESSAGE_TAG))
                .map_or(command_name_match.start, |tag| tag.start);
            let command_end = text
                .get(command_name_match.end..)
                .and_then(|suffix| first_tag_match(suffix, COMMAND_ARGS_TAG))
                .map_or(command_name_match.end, |tag| {
                    tag.with_offset(command_name_match.end).end
                });
            return Some(LocalOnlyMatch {
                command_name: spec.name,
                start: command_start,
                end: command_end,
            });
        }

        offset = command_name_match.end;
        remaining = text.get(offset..).unwrap_or_default();
    }

    None
}

fn local_only_stdout_span(text: &str) -> Option<LocalOnlyMatch> {
    let mut remaining = text;
    let mut offset = 0;

    while let Some(stdout_match) = first_tag_match(remaining, LOCAL_COMMAND_STDOUT_TAG) {
        let stdout_match = stdout_match.with_offset(offset);
        if let Some(spec) = local_only_stdout_spec(&stdout_match.value) {
            return Some(LocalOnlyMatch {
                command_name: spec.name,
                start: stdout_match.start,
                end: stdout_match.end,
            });
        }

        offset = stdout_match.end;
        remaining = text.get(offset..).unwrap_or_default();
    }

    None
}

pub(crate) fn classify_claude_code_command(command_name: &str) -> ClaudeCodeCommandClassification {
    command_spec_by_name(normalize_command_name(command_name)).map_or(
        ClaudeCodeCommandClassification::PromptBacked,
        |spec| match spec.classification {
            InternalCommandClassification::LocalOnly => ClaudeCodeCommandClassification::LocalOnly,
            InternalCommandClassification::Translate => ClaudeCodeCommandClassification::Translate,
        },
    )
}

fn local_only_command_spec(command_name: &str) -> Option<&'static InternalCommandSpec> {
    command_spec_by_name(normalize_command_name(command_name))
        .filter(|spec| spec.classification == InternalCommandClassification::LocalOnly)
}

fn local_only_stdout_spec(stdout: &str) -> Option<&'static InternalCommandSpec> {
    // Look through all specs for one that:
    // 1. Is classified as LocalOnly
    // 2. Has stdout markers defined
    // 3. All markers are present in the stdout text
    INTERNAL_COMMANDS
        .iter()
        .filter(|spec| spec.classification == InternalCommandClassification::LocalOnly)
        .find(|spec| {
            !spec.stdout_markers.is_empty()
                && spec
                    .stdout_markers
                    .iter()
                    .all(|marker| stdout.contains(marker))
        })
}

fn command_spec_by_name(command_name: &str) -> Option<&'static InternalCommandSpec> {
    static COMMANDS_BY_NAME: OnceLock<HashMap<&'static str, &'static InternalCommandSpec>> =
        OnceLock::new();

    COMMANDS_BY_NAME
        .get_or_init(|| {
            INTERNAL_COMMANDS
                .iter()
                .map(|spec| (spec.name, spec))
                .collect()
        })
        .get(command_name)
        .copied()
}

fn normalize_command_name(command_name: &str) -> &str {
    command_name.trim().trim_start_matches('/')
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
