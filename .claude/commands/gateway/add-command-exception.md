Add or update a Claude Code command exception in Gateway’s conversation inclusion layer.

Command arguments are provided in `$ARGUMENTS`.
Expected form:
- a single-line instruction describing one or more command exceptions to add or update
- examples:
  - `/rename is local-only; remove command and "Session renamed to:" stdout`
  - `/branch is local-only; remove command and branched-session stdout`
  - `/btw is read-only side context; do not persist it into the main conversation`
  - `/foo is prompt-backed; do not add an exception`
  - `/branch and /rename are local-only; remove their command envelopes and CLI stdout`

Parse `$ARGUMENTS` as one single-line exception request. It may mention one command or multiple commands. Treat it as the latest and only active task.

Before changing anything, read these files:
1. `core/domain/claudecode/commands.go`
2. `core/domain/claudecode/envelope.go`
3. `core/domain/claudecode/context.go`

Use the current code as the source of truth. Do not assume the exception model from memory.

Identify every command mentioned in `$ARGUMENTS`. For each command, classify it into exactly one category:

1. `local-only`
   - Meaning: Claude Code handled the command locally; the model must not see it as work to execute.
   - Examples: `/branch`, `/rename`.
   - Implementation target:
     - `commandsByName` in `core/domain/claudecode/commands.go` (each entry is an `internalCommandSpec{name, classification, stdoutMarkers}`; local-only entries use `classification: internalLocalOnly`).
   - Required data:
     - normalized command name without leading `/`
     - stdout marker text that uniquely identifies the local CLI result, when Claude Code emits stdout (goes in `stdoutMarkers []string`)
   - Behavior to preserve:
     - remove the command envelope
     - remove matching `<local-command-stdout>...</local-command-stdout>`
     - preserve any following real user prompt or active slash command
     - record `gateway_local_only_commands` metadata

2. `read-only`
   - Meaning: the model may answer this turn, but the turn should be marked as side-context rather than normal read-write conversation.
   - Example: Claude Code `/btw` side-question turns.
   - Implementation target:
     - `inclusionReadOnly` / `InclusionResult.ReadOnly` in `core/domain/claudecode/envelope.go`.
   - Required data:
     - stable marker text that Claude Code emits for this kind of side turn
   - Behavior to preserve:
     - keep the user’s side question visible to the model
     - set `gateway_conversation_inclusion` to `read_only` (via `extendClientMetadata`/`asStr()` in envelope.go)
     - do not classify a normal message containing the literal command name as read-only — `InclusionResult.ReadOnly` must never be set from message text; only from the structured client metadata/command tags already present in the request

3. `prompt-backed/read-write`
   - Meaning: the command expands to instructions for the model and should flow through normal slash-command promotion.
   - Examples: `/commit`, `/review_agent`, skill-like commands that include prompt instructions.
   - Implementation target:
     - no local-only or read-only exception should be added
   - Behavior to preserve:
     - latest command body is promoted into backend instructions
     - command arguments remain as current user input
     - older command envelopes stay historical only

Use the `AskQuestion` tool before editing if any of these are unclear:
- whether the command is local-only, read-only, or prompt-backed/read-write
- the exact command name
- the exact stdout marker for a local-only command
- the exact stable marker for a read-only command
- whether multiple commands are being requested in one line
- whether multiple mentioned commands share the same category and markers
- whether the requested exception would hide instructions that the model is supposed to execute

When asking, present the smallest possible question that resolves the ambiguity. Do not guess.

If logs are needed to identify the real Claude Code wire shape:
1. Inspect `~/.gateway/logs/http-exchange.log` (formatted text: `key: value` lines separated by a dashed line — not JSON/JSONL).
2. Search only for the exact command tag or marker related to the requested command.
3. Print only small snippets around matches.
4. Do not paste large request bodies into the response.

Implementation rules:
1. Explain why the current behavior is wrong and how the planned change fixes it before editing.
2. Reuse the existing conversation inclusion layer (`core/domain/claudecode/`). Do not add ad-hoc filtering in translator call sites (`core/domain/translator/`).
3. Keep command matching normalized so `/name` and `name` resolve consistently where the existing code supports it.
4. Add or update tests in `core/domain/claudecode/` (as `<file>_test.go` alongside the file being changed — this package currently has no test files, so a new exception may require creating the first one) and, where the exception affects translated output, in `core/domain/translator/generic_test.go`.
5. Tests must be self-contained and must not reference user-specific paths, real project names outside this repo, or machine-local state.
6. For `local-only`, test both command-envelope removal and stdout removal.
7. For `local-only` followed by a real prompt or command, test that the following prompt or command is still active.
8. For `read-only`, test that the side question remains visible and metadata is set.
9. For `prompt-backed/read-write`, do not add an exception; report that no code change is needed unless the current normal path is broken.

Validation:
1. Run `make fmt-check`.
2. If formatting fails only because of your edits, run `make fmt-fix`, then rerun `make fmt-check`.
3. Run the most focused relevant test first, e.g. `go test ./core/domain/claudecode/... -run <TestName>`.
4. Run `make lint`.
5. If the user asks for full validation, run `make check`.

Final response requirements:
- State each command name and final category.
- List the exact files changed.
- State the tests/validation run.
- If no code change was made because the command is prompt-backed/read-write, say that explicitly and explain why.
