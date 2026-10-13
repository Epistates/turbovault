# Releasing

1. **Prepare the release PR.** Bump `version` in the root `Cargo.toml` (and the
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
