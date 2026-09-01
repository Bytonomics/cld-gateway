package translator

import (
	"encoding/json"
	"sort"
	"strings"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// StructuredOutputSchemaFromConfig extracts the JSON schema from an
// Anthropic output_config when its format is {"type":"json_schema",
// "schema": ...}. Port of claude_response_gate.rs
// structured_output_schema_from_config.
func StructuredOutputSchemaFromConfig(outputConfig *dto.OutputConfig) any {
	if outputConfig == nil {
		return nil
	}
	format, ok := outputConfig.Format.(map[string]any)
	if !ok {
		return nil
	}
	if formatType, _ := format["type"].(string); formatType != "json_schema" {
		return nil
	}
	schema, ok := format["schema"]
	if !ok {
		return nil
	}
	return schema
}

// CleanupStructuredOutputTextForAnthropic drops null-valued optional fields
// (per outputConfig's JSON schema) from a structured-output JSON text
// payload before it reaches the Anthropic-shaped response. Port of
// claude_response_gate.rs cleanup_structured_output_text_for_anthropic.
func CleanupStructuredOutputTextForAnthropic(outputConfig *dto.OutputConfig, text string) string {
	schema := StructuredOutputSchemaFromConfig(outputConfig)
	return CleanupStructuredOutputTextWithSchema(schema, text)
}

// CleanupStructuredOutputTextWithSchema is the schema-parameterized core of
// CleanupStructuredOutputTextForAnthropic. Port of claude_response_gate.rs
// cleanup_structured_output_text_with_schema.
func CleanupStructuredOutputTextWithSchema(schema any, text string) string {
	if schema == nil {
		return text
	}

	var value any
	if err := json.Unmarshal([]byte(text), &value); err != nil {
		return text
	}

	value = removeNullOptionalFields(value, schema)
	encoded, err := json.Marshal(value)
	if err != nil {
		return text
	}
	return string(encoded)
}

// SanitizeResponseValue removes null-valued fields and empty text content
// blocks from a decoded Anthropic response value. Port of
// claude_response_gate.rs sanitize_anthropic_response_value.
func SanitizeResponseValue(value any) any {
	return removeNullFieldsAndEmptyTextBlocks(value)
}

// SanitizeResponseText is the string-in/string-out wrapper around
// SanitizeResponseValue. Port of claude_response_gate.rs
// sanitize_anthropic_response_text.
func SanitizeResponseText(text string) string {
	var value any
	if err := json.Unmarshal([]byte(text), &value); err != nil {
		return text
	}
	encoded, err := json.Marshal(SanitizeResponseValue(value))
	if err != nil {
		return text
	}
	return string(encoded)
}

func removeNullFieldsAndEmptyTextBlocks(value any) any {
	switch v := value.(type) {
	case map[string]any:
		out := make(map[string]any, len(v))
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			nested := removeNullFieldsAndEmptyTextBlocks(v[k])
			if nested == nil {
				continue
			}
			out[k] = nested
		}
		if content, ok := out["content"]; ok {
			if blocks, ok := content.([]any); ok {
				out["content"] = filterEmptyTextBlocks(blocks)
			}
		}
		return out
	case []any:
		out := make([]any, len(v))
		for i, nested := range v {
			out[i] = removeNullFieldsAndEmptyTextBlocks(nested)
		}
		return out
	default:
		return v
	}
}

func filterEmptyTextBlocks(blocks []any) []any {
	out := make([]any, 0, len(blocks))
	for _, b := range blocks {
		block, ok := b.(map[string]any)
		if !ok {
			out = append(out, b)
			continue
		}
		blockType, _ := block["type"].(string)
		if blockType != "text" {
			out = append(out, b)
			continue
		}
		text, hasText := block["text"].(string)
		if hasText && strings.TrimSpace(text) != "" {
			out = append(out, b)
		}
	}
	return out
}

func removeNullOptionalFields(value any, schema any) any {
	if valueObj, ok := value.(map[string]any); ok {
		if schemaObj, ok := schema.(map[string]any); ok {
			required := map[string]bool{}
			if requiredList, ok := schemaObj["required"].([]any); ok {
				for _, r := range requiredList {
					if s, ok := r.(string); ok {
						required[s] = true
					}
				}
			}

			if properties, ok := schemaObj["properties"].(map[string]any); ok {
				for propName, propSchema := range properties {
					propValue, exists := valueObj[propName]
					if !exists {
						continue
					}
					if propValue == nil && !required[propName] {
						delete(valueObj, propName)
						continue
					}
					valueObj[propName] = removeNullOptionalFields(propValue, propSchema)
				}
			}
			return valueObj
		}
		return valueObj
	}

	if valueArr, ok := value.([]any); ok {
		if schemaObj, ok := schema.(map[string]any); ok {
			if itemsSchema, ok := schemaObj["items"]; ok {
				out := make([]any, len(valueArr))
				for i, item := range valueArr {
					out[i] = removeNullOptionalFields(item, itemsSchema)
				}
				return out
			}
		}
		return valueArr
	}

	return value
}
