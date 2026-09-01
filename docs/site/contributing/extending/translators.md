# Extending: the translator surface

A `BackendTranslator` converts between the gateway's Anthropic-shaped
request/response types and a specific backend's own request/response
shape. See [ADR-0006](../adr/ADR-0006.md) for why this is composition
(embedding) rather than a from-scratch implementation per backend.

## The interface

```go
type BackendTranslator interface {
    TranslateRequest(ctx context.Context, in *dto.MessagesRequest, meta TranslateMeta) (*port.BackendRequest, error)
    TranslateResponseEvent(ev port.BackendEvent) ([]dto.SSEEvent, error)
    BuildUnaryResponse(events []port.BackendEvent) (*dto.MessagesResponse, error)
}
```

- `TranslateRequest` — the inbound direction. Takes the parsed and
  already-normalized Anthropic-shaped request (context management,
  Claude Code normalization, and model resolution have already run by
  this point — see the request-flow walkthrough in
  [Architecture](../architecture.md)) plus `TranslateMeta` (resolved
  model, reasoning effort, service tier), and produces the backend's own
  request shape.
- `TranslateResponseEvent` — the outbound streaming direction. Takes one
  backend event and produces zero or more Anthropic SSE events. Called
  once per backend event by the single writer goroutine
  ([ADR-0004](../adr/ADR-0004.md)) — this method must not block on
  anything beyond pure translation work, since it sits directly on the
  streaming hot path.
- `BuildUnaryResponse` — the outbound non-streaming direction. Takes the
  full set of backend events for a completed turn and produces one
  Anthropic Messages response.

## What `GenericBackendTranslator` already gives you

Embed `*translator.GenericBackendTranslator` and you get, for free:

- System-prompt-to-instructions assembly.
- Common message shaping (role mapping, content-block normalization)
  shared across backends.
- Tool-schema gating via `ToolArgPolicy` (`tool_arg_policy.go`) —
  `ApplyPolicies` and `SanitizedToolArgsForKind` handle which tool
  arguments are safe to forward for a given conversation kind.
- Output-config mapping (max tokens, stop behavior, and comparable
  fields) into the backend request shape.
- Response-gate sanitization hooks (`claude_response_gate.go`) —
  `StructuredOutputSchemaFromConfig`, `SanitizeResponseValue`,
  `SanitizeResponseText` — for cleaning up structured-output text before
  it reaches the client.

## What to override

Override only the methods that are genuinely backend-specific — typically
the parts of `TranslateRequest` that shape the backend's own wire format
(field names, nesting, backend-specific request options) and the event
mapping in `TranslateResponseEvent` if the backend's event stream shape
differs from what `GenericBackendTranslator` already assumes. If you find
yourself overriding most of the surface, that's a signal either the new
backend is unusually different from the existing ones, or that some logic
you're duplicating actually belongs promoted into
`GenericBackendTranslator` for everyone to share — prefer the latter when
it's genuinely shared behavior.

## The compile-time assertion convention

Every concrete translator ends its file with:

```go
var _ translator.BackendTranslator = (*OpenAITranslator)(nil)
```

This isn't decorative — it's what catches an interface signature drift
(an added method, a changed parameter) at compile time on the concrete
type, instead of at whatever point in `app.Initialize` first assigns the
concrete type to the interface-typed field. Add this line to any new
translator before writing its methods, not after — it should fail to
compile until the interface is actually satisfied.
