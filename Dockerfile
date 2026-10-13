# syntax=docker/dockerfile:1.9
# TurboVault MCP server, built with the same feature set as the prebuilt
# release binaries (every transport, SQL, compiled-in plugins off until
# `--plugins` names them).
#
# Base images are pinned by digest so a rebuild of the same commit uses the
# same toolchain and runtime. Dependabot proposes digest updates.

# Stage 1: Builder
FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS builder

# libgit2 is vendored and built from source: it needs cmake and a C compiler.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# The lockfile is authoritative; a build that would change it fails.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --package turbovault --features release,unix \
    && cp target/release/turbovault /usr/local/bin/turbovault

# Stage 2: Runtime
FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/turbovault /usr/local/bin/turbovault

# Non-root. UID 1000 matches the first user on most Linux hosts, so a vault
# bind-mounted from the host stays writable without a chown.
RUN useradd --create-home --uid 1000 obsidian \
    && mkdir -p /var/obsidian-vault \
    && chown obsidian:obsidian /var/obsidian-vault

USER obsidian
WORKDIR /var/obsidian-vault

ENV RUST_LOG=info
ENV OBSIDIAN_VAULT_PATH=/var/obsidian-vault

# The MCP Registry checks this label to verify the image belongs to the
# server named in server.json.
LABEL io.modelcontextprotocol.server.name="io.github.epistates/turbovault"

# There is no HTTP health endpoint. Over stdio (the default) the client owns
# the process. Over a network transport (`--transport http --host 0.0.0.0`),
# probe the port from the orchestrator, e.g. a Kubernetes tcpSocket probe.

# Mount a vault at runtime with: -v /path/to/vault:/var/obsidian-vault
ENTRYPOINT ["/usr/local/bin/turbovault", "--profile", "production", "--init"]
