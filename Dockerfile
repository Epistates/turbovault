# syntax=docker/dockerfile:1.9
# Multi-stage build for Rust TurboVault Server

# Stage 1: Builder
FROM rust:latest as builder

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build server in release mode
RUN cargo build --release --package turbovault-server

# Stage 2: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /build/target/release/turbovault /usr/local/bin/

# Create non-root user
RUN useradd -m -u 1000 obsidian

# Create vault directory
RUN mkdir -p /var/obsidian-vault && chown obsidian:obsidian /var/obsidian-vault

# Switch to non-root user
USER obsidian

# Set working directory
WORKDIR /var/obsidian-vault

# Environment variables
ENV RUST_LOG=info
ENV OBSIDIAN_VAULT_PATH=/var/obsidian-vault

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD /usr/local/bin/turbovault --help > /dev/null 2>&1 || exit 1

# Run server with STDIO transport (MCP protocol - standard)
# Can be mounted at runtime with: -v /path/to/vault:/var/obsidian-vault
ENTRYPOINT ["/usr/local/bin/turbovault", "--profile", "production", "--init"]

