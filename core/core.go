package core

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
)

type RequestID string

func NewRequestID() RequestID {
	b := make([]byte, 16)
	_, _ = rand.Read(b)
	return RequestID(hex.EncodeToString(b))
}

func (r RequestID) String() string { return string(r) }

// Secret prevents accidental printing of sensitive values.
type Secret string

func NewSecret(v string) Secret { return Secret(v) }
func (s Secret) Expose() string { return string(s) }
func (s Secret) String() string { return "[REDACTED]" } // never leak

// Unwrap is a small helper using errors.Unwrap
func Unwrap(err error) error {
	return errors.Unwrap(err)
}

// FormatErrorChain mirrors Rust format_error_chain: message + ": caused by: " per wrapped err.
func FormatErrorChain(err error) string {
	if err == nil {
		return ""
	}
	msg := err.Error()
	for e := Unwrap(err); e != nil; e = Unwrap(e) {
		if e.Error() != "" {
			msg += ": caused by: " + e.Error()
		}
	}
	return msg
}
