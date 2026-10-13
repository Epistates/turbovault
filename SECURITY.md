# Security Policy

## Supported versions

Security fixes land on the latest minor release of the current major version.
Older releases are not patched; upgrade to the latest release to pick up a fix.

| Version | Supported |
|---|---|
| 3.x (latest minor) | Yes |
| Older | No |

## Reporting a vulnerability

Please report vulnerabilities privately, not in a public issue or pull request.

- **Preferred:** GitHub's private vulnerability reporting, at
  <https://github.com/Epistates/turbovault/security/advisories/new>.
- **Email:** nick@epistates.com, with "TurboVault security" in the subject.

Include what you can of: the TurboVault version and how it was installed, the
transport (stdio, HTTP, WebSocket, TCP, Unix socket), the configuration
profile, the tool calls or input that trigger the issue, and what an attacker
gains. A minimal vault that reproduces it helps most.

We'll acknowledge the report, keep you updated as we investigate, and agree a
disclosure date with you once a fix is ready. Fixes ship in a patch or minor
release with a GitHub security advisory and a CVE where one applies. We credit
reporters in the advisory unless you'd rather we didn't.

## Scope

TurboVault exposes a vault on disk to an AI agent, so the boundaries that
matter most are:

- **The vault boundary.** No tool call should read or write outside the
  registered vault, or inside its protected directories (`.obsidian`, `.git`,
  `.turbovault` and the configured `excluded_paths`), on either write backend.
- **Note content is untrusted.** Notes can carry text written to steer an agent
  (prompt injection). A path where note content alone leads to a write, a
  vault registration or data leaving the machine is in scope.
- **Network transports.** Anything that lets a network client act without the
  authentication the server is configured with.
- **Integrity.** A write that corrupts or silently loses a note, or an audit
  trail that can be altered without detection.

Out of scope: issues that need an attacker who already runs code as the same
OS user as the server, denial of service from a client the operator chose to
trust, and vulnerabilities in dependencies with no reachable path in TurboVault
(report those upstream; `deny.toml` documents the advisories we've triaged).

## Verifying a release

Release archives carry Sigstore-signed build provenance and an SBOM
attestation. Verify a download with the GitHub CLI:

```sh
gh attestation verify turbovault-x86_64-unknown-linux-gnu.tar.gz --repo Epistates/turbovault
```

`SHA256SUMS` on each release lists the checksum of every archive, and native
binaries embed their dependency list for `cargo audit bin`.
