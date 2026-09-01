package conversation

import (
	"context"
	"math"
	"reflect"
	"testing"
)

// TestCleanupSessionsOlderThanOverflowGuard is a regression test for the
// integer-overflow bug in CleanupSessionsOlderThan: multiplying an
// attacker/config-controlled days value by 86400 could silently wrap
// int64 instead of panicking. Mirrors the checked_mul chain in
// crates/gateway-state/src/conversation.rs:361-368, which returns Ok(0)
// (clean up nothing) rather than overflowing.
func TestCleanupSessionsOlderThanOverflowGuard(t *testing.T) {
	store := NewWithRoot(t.TempDir(), CorruptionPolicyFailClosed, nil)

	tests := []struct {
		name string
		days int
	}{
		{name: "math.MaxInt64 days", days: math.MaxInt64},
		{name: "just above the safe bound", days: maxRetentionDays + 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			deleted, err := store.CleanupSessionsOlderThan(context.Background(), tt.days)
			if err != nil {
				t.Fatalf("CleanupSessionsOlderThan(%d) returned error: %v", tt.days, err)
			}
			if deleted != 0 {
				t.Errorf("CleanupSessionsOlderThan(%d) = %d, want 0", tt.days, deleted)
			}
		})
	}
}

// TestCleanupSessionsOlderThanNormalDaysUnaffected verifies the overflow
// guard does not change behavior for ordinary, small days values.
func TestCleanupSessionsOlderThanNormalDaysUnaffected(t *testing.T) {
	store := NewWithRoot(t.TempDir(), CorruptionPolicyFailClosed, nil)

	deleted, err := store.CleanupSessionsOlderThan(context.Background(), 30)
	if err != nil {
		t.Fatalf("CleanupSessionsOlderThan(30) returned error: %v", err)
	}
	if deleted != 0 {
		t.Errorf("CleanupSessionsOlderThan(30) on an empty store = %d, want 0", deleted)
	}
}

// TestJsonEqualNumericNormalization demonstrates that jsonEqual correctly
// treats float64 and int numeric types as equal when they represent the same
// JSON value, unlike reflect.DeepEqual which treats them as unequal.
// This is a regression test for the bug where reflect.DeepEqual was causing
// spurious "snapshot changed" detections.
func TestJsonEqualNumericNormalization(t *testing.T) {
	tests := []struct {
		name     string
		a        any
		b        any
		expected bool
	}{
		{
			name:     "float64 and int with same value are equal",
			a:        map[string]any{"count": 5.0},
			b:        map[string]any{"count": 5},
			expected: true,
		},
		{
			name:     "nested structure with float64 and int",
			a:        []any{map[string]any{"id": 1, "value": 42.0}},
			b:        []any{map[string]any{"id": 1, "value": 42}},
			expected: true,
		},
		{
			name: "complex structure from disk load simulation",
			a: []any{
				map[string]any{
					"role":    "user",
					"content": "hello",
					"id":      float64(1), // Loaded from JSON (json.Unmarshal always produces float64)
				},
			},
			b: []any{
				map[string]any{
					"role":    "user",
					"content": "hello",
					"id":      1, // Native Go int
				},
			},
			expected: true,
		},
		{
			name:     "different values are unequal",
			a:        map[string]any{"count": 5.0},
			b:        map[string]any{"count": 6},
			expected: false,
		},
		{
			name:     "different structure shapes are unequal",
			a:        map[string]any{"a": 1},
			b:        map[string]any{"b": 1},
			expected: false,
		},
		{
			name:     "identical strings are equal",
			a:        []any{"user", "system"},
			b:        []any{"user", "system"},
			expected: true,
		},
		{
			name:     "empty slices are equal",
			a:        []any{},
			b:        []any{},
			expected: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := jsonEqual(tt.a, tt.b)
			if result != tt.expected {
				t.Errorf("jsonEqual(%v, %v) = %v, want %v", tt.a, tt.b, result, tt.expected)
			}
		})
	}
}

// TestReflectDeepEqualBugDemonstration shows that reflect.DeepEqual
// incorrectly treats float64(5) and int(5) as unequal, even though they
// represent the same JSON value. This is the bug that jsonEqual fixes.
func TestReflectDeepEqualBugDemonstration(t *testing.T) {
	// This demonstrates the problem with reflect.DeepEqual
	floatVersion := map[string]any{"count": float64(5)}
	intVersion := map[string]any{"count": 5}

	// reflect.DeepEqual incorrectly reports them as unequal
	if reflect.DeepEqual(floatVersion, intVersion) {
		t.Error("reflect.DeepEqual unexpectedly treats float64(5) and int(5) as equal")
	}

	// jsonEqual correctly treats them as equal
	if !jsonEqual(floatVersion, intVersion) {
		t.Error("jsonEqual should treat float64(5) and int(5) as equal")
	}

	// Verify this is the actual bug: when branch.ActiveCanonicalMessages
	// is loaded from disk (contains float64), and params.Messages has native
	// Go ints, reflect.DeepEqual would incorrectly flag them as changed
	branchMessages := []any{
		map[string]any{
			"role":    "user",
			"content": "hello",
			"id":      float64(1), // From json.Unmarshal
		},
	}
	paramMessages := []any{
		map[string]any{
			"role":    "user",
			"content": "hello",
			"id":      1, // Native Go type
		},
	}

	// This is the bug: reflect.DeepEqual incorrectly reports a change
	snapshotChangedByReflect := !reflect.DeepEqual(branchMessages, paramMessages)
	if !snapshotChangedByReflect {
		t.Errorf("reflect.DeepEqual unexpectedly treats disk-loaded and native-Go structures as equal")
	}

	// Our fix: jsonEqual correctly treats them as unchanged
	snapshotChangedByJsonEqual := !jsonEqual(branchMessages, paramMessages)
	if snapshotChangedByJsonEqual {
		t.Error("jsonEqual should not report a change between disk-loaded and native-Go structures")
	}
}
