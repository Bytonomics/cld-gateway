Update the release version across the known version-pinned files in this repo.

Command arguments are provided in `$ARGUMENTS`.
Expected form:
- `<version>`

Parse `$ARGUMENTS` as exactly one positional value:
1. semantic version string, e.g. `0.1.2`

Rules:
1. Read every target file before changing it.
2. Validate the argument is a plain semantic version: `X.Y.Z` with optional `-alpha(.N)` or `-beta(.N)` suffix only if needed by the repo’s existing version rules.
3. Update only the exact release/version references that are supposed to move together.
4. Do not make unrelated edits.
5. After editing, show a concise summary of which files were updated and what old value changed to what new value.

Current required update targets:
- `Cargo.toml`
  - `[workspace.package].version`
- `README.md`
  - pinned shell installer example currently using a concrete release value
- `homebrew-tap/Formula/cld-gateway.rb`
  - any release asset URLs pinned to `cld-gateway-v<version>`

Behavior:
- If additional exact version references are discovered while reading those files and they are clearly part of the same release-version set, include them.
- If a found version reference is ambiguous, stop and ask before changing it.
- Do not create commits.
- Do not tag releases.
- Do not push anything.

Suggested execution order:
1. Parse `$ARGUMENTS` into the new version.
2. Read the target files.
3. Replace the old version with the new version in the approved places only.
4. Summarize the edits.
