package core

import (
	"fmt"
	"testing"
)

func TestNewRequestID(t *testing.T) {
	rid := NewRequestID()
	if len(rid) != 32 {
		t.Errorf("RequestID length = %d, want 32", len(rid))
	}
	if rid == "" {
		t.Error("RequestID is empty")
	}
}

func TestSecretString(t *testing.T) {
	s := NewSecret("sensitive-value")
	if s.String() != "[REDACTED]" {
		t.Errorf("Secret.String() = %s, want [REDACTED]", s.String())
	}
	if s.Expose() != "sensitive-value" {
		t.Errorf("Secret.Expose() = %s, want sensitive-value", s.Expose())
	}
}

func TestFormatErrorChain(t *testing.T) {
	inner := fmt.Errorf("inner")
	outer := fmt.Errorf("outer: %w", inner)
	got := FormatErrorChain(outer)
	// Expected: "outer: inner: caused by: inner"
	want := "outer: inner: caused by: inner"
	if got != want {
		t.Errorf("FormatErrorChain() = %q, want %q", got, want)
	}
}
