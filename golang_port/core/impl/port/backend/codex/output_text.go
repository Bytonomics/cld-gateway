package codex

import (
	"encoding/json"
	"sort"
)

// ExtractTextFromData returns the last "text" or "delta" string found
// anywhere in data's JSON structure. If data is not valid JSON, data itself
// is returned as the text.
func ExtractTextFromData(data string) *string {
	var value any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		out := data
		return &out
	}

	var last *string
	extractLastTextFromValue(value, &last)
	return last
}

// ParseOutputItemMessageTexts extracts output_text strings from a
// response.output_item.done/added event payload.
func ParseOutputItemMessageTexts(eventName, data string) []string {
	if eventName != "response.output_item.done" && eventName != "response.output_item.added" {
		return nil
	}

	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}

	itemRaw, keyPresent := value["item"]

	if !keyPresent {
		// Top-level "item" key is absent, try response.item
		response, ok := value["response"].(map[string]any)
		if !ok {
			return nil
		}
		itemRaw, ok = response["item"]
		if !ok {
			return nil
		}
	}

	// At this point, itemRaw is from either top-level or response.item
	// Now attempt the type assertion
	item, ok := itemRaw.(map[string]any)
	if !ok {
		// Type assertion failed - terminal failure, no fallback
		return nil
	}

	return MessageItemOutputTexts(item)
}

// MessageItemOutputTexts extracts output_text strings from a single
// response output item.
func MessageItemOutputTexts(item map[string]any) []string {
	itemType, _ := item["type"].(string)
	switch itemType {
	case "message":
		return outputTextsFromContentArray(item)
	case "output_text":
		text, ok := item["text"].(string)
		if !ok || text == "" {
			return nil
		}
		return []string{text}
	default:
		return nil
	}
}

func outputTextsFromContentArray(item map[string]any) []string {
	content, ok := item["content"].([]any)
	if !ok {
		return nil
	}

	var out []string
	for _, c := range content {
		contentItem, ok := c.(map[string]any)
		if !ok {
			continue
		}
		if ty, _ := contentItem["type"].(string); ty != "output_text" {
			continue
		}
		text, ok := contentItem["text"].(string)
		if !ok || text == "" {
			continue
		}
		out = append(out, text)
	}
	return out
}

func extractLastTextFromValue(value any, last **string) {
	switch v := value.(type) {
	case map[string]any:
		if text, ok := v["text"].(string); ok {
			t := text
			*last = &t
		}
		if delta, ok := v["delta"].(string); ok {
			d := delta
			*last = &d
		}
		if content, ok := v["content"]; ok {
			extractLastTextFromValue(content, last)
		}
		// Collect remaining keys (excluding text, delta, content) and sort them
		// for deterministic iteration order, matching the pattern from schema_gate.go.
		remainingKeys := make([]string, 0, len(v))
		for key := range v {
			if key == "text" || key == "delta" || key == "content" {
				continue
			}
			remainingKeys = append(remainingKeys, key)
		}
		sort.Strings(remainingKeys)
		for _, key := range remainingKeys {
			extractLastTextFromValue(v[key], last)
		}
	case []any:
		for _, child := range v {
			extractLastTextFromValue(child, last)
		}
	}
}
