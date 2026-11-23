# Releasing

Versions follow [semver](https://semver.org). Between releases, `main`
collects changes while the workspace version still names the last release,
so CI's **Semver Check (advisory)** job reports on every pull request what the
next version has to be, against the latest release tag, without failing. Read
it in the job summary when choosing the next version: `semver requires new
major version` on any crate means the next release is a major one.

At release time the check is enforced: the release workflow's verify job runs
`cargo semver-checks` against the previous release tag, and a tag whose
version bump is too small for what changed fails before anything is built or
published. To see the verdict before tagging:

```sh
cargo semver-checks --workspace --baseline-rev "$(git describe --tags --abbrev=0 --match 'v*')"
```

1. **Prepare the release PR.** Choose the version from the semver report.
   Bump `version` in the root `Cargo.toml` (and the
   workspace crates' entries under `[workspace.dependencies]`), date the
   `## [Unreleased]` section of `CHANGELOG.md` as `## [X.Y.Z] - YYYY-MM-DD`, and
   set the three versions in `server.json` (`version`, the cargo package
   `version`, and the OCI image tag). CI fails if `server.json` disagrees with
   `Cargo.toml`.
2. **Dry-run the publish** from the release branch:
   `cargo publish --workspace --locked --dry-run`, excluding any independently
   versioned crate whose version is already on crates.io.
3. **Merge, then tag the merge commit:** `git tag -a vX.Y.Z <sha> -m vX.Y.Z` and
   push the tag. The release workflow verifies, builds the seven binaries and
   smoke-tests the ones the runners can execute. Once the builds pass, its
   publish job puts every crate whose version isn't on crates.io yet there,
   through Trusted Publishing (below). Only after that does it tag the
   multi-arch image at `ghcr.io/epistates/turbovault` and create the GitHub
   Release with the archives, the SBOM and checksums, and provenance
   attestations for the archives and the image.
4. **Check crates.io.** Every crate should show the new version. If the
   publish job failed, read why: a crate missing its trusted publisher fails
   the token exchange or its upload. Neither the image tags nor the GitHub
   Release exist yet. Fix the cause and re-run the failed jobs; the publish
   job skips the crates that already landed, and the image and the release
   follow.
5. **Publish to the MCP Registry** once the crate and image are live, with
   [mcp-publisher](https://github.com/modelcontextprotocol/registry):

   ```sh
   mcp-publisher login github
   mcp-publisher publish
   ```

   The registry checks ownership against the `mcp-name:` line in
   `crates/turbovault/README.md` (as rendered on crates.io) and the
   `io.modelcontextprotocol.server.name` label on the image.

## Trusted Publishing

The publish job holds no crates.io token. It asks GitHub for an OIDC token,
and `rust-lang/crates-io-auth-action` exchanges that for a crates.io publish
token that lasts 30 minutes and is revoked when the job ends. crates.io issues
it only if the crate trusts this exact repository, workflow file and GitHub
environment.

Each crate is configured on its own, and only an owner of the crate can do it.
For every published crate in the workspace (`cargo metadata --no-deps
--format-version 1 | jq -r '.packages[].name'`), open
`https://crates.io/crates/<name>/settings`, choose **Trusted Publishing**, then
**Add**, then **GitHub**, and enter:

| Field | Value |
|---|---|
| Repository owner | `Epistates` |
| Repository name | `turbovault` |
| Workflow filename | `release.yml` |
| Environment | `release` |

The job runs in the `release` environment, which GitHub creates the first time
the job runs. Restricting that environment to `v*` tags in the repository's
environment settings means no other ref can ever obtain the token.

A crate has to exist on crates.io before it can trust a publisher, so a new
workspace crate's first version is published by an owner with
`cargo publish -p <name>`, and its trusted publisher added afterwards. Until
it is, the release's publish job fails.
