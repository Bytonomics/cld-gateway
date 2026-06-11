Create a release tag for this repo using the current release version and the requested release kind.

Command arguments are provided in `$ARGUMENTS`.
Expected form:
- `<version> stable`
- `<version> alpha`
- `<version> beta`

Parse `$ARGUMENTS` into exactly two positional values:
1. semantic version string, e.g. `0.1.2`
2. release kind, one of `stable`, `alpha`, or `beta` (if nothing is given, assume `stable`)

Do the following. Strictly, step by step:

1. Read `Cargo.toml`, `RELEASE.md`, and `.github/workflows/release.yml`.
2. Validate the requested version against the repo’s accepted version/tag rules.
3. Validate that the requested version matches `[workspace.package].version` in `Cargo.toml` exactly.
4. Validate the release kind.
   - `stable` => `cld-gateway-vX.Y.Z`
   - `alpha` => `cld-gateway-vX.Y.Z-alpha`
   - `beta` => `cld-gateway-vX.Y.Z-beta`
5. Check whether the current repo has uncommitted or staged changes.
   - If the release commit is not done yet, proactively offer `/commit` and continue with tag creation if the user approves.
6. After checking or committing, first offer the uder to `git push` the main branch to the remote by asking user using AskQuestion tool.
7. Check whether the tag already exists locally or remotely.
8. Create the annotated tag only if all checks pass.
9. Offer to push the tag to origin after creating it, and push it if the user approves.
10. Report the exact tag name and the commit SHA it points to.
11. While doing the above, follow these constraints.
   - Complete every required validation before sending any user-facing response.
   - Do not provide partial progress updates, intermediate status, or “not done yet” messages.
   - If any check fails, report only the final blocking reason after completing the required checks.
   - Do not change files.
   - Do not create commits directly.
   - Do not push tags unless the user approves after you offer.
   - Do not guess the version or kind.
