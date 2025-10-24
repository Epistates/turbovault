# Docker Deployment

Run TurboVault in a Docker container.

## Dockerfile

```dockerfile
FROM rust:1.90

WORKDIR /app

COPY . .

RUN cargo build --release

ENTRYPOINT ["/app/target/release/turbovault"]
```

## Docker Compose

```yaml
version: '3.8'

services:
  turbovault:
    build: .
    volumes:
      - /path/to/vault:/vault
    environment:
      - RUST_LOG=info
    command: turbovault --vault /vault
```

## Run

```bash
docker compose up -d
```

## Coming in v0.2.0

- Pre-built images on Docker Hub
- Multi-architecture support (arm64, amd64)
- Smaller image size (~100 MB)
