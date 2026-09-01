# DOCSITE: documentation plan for the Go gateway

Modeled on pedantigo.dev (Docusaurus, versioned docs, Getting Started →
Concepts → Reference arc). Two audiences, strictly separated:

1. USERS — install, configure, run, troubleshoot. Zero internals.
2. CONTRIBUTORS — architecture, design decisions, extending the service.

Future home: a Docusaurus site (e.g. cldgateway.dev or docs section of the
repo site). Until the site exists, these files live in-repo under
`golang_port/docs/site/` with the exact paths below, so the site build is a
pure lift-and-shift.

---

## Section 1 — User documentation (the pedantigo.dev-style usability focus)

### `site/index.md` — Landing page
- What the gateway is (one paragraph, no internals): local proxy that lets
  Claude Code-style clients use a Codex/ChatGPT backend.
- Three capability cards (mirroring pedantigo's three): "Drop-in Anthropic
  API", "Conversation state that survives restarts", "Configurable
  translation and transport".
- Links: Get Started, Configuration, Go Reference.

### `site/docs/getting-started/installation.md`
- Homebrew install (tap + formula), what gets installed (cld-gateway,
  cld-gateway-sh, cldg, clddg, config.yml, settings.json, commands).
- First run: `cld-gateway login` → `serve`.
- The two run modes (dev 6483 / packaged 6473) and which config file each
  reads.

### `site/docs/getting-started/quickstart.md`
- Login → serve → point the client at the gateway → first streamed turn.
- Verify with /health and one exchange-log entry.

### `site/docs/getting-started/setup-command.md`
- `cld-gateway-sh setup` — including the NEW config-modification options
  (choose active backend etc.).

### `site/docs/configuration/index.md`
- Every config key, its default, env overrides
  (GATEWAY_CONFIG_PATH / GATEWAY_HOME).
- The providers map (backend name → active flag, default_model,
  unsupported_models).
- Workflow keys: fast_mode, context_management (edits, hard limits),
  claude_code.slash_commands, conversation_state (persistence root,
  corruption policy, retention).
- Network: listen_addr, allowed_hosts; the Anthropic-host block rule.

### `site/docs/configuration/backends.md`
- The 1-to-n model: one active backend, how to switch, what a backend
  entry looks like. Per-backend knobs (timeout — G4; WS keepalive if G10
  approved).

### `site/docs/configuration/logs-and-state.md`
- Where everything lives: auth.json, tool_calls.sqlite, sessions/
  claudecode, logs/.
- Exchange-log format: the key: value + dashed-separator entries.
- Rotation and retention (G9): sizes, how many kept, old-file deletion.
- Conversation-state retention (max_session_age_days).

### `site/docs/usage/commands.md`
- CLI reference: serve, login [openai|gemini], TUI login flow.
- Exit behaviors and what serve preflight does when auth is stale.

### `site/docs/usage/api.md`
- The six HTTP routes, request/response shapes, SSE event sequence the
  client sees, Anthropic error shape with all gateway error codes.

### `site/docs/usage/troubleshooting.md`
- Symptom → log → fix table. Start at the exchange log; correlate by
  request-id; transport-decisions.jsonl for full-vs-incremental questions;
  auth refresh failures; corruption policy behavior (fail-closed vs
  quarantine).

### `site/docs/usage/security.md`
- Localhost-only binding, outbound allowlist, Anthropic-host denylist,
  what is stored on disk (tokens in auth.json) and what is not (keyring
  dropped).

---

## Section 2 — Contributor documentation (architecture and design)

### `site/contributing/architecture.md`
- Distro of ARCHITECTURE_v2.md: layers, dependency rule, Providers DI,
  request-flow walkthrough (9 steps), SSE single-writer model, Option C
  logging.

### `site/contributing/design-decisions.md`
- The interview record, distilled: 1-to-n scope, port shapes,
  extend-via-composition translator, state-stays-core, lease machine
  ported verbatim, library picks with reasons (echo/pedantigoecho,
  pedantigo, coder/websocket, GORM+glebarez, viper, bubbletea, slog).

### `site/contributing/adr/index.md` + numbered ADRs
- ADR-0001 1-to-n scope (Claude Code only inbound)
- ADR-0002 smritea-style DDD layout, single module
- ADR-0003 Echo + pedantigoecho + pedantigo v2
- ADR-0004 SSE single-writer goroutine + Option C post-stream logging
- ADR-0005 AppError → Anthropic error shape
- ADR-0006 Backend port + extend-via-composition translators
- ADR-0007 Conversation state stays core, common format
- ADR-0008 Lease machine ported verbatim
- ADR-0009 Library selections (coder/websocket, GORM+glebarez, viper,
  bubbletea, slog; keyring dropped)
- ADR-0010 Approved gap fixes (G4, G5, G7, G8, G9, G12 scope)
- ADR-0011 Formatted-text exchange log format
- ADR-0012 Rust kept until parity cutover; one-commit delete

### `site/contributing/extending/backends.md`
- How to add a backend: implement port.Backend, compose
  GenericBackendTranslator, register in the providers map, config entry,
  capability flags (what the core does when WS delta / server-side state
  is absent).

### `site/contributing/extending/translators.md`
- GenericBackendTranslator surface, what to override, compile-time
  interface assert convention.

### `site/contributing/testing.md`
- The CONTRACT/FAKE/SLOP-ADJACENT labeling, migration matrix, seam choice
  (HTTP boundary), flush-invariant stream tests, golden files.

### `site/contributing/releasing.md`
- Python packager retained; go build with CGO_ENABLED=0; target remap;
  formula contract; rollback story.

---

## Site chrome (when the website is built)

- Docusaurus; version selector pinned to gateway releases.
- Landing: three capability cards, Getting Started + Go Reference links.
- "Report an issue" → GitHub.
- Later: optional OpenTelemetry page under usage (G12) once the opt-in
  metrics endpoint ships.

## Authoring rules

- User docs never mention packages, files, or types. Contributors' docs
  never repeat user config tables — it links to them.
- One page = one job; a page that needs both audiences is two pages.
- Every user-doc claim must be verifiable against the shipped binary.
