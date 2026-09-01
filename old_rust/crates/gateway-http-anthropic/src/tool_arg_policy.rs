#![forbid(unsafe_code)]

use serde_json::Map;
use serde_json::Value;

use gateway_backend_codex::tool_calls::normalize_json_object_string;
use gateway_backend_codex::types::CodexToolCallKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyEdit {
    pub(crate) field: &'static str,
    pub(crate) action: &'static str,
    pub(crate) reason: &'static str,
}

pub(crate) struct ToolArgContext<'a> {
    pub(crate) tool_name: &'a str,
}

pub(crate) fn apply_policies(
    ctx: &ToolArgContext<'_>,
    args: &mut Map<String, Value>,
) -> Vec<PolicyEdit> {
    let mut edits = Vec::new();
    edits.extend(agent_policy(ctx, args));
    edits.extend(read_policy(ctx, args));
    edits
}

pub(crate) fn sanitized_tool_args_for_kind(
    tool_name: &str,
    kind: CodexToolCallKind,
    buf: &str,
) -> Result<(Map<String, Value>, Vec<PolicyEdit>), String> {
    let mut args = parse_tool_args_object_for_kind(kind, buf)?;
    let ctx = ToolArgContext { tool_name };
    let edits = apply_policies(&ctx, &mut args);
    Ok((args, edits))
}

fn validate_tool_args_json_object(buf: &str) -> Result<(), String> {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| format!("tool_use.input is not valid JSON: {error}"))?;
    if !value.is_object() {
        return Err("tool_use.input must be a JSON object".to_string());
    }
    Ok(())
}

fn parse_tool_args_object(buf: &str) -> Result<Map<String, Value>, String> {
    let trimmed = buf.trim();
    validate_tool_args_json_object(trimmed)?;
    if trimmed.is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| format!("tool_use.input is not valid JSON: {error}"))?;
    let obj = value
        .as_object()
        .cloned()
        .ok_or_else(|| "tool_use.input must be a JSON object".to_string())?;
    Ok(obj)
}

fn parse_tool_args_object_for_kind(
    kind: CodexToolCallKind,
    buf: &str,
) -> Result<Map<String, Value>, String> {
    if kind != CodexToolCallKind::Custom {
        return parse_tool_args_object(buf);
    }

    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(Map::new());
    }
    if let Ok(obj) = parse_tool_args_object(trimmed) {
        return Ok(obj);
    }

    let normalized = normalize_json_object_string(trimmed, "input");
    parse_tool_args_object(&normalized)
}

fn agent_policy(ctx: &ToolArgContext<'_>, args: &mut Map<String, Value>) -> Vec<PolicyEdit> {
    if ctx.tool_name != "Agent" || !args.contains_key("isolation") {
        return Vec::new();
    }

    args.remove("isolation");
    vec![PolicyEdit {
        field: "isolation",
        action: "remove",
        reason: "gateway should not force worktree isolation for Agent calls",
    }]
}

fn read_policy(ctx: &ToolArgContext<'_>, args: &mut Map<String, Value>) -> Vec<PolicyEdit> {
    if ctx.tool_name != "Read" {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let pages = args.get("pages");
    if pages.is_none() {
        return edits;
    }

    let file_path = args.get("file_path").and_then(Value::as_str);
    let is_pdf = file_path.is_some_and(|p| p.to_ascii_lowercase().ends_with(".pdf"));

    let pages_empty = pages.and_then(Value::as_str).is_some_and(str::is_empty);

    if pages_empty {
        args.remove("pages");
        edits.push(PolicyEdit {
            field: "pages",
            action: "remove",
            reason: "empty string is invalid; omit pages unless reading a PDF",
        });
        return edits;
    }

    if !is_pdf {
        args.remove("pages");
        edits.push(PolicyEdit {
            field: "pages",
            action: "remove",
            reason: "pages only applies to PDF reads",
        });
    }

    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tool_name: &str) -> ToolArgContext<'_> {
        ToolArgContext { tool_name }
    }

    #[test]
    fn read_drops_empty_pages() {
        let mut args = Map::from_iter([
            (
                "file_path".to_string(),
                Value::String("/tmp/a.txt".to_string()),
            ),
            ("pages".to_string(), Value::String(String::new())),
        ]);
        let edits = apply_policies(&ctx("Read"), &mut args);
        assert!(args.get("pages").is_none());
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn read_drops_pages_for_non_pdf() {
        let mut args = Map::from_iter([
            (
                "file_path".to_string(),
                Value::String("/tmp/a.txt".to_string()),
            ),
            ("pages".to_string(), Value::String("1-2".to_string())),
        ]);
        let edits = apply_policies(&ctx("Read"), &mut args);
        assert!(args.get("pages").is_none());
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn read_keeps_pages_for_pdf() {
        let mut args = Map::from_iter([
            (
                "file_path".to_string(),
                Value::String("/tmp/a.PDF".to_string()),
            ),
            ("pages".to_string(), Value::String("1-2".to_string())),
        ]);
        let edits = apply_policies(&ctx("Read"), &mut args);
        assert!(args.get("pages").is_some());
        assert!(edits.is_empty());
    }

    #[test]
    fn agent_drops_isolation() {
        let mut args = Map::from_iter([
            (
                "description".to_string(),
                Value::String("Research files".to_string()),
            ),
            (
                "prompt".to_string(),
                Value::String("Inspect relevant files".to_string()),
            ),
            (
                "isolation".to_string(),
                Value::String("worktree".to_string()),
            ),
        ]);

        let edits = apply_policies(&ctx("Agent"), &mut args);

        assert!(args.get("isolation").is_none());
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].field, "isolation");
    }

    #[test]
    fn tool_args_validation_requires_object() {
        let err = validate_tool_args_json_object("[]").expect_err("should reject non-object");
        assert_eq!(err, "tool_use.input must be a JSON object");
    }

    #[test]
    fn parse_tool_args_object_accepts_empty_as_object() {
        let obj = parse_tool_args_object("").expect("empty ok");
        assert!(obj.is_empty());
    }

    #[test]
    fn custom_tool_args_wrap_raw_input() {
        let (args, edits) =
            sanitized_tool_args_for_kind("apply_patch", CodexToolCallKind::Custom, "raw patch")
                .expect("sanitize");
        assert_eq!(args.get("input").and_then(Value::as_str), Some("raw patch"));
        assert!(edits.is_empty());
    }
}
