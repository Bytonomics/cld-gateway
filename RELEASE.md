# Release Playbook

Maintainer-facing checklist for publishing a new `cld-gateway` release from `Bytonomics/cld-gateway` and updating the public Homebrew tap at `Bytonomics/homebrew-tap`.

---

## Platform scope

`cld-gateway` currently supports Unix-like platforms (macOS and Linux). Windows support is planned for a future release.

| Platform | Status | Published archive |
|---|---|---|
| macOS ARM64 | Supported | `cld-gateway-package-aarch64-apple-darwin.tar.gz` |
| macOS Intel | Supported | `cld-gateway-package-x86_64-apple-darwin.tar.gz` |
| Linux ARM64 | Supported | `cld-gateway-package-aarch64-unknown-linux-musl.tar.gz` |
| Linux x86_64 | Supported | `cld-gateway-package-x86_64-unknown-linux-musl.tar.gz` |
| Windows x64 | Future | not published in v1 |

Every published package archive must contain:

- `bin/cld-gateway`
- `cld-gateway-package.json`
- `config.yml`
- `settings.json`
- `homebrew/post_install.py` (Homebrew post-install helper)
- `bin/cld-gateway-sh` (Setup and diagnostics facade wrapper)
- `bin/cldg` (Claude wrapper script)
- `bin/clddg` (Dangerously-skip-permissions variant wrapper)
- `commands/` subtree (packaged translated-command files, e.g. `commands/codex/status.md`)

---

## Release architecture

There are two repositories involved.

### Main package repo

`Bytonomics/cld-gateway` builds and publishes GitHub Release assets.

A tag push matching `cld-gateway-vX.Y.Z` triggers `.github/workflows/release.yml`, which runs:

```text
tag-check → build (matrix) → verify → release
```

The release job publishes:

- all four target archives
- `cld-gateway-package_SHA256SUMS`
- `install.sh`

After publishing the release assets, the workflow dispatches a tap update event to `Bytonomics/homebrew-tap`.

### Homebrew tap repo

`Bytonomics/homebrew-tap` owns the formula at:

```text
Formula/cld-gateway.rb
```

The formula is generated from the checksum manifest published by `Bytonomics/cld-gateway`.

Before the first release exists, `Formula/cld-gateway.rb` may be a bootstrap placeholder because there are no release archives or SHA256 values yet. It becomes installable only after the first release publishes `cld-gateway-package_SHA256SUMS` and the tap workflow renders the real formula.

#### Homebrew formula architecture

The formula is now minimal DSL only: it installs the binary, configuration files, wrapper scripts, and the Python helper. The post-install hook delegates all user-home directory setup and symlink creation to the packaged Python helper at `homebrew/post_install.py`. The setup facade wrapper (`cld-gateway-sh`) is invoked by users after installation to complete configuration setup. Wrapper scripts (`cld-gateway-sh`, `cldg`, and `clddg`) are packaged as assets rather than Ruby-generated.

---

## Required GitHub setup

### 1. Ensure the tap repo is public and initialized

Before tagging a `cld-gateway` release, the tap repo must already be pushed to GitHub with its workflows present.

Required tap repo files:

- `LICENSE`
- `README.md`
- `Formula/cld-gateway.rb`
- `.github/scripts/check-version.sh`
- `.github/scripts/render-formula.py`
- `.github/scripts/tests/...`
- `.github/workflows/ci.yml`
- `.github/workflows/publish-formula-update.yml`
- `.github/workflows/manual-publish-formula-update.yml`

If you changed files inside the `homebrew-tap` submodule, push that repo first:

```sh
cd homebrew-tap
git status
git push origin main
cd ..
```

Then commit the updated submodule pointer in `Bytonomics/cld-gateway` if it changed.

### 2. Add the cross-repo dispatch secret

In `Bytonomics/cld-gateway`, configure this GitHub Actions secret:

```text
HOMEBREW_TAP_DISPATCH_TOKEN
```

The token must be able to dispatch repository events to:

```text
Bytonomics/homebrew-tap
```

The release workflow requires this secret. If it is missing, the release job fails before dispatching the tap update.

The dispatch contract is:

```text
event-type: version-updated
repository: Bytonomics/homebrew-tap
payload: {"version":"X.Y.Z"}
```

The tap repo workflow `publish-formula-update.yml` listens for this event and renders `Formula/cld-gateway.rb` from the published release manifest.

---

## Release steps

### 1. Verify the working tree

Start from the main repo root:

```sh
git status
```

Confirm only intended release changes are present. If the `homebrew-tap` submodule changed, make sure the submodule commit has already been pushed to `Bytonomics/homebrew-tap`, then include the updated submodule pointer in the main repo commit.

### 2. Pre-release validation

Run the local validation suite before tagging:

```sh
make check
RUN_WIREMOCK=1 make verify-test
```

All checks must pass before proceeding.

These checks validate the current Unix-like platform scope. Windows support is planned for a future release and does not currently have binary assets.

### 3. Choose and set the release version

The canonical version is in the root `Cargo.toml` under `[workspace.package]`:

```toml
[workspace.package]
version = "X.Y.Z"
```

Update `Cargo.toml` to the intended release version, then run the validation step below. `make check` already runs the normal test suite and will surface whether `Cargo.lock` needs to be updated. If `Cargo.lock` changes, include it in the release-preparation commit.

The release tag must match this version exactly:

```text
Cargo.toml version: X.Y.Z
Release tag:        cld-gateway-vX.Y.Z
```

The release workflow rejects mismatches.

### 4. Commit the release-ready state

Commit the version bump and any release-preparation changes.

Include the submodule pointer if `homebrew-tap` changed:

```sh
git add Cargo.toml Cargo.lock .github/workflows/release.yml README.md RELEASE.md homebrew-tap
git commit -m "chore: prepare cld-gateway vX.Y.Z release"
git push origin main
```

Only stage files that are intentionally part of the release preparation.

### 5. Create and push the release tag

Use the required `cld-gateway-v` prefix. The tag version must match the version in `Cargo.toml` exactly.

Set the release version once in your shell:

```sh
VERSION=X.Y.Z
```

Verify that `Cargo.toml` contains the same version:

```sh
grep -A5 '^\[workspace.package\]' Cargo.toml
```

Verify the tag does not already exist locally:

```sh
git tag --list "cld-gateway-v${VERSION}"
```

If that command prints anything, stop and investigate before continuing.

Create an annotated release tag:

```sh
git tag -a "cld-gateway-v${VERSION}" -m "Release ${VERSION}"
```

Verify the local tag points at the intended commit:

```sh
git show --stat "cld-gateway-v${VERSION}"
```

Push the tag:

```sh
git push origin "cld-gateway-v${VERSION}"
```

Verify the remote tag exists:

```sh
git ls-remote --tags origin "cld-gateway-v${VERSION}"
```

The tag push triggers the `release` workflow automatically.

```text
cld-gateway-vX.Y.Z
cld-gateway-vX.Y.Z-alpha
cld-gateway-vX.Y.Z-alpha.N
cld-gateway-vX.Y.Z-beta
cld-gateway-vX.Y.Z-beta.N
```

Do not use a bare `vX.Y.Z` tag.

### 6. Monitor the main release workflow

Open:

```text
https://github.com/Bytonomics/cld-gateway/actions
```

Wait for the `release` workflow.

The workflow is complete only when all jobs are green:

```text
tag-check → build (matrix) → verify → release
```

What each job does:

- `tag-check`: validates tag format and confirms tag version equals `Cargo.toml` version.
- `build`: builds all four target binaries and packages archives.
- `verify`: confirms each archive exists and contains `bin/cld-gateway`, `cld-gateway-package.json`, `config.yml`, `settings.json`, `homebrew/post_install.py`, `bin/cld-gateway-sh`, `bin/cldg`, `bin/clddg`, and the `commands/` subtree.
- `release`: publishes GitHub Release assets, generates `cld-gateway-package_SHA256SUMS`, and dispatches the Homebrew tap update.

### 6.c. Validating packaged setup (optional)

After the release is published and Homebrew tap is updated, maintainers can optionally validate the packaged setup behavior:

```sh
brew install bytonomics/tap/cld-gateway
cld-gateway-sh setup
cld-gateway-sh doctor
```

The `cld-gateway-sh setup` command runs the packaged Python helper with zero arguments; the helper derives paths internally. The packaged archive includes the `commands/` subtree (currently `commands/codex/status.md`), and setup syncs each file to `~/.codex_gateway/commands/` (e.g. `~/.codex_gateway/commands/codex/status.md`). After setup, the verifier checks that:

- `~/.gateway/config.yml` exists and matches packaged config
- `~/.claude_gateway/settings.json` exists and matches packaged settings
- `~/.codex_gateway/commands/` subtree exists and contains the packaged command files
- representative shared entries under `~/.claude_gateway` are valid as either directories or symlinks (not symlink-only)

### 6.a. If the release workflow fails before publishing assets

If the release workflow fails before publishing GitHub Release assets, fix the code/workflow, commit the fix, push `main`, then move the same release tag to the fixed commit.

Use one copy-paste block:

```sh
VERSION=X.Y.Z
TAG="cld-gateway-v${VERSION}"

git status
git add .github/workflows/release.yml README.md RELEASE.md homebrew-tap Cargo.toml Cargo.lock
git commit -m "fix(release): repair ${TAG} release workflow"
git push origin main

git tag -fa "${TAG}" -m "Release ${VERSION}"
git push --force origin "${TAG}"

git rev-parse HEAD
git ls-remote --tags origin "${TAG}"
```

### 6.b. If the release workflow fails after publishing assets

If GitHub Release assets already exist for the tag, do not blindly reuse the tag until you inspect whether a partial release was published. Delete or replace the partial GitHub Release only after confirming it is incomplete, then move the tag to the fixed commit.

Use one copy-paste block:

```sh
VERSION=X.Y.Z
TAG="cld-gateway-v${VERSION}"

git status
git add .github/workflows/release.yml README.md RELEASE.md homebrew-tap Cargo.toml Cargo.lock
git commit -m "fix(release): repair ${TAG} release workflow"
git push origin main

gh release view "${TAG}"
gh release delete "${TAG}" --yes --cleanup-tag

git tag -fa "${TAG}" -m "Release ${VERSION}"
git push --force origin "${TAG}"

git rev-parse HEAD
git ls-remote --tags origin "${TAG}"
```

Only use `gh release delete ... --cleanup-tag` for a failed/partial release that should be replaced. Do not use it for a release users may already have consumed.

### 7. Verify GitHub Release assets

After the workflow completes, open:

```text
https://github.com/Bytonomics/cld-gateway/releases/tag/cld-gateway-vX.Y.Z
```

Confirm these assets are present:

- `cld-gateway-package-aarch64-apple-darwin.tar.gz`
- `cld-gateway-package-x86_64-apple-darwin.tar.gz`
- `cld-gateway-package-aarch64-unknown-linux-musl.tar.gz`
- `cld-gateway-package-x86_64-unknown-linux-musl.tar.gz`
- `cld-gateway-package_SHA256SUMS`
- `install.sh`

Inspect `cld-gateway-package_SHA256SUMS` and confirm it has one checksum line per archive.

If any asset is missing, inspect the `release` workflow logs before continuing.

### 8. Monitor the Homebrew tap update

The main release workflow dispatches a `version-updated` event to `Bytonomics/homebrew-tap` after publishing the release assets.

Open:

```text
https://github.com/Bytonomics/homebrew-tap/actions
```

Monitor the `publish-formula-update` workflow.

It should:

1. receive payload `{"version":"X.Y.Z"}`
2. run `.github/scripts/check-version.sh X.Y.Z`
3. fetch `cld-gateway-package_SHA256SUMS` from the new release
4. render `Formula/cld-gateway.rb`
5. validate the formula
6. commit and push the formula update to `main`

After it completes, confirm `Formula/cld-gateway.rb` references:

```text
https://github.com/Bytonomics/cld-gateway/releases/download/cld-gateway-vX.Y.Z/...
```

and contains real SHA256 values from `cld-gateway-package_SHA256SUMS`.

### 9. If automatic tap update fails, run the manual fallback

If the dispatch does not run or the automated tap workflow fails, open:

```text
https://github.com/Bytonomics/homebrew-tap/actions/workflows/manual-publish-formula-update.yml
```

Run the workflow with:

```text
version = X.Y.Z
```

The manual workflow uses the same renderer and checksum manifest as the automated workflow.

Do not hand-edit formula checksums unless the workflows cannot be used.

### 10. Validate Homebrew formula fetch and audit

After the tap formula update lands:

```sh
sh scripts/install/homebrew-reinstall.sh
```

If you only want the low-level formula fetch/audit checks without reinstalling:

```sh
brew tap bytonomics/tap
brew fetch --force --formula cld-gateway
HOMEBREW_NO_INSTALL_FROM_API=1 brew audit --strict --online bytonomics/tap/cld-gateway
```

This matches the current tap workflow behavior: Homebrew must be able to resolve the formula, fetch the published release artifact for the current platform, and pass the strict online audit that runs in `homebrew-tap` CI.

### 11. Validate Homebrew install and wrappers

Run the installed verification helper from the main repo:

```sh
sh scripts/install/homebrew-verify.sh
```

This verifies:

- the `cld-gateway` binary is installed
- wrapper commands `cld-gateway-sh`, `cldg`, and `clddg` exist
- gateway runtime config exists at `~/.gateway/config.yml`
- Claude settings for wrappers exist at `~/.claude_gateway/settings.json`
- wrapper script contents point at the runtime settings path
- the health endpoint responds using the address read from the installed config file

The `cld-gateway-sh setup` command initializes user-home configuration and shared Claude entries. The `cldg` and `clddg` wrappers shell out to `claude`. The verification helper always inspects the wrapper script contents, and will also try executing them only when `claude` is available on `PATH`.

### 12. Validate Homebrew service support

The verification helper already checks the installed health endpoint by reading `~/.gateway/config.yml`.

If you want to validate Homebrew Services explicitly:

```sh
brew services start cld-gateway
curl -fsSL http://127.0.0.1:6473/health
brew services stop cld-gateway
```

The current packaged Homebrew config uses `127.0.0.1:6473`, and the formula service sets `GATEWAY_CONFIG_PATH` to the installed user-home config path.

### 13. Validate shell installer

Verify the latest-release installer:

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh
```

To pin a specific release explicitly:

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh -s -- --release X.Y.Z
```

Then verify the installed binary executes:

```sh
~/.local/bin/cld-gateway invalid-command 2>&1 | grep -q "unknown command"
```

### 14. Validate daemon startup

Run:

```sh
cld-gateway serve
```

Expected behavior:

- listens on `127.0.0.1:6483` by default
- does not require Homebrew after installation
- when neither `GATEWAY_CONFIG_PATH` nor `GATEWAY_HOME` is set, runtime falls back to `~/.gateway/config-dev.yml`
- auth/login can be tested separately

In another shell, verify health:

```sh
curl -fsSL http://127.0.0.1:6483/health
```

### 15. Validate login/auth flow

Run the installed binary login flow:

```sh
cld-gateway login
```

The current CLI also supports explicit vendor selection:

```sh
cld-gateway login openai
cld-gateway login gemini
```

`cld-gateway login` and `cld-gateway login openai` are equivalent today because the CLI defaults `login` to the OpenAI flow.

Then start the daemon:

```sh
cld-gateway serve
```

Send a real client request through the gateway and verify it succeeds.

### 16. Auth callback port verification

During installation testing, verify OAuth callback port fallback behavior:

- default auth callback port is `1455`
- it is configurable via `CLD_GATEWAY_AUTH_PORT`
- if binding the preferred port fails, the gateway falls back to port `1457`
- login succeeds as long as the gateway can bind a supported localhost callback port and the browser can reach it

Test this by occupying port `1455`, then running login again.

---

## First-release checklist

For the very first package release, there are no existing GitHub Release assets yet. The bootstrap formula in the tap is expected to be non-installable until the first release publishes archives and checksums.

Before pushing the first tag, confirm:

- `Bytonomics/homebrew-tap` is public and pushed.
- `Bytonomics/homebrew-tap` contains the tap workflows.
- `Bytonomics/cld-gateway` has `HOMEBREW_TAP_DISPATCH_TOKEN` configured.
- The submodule pointer in `Bytonomics/cld-gateway` points at the pushed tap commit.
- `Cargo.toml` version matches the tag you are about to push.
- The tag uses the `cld-gateway-v` prefix.

Example first release for `0.1.0`:

```sh
# Push tap repo first if it changed.
cd homebrew-tap
git status
git push origin main
cd ..

# Validate main repo.
make check
make test
RUN_WIREMOCK=1 make verify-test

# Commit release-ready state.
git status
git add Cargo.toml Cargo.lock .github/workflows/release.yml README.md RELEASE.md homebrew-tap
git commit -m "chore: prepare cld-gateway v0.1.0 release"
git push origin main

# Tag and publish.
git tag -a cld-gateway-v0.1.0 -m "Release 0.1.0"
git push origin cld-gateway-v0.1.0
```

Then monitor:

1. `Bytonomics/cld-gateway` → `release` workflow
2. `Bytonomics/homebrew-tap` → `publish-formula-update` workflow
3. GitHub Release assets
4. Homebrew fetch/install
5. shell installer
6. daemon/login smoke checks

---

## Notes

**Homebrew tap maintenance:** The tap update is automated through repository dispatch after the main release publishes assets. Use `manual-publish-formula-update.yml` only as a fallback.

**Version format:** All release tags use the format `cld-gateway-vX.Y.Z`. Do not use bare `vX.Y.Z` tags.

**Tap license vs package license:** `Bytonomics/homebrew-tap` is licensed separately as the tap repository. The formula’s `license` field describes the packaged `cld-gateway` software license.
