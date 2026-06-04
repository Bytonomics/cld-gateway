.PHONY: check fmt-check fmt-fix clippy test test-release-scripts metadata tree
.PHONY: verify-test

check: fmt-check clippy test test-release-scripts

test-release-scripts:
	uv run --project scripts/release pytest scripts/release/test/

fmt-check:
	cargo fmt --check

fmt-fix:
	cargo fmt

clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features

verify-test:
	RUN_WIREMOCK=1 cargo test --workspace --all-features

metadata:
	cargo metadata --no-deps --format-version 1

tree:
	cargo tree --workspace
