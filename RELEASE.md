# Release Playbook

Maintainer-facing checklist for publishing a new `cld-gateway` release.

---

## Prerequisites

- Push tag access to `bytonomics/gateway`
- Ability to create GitHub Releases on `bytonomics/gateway`
- Access to update `bytonomics/homebrew-tap` (for Homebrew formula update)
- Homebrew installed locally (for formula validation)
- Python 3.8+ (for package builder scripts)

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
- `install.ps1`

If any asset is missing, check the workflow logs for the `release` job.

### 6. Update the Homebrew tap

In the `bytonomics/homebrew-tap` repo, update `Formula/cld-gateway.rb`:

- Set `version` to `X.Y.Z`
- Update `url` for each macOS target to point at the new GitHub Release URLs
- Update `sha256` for each asset using values from `cld-gateway-package_SHA256SUMS`

Fetch the SHA256 values:

```sh
curl -fsSL https://github.com/bytonomics/gateway/releases/download/cld-gateway-vX.Y.Z/cld-gateway-package_SHA256SUMS
```

Use the output to update the corresponding `sha256` fields in the formula. Commit and push to `bytonomics/homebrew-tap`.

### 7. Validate Homebrew install

```sh
brew tap bytonomics/tap
brew install cld-gateway
cld-gateway --help || cld-gateway
```

Confirm the installed binary reports the expected version and starts without error.

### 8. Post-release sanity check

Verify the shell installer works from the published release:

```sh
curl -fsSL https://github.com/bytonomics/gateway/releases/latest/download/install.sh | sh
```

Then run the installed binary:

```sh
~/.local/bin/cld-gateway
```

Confirm it starts, listens on `127.0.0.1:8080`, and responds to `GET /health`.

---

## Notes

**Homebrew tap maintenance (v1):** The Homebrew tap is updated manually for v1. Future releases may automate tap PR creation via the release workflow.

**Version format:** All release tags use the format `cld-gateway-vX.Y.Z`. Do not use bare `vX.Y.Z` tags — the release workflow matches on the `cld-gateway-v` prefix.
