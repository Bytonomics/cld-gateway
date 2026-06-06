# Release Playbook

Maintainer-facing checklist for publishing a new `cld-gateway` release.

---

## Platform Scope

`cld-gateway` currently supports Unix-like platforms (macOS and Linux). Windows support is planned for a future release.

| Platform | Status | Installer |
|----------|--------|-----------|
| macOS (Intel) | Supported | `install.sh` |
| macOS (ARM64) | Supported | `install.sh` |
| Linux x86_64 | Supported | `install.sh` |
| Linux ARM64 | Supported | `install.sh` |
| Windows (x64) | Future | not published in v1 |

## Prerequisites

- Push tag access to `bytonomics/gateway`
- Ability to create GitHub Releases on `bytonomics/gateway`
- Python 3.9+ (for package builder scripts)
- Rust stable toolchain (for building binaries locally if needed)
- macOS or Linux (Windows support is planned for a future release)
- `uv` installed locally for release-tooling tests and package-builder commands

### Optional prerequisites

- Access to update `bytonomics/homebrew-tap` (only required if maintaining the Homebrew formula)
- Homebrew installed locally (only required for validating the formula before publishing)
- Zig and cargo-zigbuild (automatically installed by the CI workflow for Linux targets)

---

## Release steps

### 1. Pre-release validation

Run the full test suite locally before tagging:

```sh
make check
make test
RUN_WIREMOCK=1 make verify-test
```

All checks must pass before proceeding.

**Note:** These tests validate Unix/Linux/macOS targets. Windows support is planned for a future release and does not currently have binary assets.

### 2. Bump the version

Edit the root `Cargo.toml` and change `version` under `[workspace.package]` from the current version to the new one:

```toml
[workspace.package]
version = "X.Y.Z"
```

Then update `Cargo.lock`:

```sh
cargo build -p gatewayd
```

Commit the change:

```sh
git commit -am "chore: bump version to X.Y.Z"
```

### 3. Tag and push

```sh
git tag cld-gateway-vX.Y.Z
git push origin cld-gateway-vX.Y.Z
```

The tag push triggers the release workflow automatically.

### 4. Monitor the release workflow

Go to: `https://github.com/bytonomics/gateway/actions`

Wait for the `release` workflow to complete. It runs four jobs in sequence:

```
tag-check → build (matrix) → verify → release
```

The build matrix produces binaries for all supported targets. The workflow is complete only when all four jobs are green.

### 5. Verify GitHub Release assets

After the workflow completes, open the release at:

```
https://github.com/bytonomics/gateway/releases/tag/cld-gateway-vX.Y.Z
```

Confirm the following assets are present:

- `cld-gateway-package-aarch64-apple-darwin.tar.gz`
- `cld-gateway-package-x86_64-apple-darwin.tar.gz`
- `cld-gateway-package-aarch64-unknown-linux-musl.tar.gz`
- `cld-gateway-package-x86_64-unknown-linux-musl.tar.gz`
- `cld-gateway-package_SHA256SUMS`
- `install.sh`

If any asset is missing, check the workflow logs for the `release` job.

### 6. Update the Homebrew tap

The release workflow publishes the GitHub Release first, then dispatches a `version-updated` event to `bytonomics/homebrew-tap`.

Monitor the tap repo workflow run and confirm that it rewrites `Formula/cld-gateway.rb` for `X.Y.Z` using the published `cld-gateway-package_SHA256SUMS` manifest.

If the dispatch does not run or fails, trigger the manual fallback workflow in `bytonomics/homebrew-tap` with the same version.

### 7. Validate Homebrew formula

After the tap workflow completes, validate that the formula resolves to the expected release artifacts:

```sh
brew tap bytonomics/homebrew-tap
brew fetch --dry-run cld-gateway
```

This confirms that the formula can fetch the published release artifacts.

### 8. Validate Homebrew install

```sh
brew tap bytonomics/homebrew-tap
brew install cld-gateway
cld-gateway invalid-command 2>&1 | grep -q "unknown command"
```

Confirm the installed binary prints the deterministic `unknown command` parser error from the current CLI.

### 9. Post-release sanity check (shell installer)

Verify the shell installer works from the published release:

```sh
curl -fsSL https://github.com/bytonomics/gateway/releases/latest/download/install.sh | sh
```

Then run the installed binary:

```sh
~/.local/bin/cld-gateway
```

Confirm it starts, listens on `127.0.0.1:8080`, and responds to `GET /health`.

### 10. Auth callback port verification

During installation testing, verify that the OAuth callback port selection works as expected:

- The default auth callback port is `1455` (configurable via `CLD_GATEWAY_AUTH_PORT`)
- If binding the preferred port fails, the gateway automatically falls back to port `1457`
- Login succeeds as long as the gateway can bind one of those localhost ports and the resulting callback URL is reachable in the browser
- Users can set `CLD_GATEWAY_AUTH_PORT=<custom_port>` to prefer a non-default port

Test this by running the gateway while port 1455 is occupied by another service.

---

## Notes

**Homebrew tap maintenance (v1):** The Homebrew tap is updated manually for v1. Future releases may automate tap PR creation via the release workflow.

**Version format:** All release tags use the format `cld-gateway-vX.Y.Z`. Do not use bare `vX.Y.Z` tags — the release workflow matches on the `cld-gateway-v` prefix.
