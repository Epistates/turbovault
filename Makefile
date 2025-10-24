.PHONY: help build test test-all test-quick test-verbose test-one test-integration test-unit clean check fmt fmt-check lint clippy-fix doc run docker-build docker-up docker-down docker-logs release ci all

# Colors for output
RESET := \033[0m
BOLD := \033[1m
GREEN := \033[32m
CYAN := \033[36m
YELLOW := \033[33m

.DEFAULT_GOAL := help

help: ## Show this help message
	@echo "$(BOLD)turbovault - TurboVault Server$(RESET)\n"
	@echo "$(BOLD)Available targets:$(RESET)"
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / {printf "  $(CYAN)%-20s$(RESET) %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# =============================================================================
# BUILD & COMPILATION
# =============================================================================

build: ## Build debug binary
	cargo build

release: ## Build optimized release binary
	cargo build --release

check: ## Check code without building
	cargo check --all

# =============================================================================
# TESTING
# =============================================================================

test: fmt-check lint test-all ## Run full test suite with quality checks

test-all: ## Run all tests (lib, integration, and doc tests)
	cargo test --workspace --all-features

test-quick: ## Run tests only (skip fmt and lint checks)
	cargo test --workspace --all-features

test-verbose: ## Run tests with output
	cargo test --workspace --all-features -- --nocapture

test-one: ## Run single test (pass TEST=module::test_name)
	cargo test --workspace $(TEST) -- --nocapture

test-integration: ## Run only integration tests
	cargo test --workspace --tests

test-unit: ## Run only unit tests
	cargo test --workspace --lib

# =============================================================================
# CODE QUALITY
# =============================================================================

fmt: ## Format code
	cargo fmt --all

fmt-check: ## Check formatting
	cargo fmt --all -- --check

lint: ## Run clippy linter
	cargo clippy --workspace --all-features --all-targets -- -D warnings

clippy-fix: ## Auto-fix clippy warnings
	cargo clippy --fix --allow-dirty

# =============================================================================
# DOCUMENTATION
# =============================================================================

doc: ## Generate documentation
	cargo doc --no-deps --open

# =============================================================================
# CLEANING
# =============================================================================

clean: ## Clean build artifacts
	cargo clean

# =============================================================================
# DEVELOPMENT
# =============================================================================

dev: check test ## Run checks and tests (development workflow)

setup: ## Install Rust and dependencies
	@echo "$(BOLD)Setting up Rust environment...$(RESET)"
	@command -v cargo >/dev/null 2>&1 || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
	@echo "$(GREEN)✓ Rust ready$(RESET)"

# =============================================================================
# DOCKER
# =============================================================================

docker-build: ## Build Docker image
	docker build -t turbovault:latest .

docker-up: ## Start services with docker-compose
	docker-compose up -d

docker-down: ## Stop services
	docker-compose down

docker-logs: ## View docker logs
	docker-compose logs -f

# =============================================================================
# PRODUCTION
# =============================================================================

run: release ## Run the server
	./target/release/turbovault-server

status: ## Check server status
	curl -s http://localhost:3000/status | jq .

# =============================================================================
# UTILITIES
# =============================================================================

info: ## Show project info
	@echo "$(BOLD)turbovault - Rust TurboVault Server$(RESET)"
	@echo "Version: 1.0.0"
	@echo "Crates: 8 (core, parser, graph, vault, batch, export, tools, server)"
	@echo "Tests: 165 passing"
	@echo ""
	@echo "$(BOLD)Rust version:$(RESET)"
	@rustc --version
	@cargo --version

ci: fmt-check lint test-all ## Run CI pipeline (fmt check, lint, test)
	@echo "$(GREEN)✓ CI checks passed$(RESET)"

all: fmt-check lint test-all release ## Run full CI pipeline (fmt, lint, test, release)
	@echo "$(GREEN)✓ CI pipeline complete$(RESET)"
