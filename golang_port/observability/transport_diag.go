package observability

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// TransportDiagLog is a JSONL sink for transport decisions, kept separate
// from the formatted-text exchange log (see ARCHITECTURE_v2.md
// "observability/" section). Mirrors append_transport_diagnostic in
// crates/gateway-http-anthropic/src/lib.rs.
type TransportDiagLog struct {
	mu   sync.Mutex
	path string
}

// NewTransportDiagLog returns a TransportDiagLog that appends to path.
func NewTransportDiagLog(path string) *TransportDiagLog {
	return &TransportDiagLog{path: path}
}

// DefaultTransportDiagLogPath resolves the default transport diagnostics
// log path: GATEWAY_HOME/logs/transport-decisions.jsonl if GATEWAY_HOME is
// set, else ~/.gateway/logs/transport-decisions.jsonl. Mirrors
// transport_diagnostics_log_path in
// crates/gateway-http-anthropic/src/lib.rs.
func DefaultTransportDiagLogPath() string {
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "logs", "transport-decisions.jsonl")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".gateway", "logs", "transport-decisions.jsonl")
}

// Append appends record as one JSON line, stamping timestamp_unix_ms onto
// it when record marshals to a JSON object. Write failures are returned to
// the caller (transport decisions are best-effort diagnostics, not part of
// the request-serving path).
func (l *TransportDiagLog) Append(record any) error {
	l.mu.Lock()
	defer l.mu.Unlock()

	raw, err := json.Marshal(record)
	if err != nil {
		return fmt.Errorf("marshal transport diagnostic: %w", err)
	}

	var obj map[string]any
	if err := json.Unmarshal(raw, &obj); err == nil {
		obj["timestamp_unix_ms"] = time.Now().UnixMilli()
		stamped, err := json.Marshal(obj)
		if err == nil {
			raw = stamped
		}
	}

	if dir := filepath.Dir(l.path); dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return fmt.Errorf("create transport diagnostics log dir: %w", err)
		}
	}
	f, err := os.OpenFile(l.path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("open transport diagnostics log: %w", err)
	}
	defer func() { _ = f.Close() }()

	raw = append(raw, '\n')
	if _, err := f.Write(raw); err != nil {
		return fmt.Errorf("write transport diagnostic: %w", err)
	}
	return nil
}
