# Docker

Each release publishes a multi-arch image (linux/amd64 and linux/arm64) to GitHub Container Registry, built with the same features as the prebuilt binaries: every transport, SQL, and the compiled-in plugins (off until `--plugins` names them).

```bash
docker pull ghcr.io/epistates/turbovault:4      # or 4.0, or 4.0.0
```

The image runs as a non-root user (UID 1000) and starts `turbovault --profile production --init`, serving the vault mounted at `/var/obsidian-vault`. Arguments after the image name are added to the server's own.

## Over stdio (MCP clients)

MCP clients talk to the server over stdin and stdout, so run it with `-i` and without a TTY:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "/path/to/your/vault:/var/obsidian-vault",
        "ghcr.io/epistates/turbovault:4"
      ]
    }
  }
}
```

## Over HTTP

Inside a container the server has to listen on `0.0.0.0` to be reachable, and TurboVault refuses to listen beyond loopback without authentication. Give it bearer tokens (or JWT settings; see the [configuration guide](../configuration/index.md#security)):

```bash
docker run -d --name turbovault -p 127.0.0.1:3000:3000 \
  -v /path/to/your/vault:/var/obsidian-vault \
  -v /etc/turbovault/tokens:/run/secrets/tokens:ro \
  ghcr.io/epistates/turbovault:4 \
  --transport http --host 0.0.0.0 --port 3000 \
  --auth-mode bearer --auth-token-file /run/secrets/tokens
```

If a reverse proxy in front already authenticates every request, use `--allow-unauthenticated-network` instead, and publish the port only to that proxy.

There's no HTTP health endpoint; probe the port (a Kubernetes `tcpSocket` probe, for example) or send an MCP `ping`.

## Docker Compose

`docker-compose.yml` in the repository builds the image from source and mounts the small example vault in `examples/docker-vault`. Change the volume to point at your own vault:

```bash
docker compose build
docker compose run --rm -T turbovault
```

## Building the image yourself

```bash
docker build -t turbovault:local .
```

The Dockerfile pins its base images by digest and builds with `--locked`, so a rebuild of the same commit uses the same toolchain and dependencies.

## Verifying an image

Release images carry signed build provenance:

```bash
gh attestation verify oci://ghcr.io/epistates/turbovault:4 --repo Epistates/turbovault
```
