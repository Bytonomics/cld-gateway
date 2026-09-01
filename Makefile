ROOT_DIR := $(shell dirname $(realpath $(firstword $(MAKEFILE_LIST))))

.PHONY: help check fmt-check fmt-fix lint test verify-test \
    test-release-scripts metadata tree install vendor build build-check

help: ## Show this help (wait-formally style)
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

install: vendor ## Install deps (vendor Go modules)

vendor: ## go mod download + tidy + vendor
	cd $(ROOT_DIR) && go mod download && go mod tidy && go mod vendor

build: ## Build the gateway binary
	cd $(ROOT_DIR) && CGO_ENABLED=0 go build -o bin/cld-gateway ./cmd/cld-gateway

build-check: ## Compile-only check
	cd $(ROOT_DIR) && go build -gcflags="-e" ./...

check: fmt-check lint test test-release-scripts

fmt-check: ## gofmt + goimports verify
	cd $(ROOT_DIR) && test -z $$(gofmt -l . | grep -v vendor) && goimports -l . | grep -v vendor | (! read)

fmt-fix: ## Auto-format
	cd $(ROOT_DIR) && golangci-lint run --fix ./... || true
	cd $(ROOT_DIR) && goimports -w $$(find . -name '*.go' -not -path './vendor/*')
	cd $(ROOT_DIR) && gci write --skip-generated --skip-vendor -s standard -s default -s "prefix(github.com/Bytonomics/cld-gateway)" .

lint: ## golangci-lint
	cd $(ROOT_DIR) && golangci-lint run ./...

test: ## Run all tests
	cd $(ROOT_DIR) && go test ./...

verify-test: ## Tests with mock-backend gate
	cd $(ROOT_DIR) && RUN_MOCK_BACKEND=1 go test -tags=mockbackend ./...

test-release-scripts: ## Release packager tests (unchanged Python)
	UV_CACHE_DIR=/tmp/gateway-uv-cache python3 scripts/timeout.py 120 \
	  uv run --project scripts/release pytest scripts/release/test/

metadata: ## go list -m all
	cd $(ROOT_DIR) && go list -m all

tree: ## go mod graph
	cd $(ROOT_DIR) && go mod graph
