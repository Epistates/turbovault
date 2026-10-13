# Docker Deployment

Each release publishes a multi-arch image (linux/amd64 and linux/arm64) to
GitHub Container Registry, built with the same features as the prebuilt
binaries: every transport, SQL queries, and the compiled-in plugins (off until
`--plugins` names them).

```bash
docker pull ghcr.io/epistates/turbovault:latest   # or a version: 3, 3.4, 3.4.0
```

The image runs as a non-root user (UID 1000) and starts with
`turbovault --profile production --init`, serving the vault mounted at
`/var/obsidian-vault`.

## Over stdio (MCP clients)

MCP clients talk to the server over stdin and stdout, so run it with `-i` and
without a TTY:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "/path/to/your/vault:/var/obsidian-vault",
        "ghcr.io/epistates/turbovault:latest"
      ]
    }
  }
}
```

Arguments after the image name are added to the server's own, for example
`--plugins vector_search`.

## Over a network transport

```bash
docker run -d --name turbovault -p 127.0.0.1:3000:3000 \
  -v /path/to/your/vault:/var/obsidian-vault \
  ghcr.io/epistates/turbovault:latest \
  --transport http --host 0.0.0.0 --port 3000
```

There is no HTTP health endpoint; have the orchestrator probe the port (a
Kubernetes `tcpSocket` probe, for example).

## Docker Compose

`docker-compose.yml` in the repository builds the image from source and mounts
the small example vault in `examples/docker-vault`. Change the volume to point
at your own vault:

```bash
docker compose build
docker compose run --rm -T turbovault
```

## Building the image yourself

```bash
docker build -t turbovault:local .
```

The Dockerfile pins its base images by digest and builds with `--locked`, so a
rebuild of the same commit uses the same toolchain and dependencies.

## Verifying an image

Release images carry signed build provenance:

```bash
gh attestation verify oci://ghcr.io/epistates/turbovault:latest --repo Epistates/turbovault
```
