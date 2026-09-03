// Package services declares the use-case interfaces the handlers layer
// depends on. This file is the pinned contract for the /v1/messages
// orchestration (FILEMAP.md); core/impl/services/message_service.go
// implements it.
package services

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// MessageService orchestrates one POST /v1/messages exchange: the 9-step
// flow in ARCHITECTURE_v2.md ("Request flow (POST /v1/messages)").
type MessageService interface {
	Handle(ctx context.Context, req *dto.MessagesRequest) MessageResult
}

// MessageResult is a tagged union: a unary response, or a channel of SSE
// events for the caller's single SSE-writer goroutine (T67,
// core/impl/services/stream_writer.go) to drain. Exactly one of the two
// success shapes is populated:
//   - Unary != nil, Stream == nil, Err == nil: unary JSON response ready to
//     serialize as-is.
//   - Stream != nil, Err == nil: the request was streaming (dto.
//     MessagesRequest.Stream == true) and the backend call started
//     successfully; drain Stream.
//   - Unary == nil, Stream == nil, Err != nil: the request failed before
//     any backend call was made (validation, translation, lease-busy,
//     transport-selection, or backend-connect failure for the unary path).
//     The caller renders Err through the central AppError->Anthropic JSON
//     shape; no SSE headers have been written yet.
//
// Channel contract for Stream (binding on both sides - MessageService as
// producer, the writer goroutine as sole consumer):
//  1. MessageService is the only writer on Stream and MUST close(Stream)
//     exactly once, when the backend event stream ends for any reason
//     (normal completion, backend error translated to a terminal SSE error
//     event, or ctx.Done()/client disconnect). The writer goroutine only
//     ranges over Stream; it must never close it.
//  2. Every dto.SSEEvent sent on Stream is fully framed and ready to write
//     verbatim as "event: <Event>\ndata: <Data>\n\n" - all Anthropic SSE
//     translation (translator.BackendTranslator.TranslateResponseEvent)
//     happens inside MessageService before the send, in event order, with
//     no buffering beyond the channel's own capacity.
//  3. By the time Stream closes, MessageService has already completed (or,
//     per the lease-gated commit rules in ARCHITECTURE_v2.md, deliberately
//     skipped) every state commit for this turn: lease commit-gate
//     evaluation, ConversationRepo.CommitTurn /
//     CommitOffshootCheckpoint, and ToolCallRepo recording. The writer
//     goroutine must not read or mutate conversation/lease state itself;
//     its job is strictly to flush bytes per event and, at close, hand the
//     accumulated event list to Option-C exchange logging.
//  4. If the caller's ctx is cancelled before Stream closes (client
//     aborted), MessageService observes ctx.Done(), transitions the
//     turn's lease to ClientAbortedBeforeFirstEvent or
//     ClientAbortedAfterVisibleOutput (whichever applies given whether any
//     visible event was already sent), and still closes Stream so the
//     writer goroutine's range loop terminates - it never leaves the
//     writer goroutine blocked.
type MessageResult struct {
	Unary                     *dto.MessagesResponse
	Stream                    <-chan dto.SSEEvent
	Err                       error
	ContextManagementMetadata map[string]any
	Warnings                  []dto.Warning
}
