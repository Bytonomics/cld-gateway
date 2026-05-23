#![forbid(unsafe_code)]

use serde_json::Map;
use serde_json::Value;

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
    edits.extend(read_policy(ctx, args));
    edits
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
}
