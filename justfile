# TurboVault - justfile
# Run `just` to see available recipes

set shell := ["bash", "-cu"]

# Default recipe - show help
default:
    @just --list

# =============================================================================
# BUILD & COMPILATION
# =============================================================================

# Build debug binary
build:
    cargo build

# Build optimized release binary
release:
    cargo build --release

# Check code without building
check:
    cargo check --all

# =============================================================================
# TESTING
# =============================================================================

# Run full test suite with quality checks
test: fmt-check lint test-all

# Run all tests (lib, integration, and doc tests)
test-all:
    cargo test --workspace --all-features

# cargo's fail-fast is per-binary: it stops launching binaries after the first
# fails, under-reporting failures that span crates. Slower; use in TDD red phases.
# Run all tests, reporting EVERY failure across all binaries (--no-fail-fast)
test-all-full:
    cargo test --workspace --all-features --no-fail-fast

# Run tests only (skip fmt and lint checks)
test-quick:
    cargo test --workspace --all-features

# Run tests with output
test-verbose:
    cargo test --workspace --all-features -- --nocapture

# Run single test (e.g., just test-one module::test_name)
test-one TEST:
    cargo test --workspace {{ TEST }} -- --nocapture

# Run only integration tests
test-integration:
    cargo test --workspace --tests

# Run only unit tests
test-unit:
    cargo test --workspace --lib

# Run instrumented coverage and enforce the CI line-coverage floor
coverage:
    cargo llvm-cov --workspace --all-features --locked --summary-only --fail-under-lines 75

# =============================================================================
# WRITE-SAFETY MATRIX (WSS)
# =============================================================================

# Write-safety burndown report: per-op precondition×state grid coloured by
# pass/fail across both backends (terminal ANSI). Exits non-zero off-fixpoint.
wss-report:
    python3 scripts/wss-report.py

# Same report as one self-contained HTML file (default: wss-report.html).
wss-report-html OUT="wss-report.html":
    python3 scripts/wss-report.py --html {{ OUT }}

# Run only the write-safety matrix (active cells; pending are ignored).
wss-test:
    cargo test --test wss_matrix

# =============================================================================
# CODE QUALITY
# =============================================================================

# Format code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Run clippy linter
lint:
    cargo clippy --workspace --all-features --all-targets -- -D warnings

# Auto-fix clippy warnings
clippy-fix:
    cargo clippy --fix --allow-dirty

# =============================================================================
# MUTATION TESTING (cargo-mutants — test EFFECTIVENESS, not just coverage)
# =============================================================================

# Mutation-test the substrate crate (highest-value, fastest signal). A
# surviving mutant = code a test runs but does NOT actually assert on.
mutants:
    cargo mutants -p turbovault-git

# Mutation-test one crate, e.g. `just mutants-crate turbovault-tools`
mutants-crate CRATE:
    cargo mutants -p {{ CRATE }}

# Mutation-test the whole workspace (SLOW — many minutes).
mutants-all:
    cargo mutants --workspace

# =============================================================================
# DOCUMENTATION
# =============================================================================

# Generate documentation
doc:
    cargo doc --no-deps --open

# =============================================================================
# CLEANING
# =============================================================================

# Clean build artifacts
clean:
    cargo clean

# =============================================================================
# DEVELOPMENT
# =============================================================================

# Run checks and tests (development workflow)
dev: check test

# Install Rust and dependencies
setup:
    @echo "Setting up Rust environment..."
    @command -v cargo >/dev/null 2>&1 || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    @echo "Rust ready"

# =============================================================================
# DOCKER
# =============================================================================

# Build Docker image
docker-build:
    docker build -t turbovault:latest .

# Start services with docker-compose
docker-up:
    docker-compose up -d

# Stop services
docker-down:
    docker-compose down

# View docker logs
docker-logs:
    docker-compose logs -f

# =============================================================================
# PRODUCTION
# =============================================================================

# Run the server
run: release
    ./target/release/turbovault

# Check server status (requires HTTP transport mode on port 3000)
status:
    @echo "Note: This only works with HTTP transport (--transport http --port 3000)"
    curl -sf http://localhost:3000/status | jq . || echo "Server not responding on HTTP port 3000"

# =============================================================================
# UTILITIES
# =============================================================================

# Show project info
info:
    @echo "TurboVault - Rust TurboVault Server"
    @grep '^version' Cargo.toml | head -1 | sed 's/.*= *"/Version: /' | sed 's/"//'
    @echo "Crates: 9 (core, audit, parser, graph, vault, batch, export, tools, binary)"
    @echo ""
    @echo "Rust version:"
    @rustc --version
    @cargo --version

# Run CI pipeline (fmt check, lint, test)
ci: fmt-check lint test-all
    @echo "CI checks passed"

# Run full CI pipeline (fmt, lint, test, release)
all: fmt-check lint test-all release
    @echo "CI pipeline complete"
