package observability

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// separator ends every formatted entry: exactly 36 dashes.
const separator = "------------------------------------"

// FormatEntry renders one Entry as `key: value` lines, one field per line
// in a stable order, followed by the separator line. This is the gateway's
// own log format (see ARCHITECTURE_v2.md "Log format"), not a port of
// Rust's format_exchange_record.
func FormatEntry(e Entry) []byte {
	var b strings.Builder
	writeLine(&b, "request_id", e.RequestID.String())
	writeLine(&b, "started_at_unix_ms", strconv.FormatInt(e.StartedAtUnixMs, 10))
	writeLine(&b, "duration_ms", strconv.FormatInt(e.DurationMs, 10))
	if e.ModelResolution != nil {
		writeLine(&b, "model_resolution.requested", e.ModelResolution.Requested)
		writeLine(&b, "model_resolution.selected_backend_model", e.ModelResolution.SelectedBackendModel)
		writeLine(&b, "model_resolution.selection_reason", e.ModelResolution.SelectionReason)
	}
	writeLine(&b, "request.method", e.Request.Method)
	writeLine(&b, "request.uri", e.Request.URI)
	writeHeaders(&b, "request.headers", e.Request.Headers)
	writeBody(&b, "request.body", e.Request.Body)
	writeLine(&b, "response.status", strconv.Itoa(e.Response.Status))
	writeHeaders(&b, "response.headers", e.Response.Headers)
	writeBody(&b, "response.body", e.Response.Body)
	b.WriteString(separator)
	b.WriteString("\n")
	return []byte(b.String())
}

func writeLine(b *strings.Builder, key, value string) {
	b.WriteString(key)
	b.WriteString(": ")
	b.WriteString(value)
	b.WriteString("\n")
}

func writeHeaders(b *strings.Builder, prefix string, headers map[string]string) {
	names := make([]string, 0, len(headers))
	for name := range headers {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		writeLine(b, prefix+"."+name, headers[name])
	}
}

func writeBody(b *strings.Builder, prefix string, body CapturedBody) {
	writeLine(b, prefix+".content_type", body.ContentType)
	writeLine(b, prefix+".bytes_captured", strconv.Itoa(body.BytesCaptured))
	writeLine(b, prefix+".truncated", strconv.FormatBool(body.Truncated))
	switch body.Kind {
	case BodyJSON:
		encoded, err := json.Marshal(body.JSON)
		if err != nil {
			encoded = []byte("null")
		}
		writeLine(b, prefix+".value", string(encoded))
	case BodyText:
		writeLine(b, prefix+".value", body.Text)
	case BodyBinary:
		writeLine(b, prefix+".value", body.Note)
	default:
		writeLine(b, prefix+".value", "")
	}
}

// Rotation/retention/breaker tuning (✱G9, ✱G7). Owner decision: rotate the
// formatted-text log by size and delete old rotated files to keep the disk
// clean; degrade (stop attempting writes) rather than let a broken disk
// cascade into blocking request handling.
const (
	maxLogSizeBytes         = 20 * 1024 * 1024 // rotate once the active file reaches this size
	maxRetainedRotatedLogs  = 5                // keep only this many rotated files, oldest deleted first
	breakerFailureThreshold = 5                // consecutive write failures before opening the breaker
	breakerCooldown         = 30 * time.Second // how long the breaker stays open before retrying
)

// FileExchangeLog appends formatted entries to a file, creating its parent
// directory as needed. It rotates the active file by size, deletes old
// rotated files beyond the retention count (✱G9), and trips a simple
// circuit breaker after consecutive write failures so a broken disk cannot
// cascade into blocking request handling (✱G7).
type FileExchangeLog struct {
	mu   sync.Mutex
	path string

	consecutiveFailures int
	breakerOpenUntil    time.Time
}

// NewFileExchangeLog returns a FileExchangeLog that appends to path.
func NewFileExchangeLog(path string) *FileExchangeLog {
	return &FileExchangeLog{path: path}
}

var _ ExchangeLog = (*FileExchangeLog)(nil)

// Append formats entry and appends it to the log file, rotating/pruning as
// needed. While the breaker is open, Append is a no-op that returns an
// error immediately, without touching disk.
func (l *FileExchangeLog) Append(entry Entry) error {
	l.mu.Lock()
	defer l.mu.Unlock()

	if !l.breakerOpenUntil.IsZero() {
		if time.Now().Before(l.breakerOpenUntil) {
			return fmt.Errorf("exchange log writes suspended: circuit breaker open until %s", l.breakerOpenUntil.Format(time.RFC3339))
		}
		l.breakerOpenUntil = time.Time{}
	}

	if err := l.appendLocked(entry); err != nil {
		l.recordFailureLocked(err)
		return err
	}
	l.recordSuccessLocked()
	return nil
}

func (l *FileExchangeLog) appendLocked(entry Entry) error {
	if dir := filepath.Dir(l.path); dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return fmt.Errorf("create exchange log dir: %w", err)
		}
	}

	if err := rotateIfOversized(l.path, maxLogSizeBytes, maxRetainedRotatedLogs); err != nil {
		slog.Warn("exchange log rotation failed", "path", l.path, "error", err)
	}

	f, err := os.OpenFile(l.path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("open exchange log: %w", err)
	}
	defer func() { _ = f.Close() }()

	if _, err := f.Write(FormatEntry(entry)); err != nil {
		return fmt.Errorf("write exchange log entry: %w", err)
	}
	return nil
}

func (l *FileExchangeLog) recordFailureLocked(err error) {
	l.consecutiveFailures++
	slog.Error("exchange log write failed", "path", l.path, "consecutive_failures", l.consecutiveFailures, "error", err)

	if l.consecutiveFailures >= breakerFailureThreshold && l.breakerOpenUntil.IsZero() {
		l.breakerOpenUntil = time.Now().Add(breakerCooldown)
		slog.Error("exchange logging degraded: circuit breaker open", "path", l.path, "cooldown", breakerCooldown, "resumes_at", l.breakerOpenUntil.Format(time.RFC3339))
	}
}

func (l *FileExchangeLog) recordSuccessLocked() {
	if l.consecutiveFailures > 0 {
		slog.Info("exchange logging recovered", "path", l.path)
	}
	l.consecutiveFailures = 0
	l.breakerOpenUntil = time.Time{}
}

// rotateIfOversized renames path to a timestamped sibling once it reaches
// maxSize, then deletes the oldest rotated siblings beyond retain.
func rotateIfOversized(path string, maxSize int64, retain int) error {
	info, err := os.Stat(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("stat exchange log: %w", err)
	}
	if info.Size() < maxSize {
		return nil
	}

	rotatedPath := fmt.Sprintf("%s.%s", path, time.Now().UTC().Format("20060102T150405.000000000Z"))
	if err := os.Rename(path, rotatedPath); err != nil {
		return fmt.Errorf("rotate exchange log: %w", err)
	}

	return pruneRotatedLogs(path, retain)
}

// pruneRotatedLogs deletes rotated siblings of path (path + "." + suffix)
// beyond the newest `retain` of them.
func pruneRotatedLogs(path string, retain int) error {
	dir := filepath.Dir(path)
	base := filepath.Base(path)

	entries, err := os.ReadDir(dir)
	if err != nil {
		return fmt.Errorf("list exchange log dir: %w", err)
	}

	prefix := base + "."
	var rotated []string
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if strings.HasPrefix(name, prefix) {
			rotated = append(rotated, name)
		}
	}
	if len(rotated) <= retain {
		return nil
	}

	// Rotated filenames carry a lexically sortable UTC timestamp suffix, so
	// a plain string sort orders oldest first.
	sort.Strings(rotated)

	toDelete := rotated[:len(rotated)-retain]
	var firstErr error
	for _, name := range toDelete {
		if err := os.Remove(filepath.Join(dir, name)); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("delete rotated exchange log %s: %w", name, err)
		}
	}
	return firstErr
}
