// Package codex implements the codex backend adapter (core/domain/port/backend).
package codex

import "sort"

// ApplyOpenAIStrictSchemaGate mutates body in place, normalizing the
// structured-output text.format.schema and every tool parameter schema to
// satisfy the OpenAI Responses API's strict json_schema mode.
func ApplyOpenAIStrictSchemaGate(body map[string]any) {
	normalizeTextFormatSchema(body)
	normalizeToolParameterSchemas(body)
}

// NormalizeOpenAIStrictResponseSchema returns a normalized copy of schema.
func NormalizeOpenAIStrictResponseSchema(schema map[string]any) map[string]any {
	out := deepCopyMap(schema)
	normalizeOpenAIStrictSchemaValue(out)
	return out
}

func normalizeTextFormatSchema(body map[string]any) {
	text, ok := body["text"].(map[string]any)
	if !ok {
		return
	}
	format, ok := text["format"].(map[string]any)
	if !ok {
		return
	}
	schema, ok := format["schema"].(map[string]any)
	if !ok {
		return
	}
	normalizeOpenAIStrictSchemaValue(schema)
}

func normalizeToolParameterSchemas(body map[string]any) {
	tools, ok := body["tools"].([]any)
	if !ok {
		return
	}
	for _, t := range tools {
		tool, ok := t.(map[string]any)
		if !ok {
			continue
		}
		if parameters, ok := tool["parameters"].(map[string]any); ok {
			normalizeOpenAIStrictSchemaValue(parameters)
		}
		if function, ok := tool["function"].(map[string]any); ok {
			if parameters, ok := function["parameters"].(map[string]any); ok {
				normalizeOpenAIStrictSchemaValue(parameters)
			}
		}
	}
}

func normalizeOpenAIStrictSchemaValue(obj map[string]any) {
	if isObjectSchema(obj) {
		normalizeOpenAIStrictObjectSchema(obj)
	}

	for _, key := range []string{"items", "additionalProperties", "contains", "not", "if", "then", "else"} {
		if value, ok := obj[key].(map[string]any); ok {
			normalizeOpenAIStrictSchemaValue(value)
		}
	}

	for _, key := range []string{"anyOf", "oneOf", "allOf"} {
		if values, ok := obj[key].([]any); ok {
			for _, v := range values {
				if m, ok := v.(map[string]any); ok {
					normalizeOpenAIStrictSchemaValue(m)
				}
			}
		}
	}

	for _, key := range []string{"$defs", "definitions"} {
		if defs, ok := obj[key].(map[string]any); ok {
			for _, v := range defs {
				if m, ok := v.(map[string]any); ok {
					normalizeOpenAIStrictSchemaValue(m)
				}
			}
		}
	}
}

func normalizeOpenAIStrictObjectSchema(obj map[string]any) {
	obj["additionalProperties"] = false

	originalRequired := map[string]bool{}
	if required, ok := obj["required"].([]any); ok {
		for _, r := range required {
			if s, ok := r.(string); ok {
				originalRequired[s] = true
			}
		}
	}

	if _, exists := obj["properties"]; !exists {
		obj["properties"] = map[string]any{}
	}
	properties, ok := obj["properties"].(map[string]any)
	if !ok {
		return
	}

	propertyNames := make([]string, 0, len(properties))
	for name := range properties {
		propertyNames = append(propertyNames, name)
	}
	sort.Strings(propertyNames)

	for _, name := range propertyNames {
		property, ok := properties[name].(map[string]any)
		if !ok {
			continue
		}
		normalizeOpenAIStrictSchemaValue(property)
		if !originalRequired[name] {
			makeSchemaNullable(properties, name, property)
		}
	}

	required := make([]any, len(propertyNames))
	for i, name := range propertyNames {
		required[i] = name
	}
	obj["required"] = required
}

func isObjectSchema(obj map[string]any) bool {
	if _, ok := obj["properties"]; ok {
		return true
	}
	ty, ok := obj["type"].(string)
	return ok && ty == "object"
}

func makeSchemaNullable(properties map[string]any, name string, schema map[string]any) {
	typeValue, hasType := schema["type"]
	if !hasType {
		properties[name] = map[string]any{
			"anyOf": []any{
				schema,
				map[string]any{"type": "null"},
			},
		}
		return
	}

	switch ty := typeValue.(type) {
	case string:
		if ty != "null" {
			schema["type"] = []any{ty, "null"}
		}
	case []any:
		for _, v := range ty {
			if s, ok := v.(string); ok && s == "null" {
				return
			}
		}
		schema["type"] = append(ty, "null")
	default:
		// type present but neither string nor array: leave unchanged,
		// mirrors the Rust Some(_) => {} branch (schema_gate.rs:167).
	}
}

func deepCopyMap(in map[string]any) map[string]any {
	out := make(map[string]any, len(in))
	for k, v := range in {
		out[k] = deepCopyValue(v)
	}
	return out
}

func deepCopyValue(v any) any {
	switch value := v.(type) {
	case map[string]any:
		return deepCopyMap(value)
	case []any:
		out := make([]any, len(value))
		for i, e := range value {
			out[i] = deepCopyValue(e)
		}
		return out
	default:
		return value
	}
}
