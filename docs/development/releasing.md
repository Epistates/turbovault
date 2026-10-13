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
   push the tag. The release workflow verifies, builds the seven binaries,
   smoke-tests the ones the runners can execute, publishes the multi-arch
   image to `ghcr.io/epistates/turbovault`, attaches the SBOM and checksums, and
   signs provenance attestations for the archives and the image.
4. **Publish the crates** (`cargo publish --workspace --locked` with the same
   exclusions) if the workflow's publish job has no `CARGO_REGISTRY_TOKEN`.
5. **Publish to the MCP Registry** once the crate and image are live, with
   [mcp-publisher](https://github.com/modelcontextprotocol/registry):

   ```sh
   mcp-publisher login github
   mcp-publisher publish
   ```

   The registry checks ownership against the `mcp-name:` line in
   `crates/turbovault/README.md` (as rendered on crates.io) and the
   `io.modelcontextprotocol.server.name` label on the image.
