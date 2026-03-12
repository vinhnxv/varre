.PHONY: build run release install clean check test fmt lint tui help

# Default target
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build: ## Build debug binary
	cargo build

release: ## Build optimized release binary
	cargo build --release

run: ## Run varre (pass ARGS, e.g. make run ARGS="list")
	cargo run -- $(ARGS)

tui: ## Launch TUI dashboard
	cargo run -- tui

install: ## Install varre to ~/.cargo/bin
	cargo install --path .

check: ## Run cargo check
	cargo check

test: ## Run tests
	cargo test

fmt: ## Format code
	cargo fmt

lint: ## Run clippy lints
	cargo clippy -- -D warnings

clean: ## Remove build artifacts
	cargo clean
