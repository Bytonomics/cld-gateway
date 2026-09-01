// Package toolcalls implements state.ToolCallRepo (core/domain/port/state)
// using GORM with the glebarez/sqlite pure-Go driver, matching
// crates/gateway-state/src/tool_calls.rs.
package toolcalls

import (
	"context"
	"errors"
	"os"
	"path/filepath"

	"github.com/glebarez/sqlite"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"

	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

// DefaultToolCallsDBPath ports default_tool_calls_db_path
// (crates/gateway-state/src/lib.rs): CLD_GATEWAY_STATE_DB_PATH (full path)
// wins; else GATEWAY_HOME/state/tool_calls.sqlite; else
// ~/.gateway/state/tool_calls.sqlite.
func DefaultToolCallsDBPath() string {
	if p := os.Getenv("CLD_GATEWAY_STATE_DB_PATH"); p != "" {
		return p
	}
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "state", "tool_calls.sqlite")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".gateway", "state", "tool_calls.sqlite")
}

type toolCallRow struct {
	CallID    string  `gorm:"column:call_id;primaryKey"`
	ToolName  string  `gorm:"column:tool_name;not null"`
	ToolKind  string  `gorm:"column:tool_kind;not null;default:function_call"`
	CreatedAt int64   `gorm:"column:created_at;not null"`
	RequestID *string `gorm:"column:request_id"`
}

func (toolCallRow) TableName() string { return "tool_calls" }

// Store implements state.ToolCallRepo via GORM + glebarez/sqlite.
type Store struct {
	db *gorm.DB
}

var _ state.ToolCallRepo = (*Store)(nil)

// Open opens (creating parent directories as needed) the SQLite database at
// dsn, ensures the schema, and returns a Store.
func Open(dsn string) (*Store, error) {
	if dir := filepath.Dir(dsn); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return nil, err
		}
	}
	db, err := gorm.Open(sqlite.Open(dsn), &gorm.Config{})
	if err != nil {
		return nil, err
	}
	store := &Store{db: db}
	if err := store.EnsureSchema(context.Background()); err != nil {
		return nil, err
	}
	return store, nil
}

// EnsureSchema creates the tool_calls table if missing and adds any
// missing columns (mirrors ensure_tool_kind_column's ALTER TABLE migration).
func (s *Store) EnsureSchema(ctx context.Context) error {
	return s.db.WithContext(ctx).AutoMigrate(&toolCallRow{})
}

// RecordToolCall upserts by call_id (mirrors INSERT OR REPLACE).
func (s *Store) RecordToolCall(ctx context.Context, call state.StoredToolCall) error {
	row := toolCallRow{
		CallID:    call.CallID,
		ToolName:  call.ToolName,
		ToolKind:  call.ToolKind,
		CreatedAt: call.CreatedAtUnixSeconds,
		RequestID: call.RequestID,
	}
	return s.db.WithContext(ctx).Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "call_id"}},
		UpdateAll: true,
	}).Create(&row).Error
}

func (s *Store) ToolCallExists(ctx context.Context, callID string) (bool, error) {
	var count int64
	err := s.db.WithContext(ctx).Model(&toolCallRow{}).
		Where("call_id = ?", callID).Limit(1).Count(&count).Error
	if err != nil {
		return false, err
	}
	return count > 0, nil
}

func (s *Store) GetToolCall(ctx context.Context, callID string) (*state.StoredToolCall, error) {
	var row toolCallRow
	err := s.db.WithContext(ctx).Where("call_id = ?", callID).First(&row).Error
	if err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return nil, nil
		}
		return nil, err
	}
	return &state.StoredToolCall{
		CallID:               row.CallID,
		ToolName:             row.ToolName,
		ToolKind:             row.ToolKind,
		RequestID:            row.RequestID,
		CreatedAtUnixSeconds: row.CreatedAt,
	}, nil
}
