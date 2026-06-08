Create a release tag for this repo using the current release version and the requested release kind.

Command arguments are provided in `$ARGUMENTS`.
Expected form:
- `<version> stable`
- `<version> alpha`
- `<version> beta`

Parse `$ARGUMENTS` into exactly two positional values:
1. semantic version string, e.g. `0.1.2`
2. release kind, one of `stable`, `alpha`, or `beta`

Rules:
1. Read `Cargo.toml`, `RELEASE.md`, and `.github/workflows/release.yml` before acting.
2. Complete every required validation before sending any user-facing response.
3. Do not provide partial progress updates, intermediate status, or “not done yet” messages. Respond only after all checks below have been executed or an actual blocking failure has been confirmed.
4. Validate that the requested version matches the repo’s accepted version/tag rules.
5. Validate that the requested version matches `[workspace.package].version` in `Cargo.toml` exactly. If it does not match, stop and tell the user to update the version first.
6. Validate the release kind:
   - `stable` => tag format `cld-gateway-vX.Y.Z`
   - `alpha` => tag format `cld-gateway-vX.Y.Z-alpha`
   - `beta` => tag format `cld-gateway-vX.Y.Z-beta`
7. Before creating the tag, check whether the current repo has uncommitted or staged changes. If there are any changes that mean the release commit is not done yet, stop and tell the user to commit first.
8. Check whether the tag already exists locally or remotely. If it exists, stop and report it.
9. If everything is valid, create the annotated tag only. Do not push unless the user explicitly asks.
10. After creating the tag, report the exact tag name and the commit SHA it points to.

Expected checks:
- `Cargo.toml` `[workspace.package].version`
- accepted workflow tag patterns from `.github/workflows/release.yml`
- release instructions in `RELEASE.md`
- current git status
- existing local and remote tags matching the requested tag name

Do not:
- change files
- create commits
- push tags
- guess the version or kind

Suggested execution order:
1. Parse `$ARGUMENTS` into version + release kind.
2. Read `Cargo.toml`, `.github/workflows/release.yml`, and `RELEASE.md`.
3. Build the exact tag name from version + kind.
4. Check git status.
5. Check local and remote tag existence.
6. Create annotated tag if and only if all checks pass.
7. Print the tag name and target commit SHA.
8. If any check fails, report only the final blocking reason after completing the required checks; do not emit partial status updates.
