# Publishing Guide

This guide explains how to publish TurboVault crates to crates.io.

## Current Version: 1.1.6

All crates are versioned together using workspace-level version management.

## Pre-Publishing Checklist

- [x] Version updated to 1.1.6 in `Cargo.toml` (workspace.package.version)
- [x] All tests passing (`cargo test --workspace --all-features`)
- [x] Release build successful (`cargo build --release`)
- [x] CLAUDE.md updated with correct TurboMCP version
- [ ] CHANGELOG.md updated (if exists)
- [ ] All changes committed to git
- [ ] Git tag created: `git tag v1.1.6`

## Publishing Order

Crates must be published in dependency order. **IMPORTANT**: Wait for each crate to be available on crates.io before publishing the next one (usually takes 1-2 minutes).

### 1. Core Foundation
```bash
cargo publish -p turbovault-core
```
**Wait 2-3 minutes for crates.io to index**

### 2. Domain Crates (can be published in parallel after turbovault-core is live)
```bash
# These can be published simultaneously (no dependencies on each other)
cargo publish -p turbovault-parser
cargo publish -p turbovault-graph
cargo publish -p turbovault-vault
cargo publish -p turbovault-batch
cargo publish -p turbovault-export
```
**Wait 2-3 minutes for crates.io to index all of them**

### 3. Tools Layer
```bash
cargo publish -p turbovault-tools
```
**Wait 2-3 minutes for crates.io to index**

### 4. Binary Crate
```bash
cargo publish -p turbovault
```

## Dependency Graph

```
turbovault-core (no turbovault deps)
    ├── turbovault-parser
    ├── turbovault-graph
    ├── turbovault-vault
    ├── turbovault-batch
    └── turbovault-export
            └── turbovault-tools
                    └── turbovault (binary)
```

## Publishing Script

For convenience, here's a script that publishes all crates in the correct order with appropriate delays:

```bash
#!/bin/bash
set -e

echo "Publishing turbovault v1.1.6 to crates.io"
echo "=========================================="

# 1. Core
echo "📦 Publishing turbovault-core..."
cargo publish -p turbovault-core
echo "⏳ Waiting 120 seconds for crates.io to index..."
sleep 120

# 2. Domain crates (parallel)
echo "📦 Publishing domain crates..."
cargo publish -p turbovault-parser &
cargo publish -p turbovault-graph &
cargo publish -p turbovault-vault &
cargo publish -p turbovault-batch &
cargo publish -p turbovault-export &
wait
echo "⏳ Waiting 120 seconds for crates.io to index..."
sleep 120

# 3. Tools
echo "📦 Publishing turbovault-tools..."
cargo publish -p turbovault-tools
echo "⏳ Waiting 120 seconds for crates.io to index..."
sleep 120

# 4. Binary
echo "📦 Publishing turbovault (binary)..."
cargo publish -p turbovault

echo "✅ All crates published successfully!"
echo "🔗 Check status at: https://crates.io/crates/turbovault"
```

Save as `scripts/publish.sh`, make executable with `chmod +x scripts/publish.sh`, and run.

## Verification

After publishing, verify each crate at:
- https://crates.io/crates/turbovault-core
- https://crates.io/crates/turbovault-parser
- https://crates.io/crates/turbovault-graph
- https://crates.io/crates/turbovault-vault
- https://crates.io/crates/turbovault-batch
- https://crates.io/crates/turbovault-export
- https://crates.io/crates/turbovault-tools
- https://crates.io/crates/turbovault

## Post-Publishing

1. **Tag the release in git:**
   ```bash
   git tag v1.1.6
   git push origin v1.1.6
   ```

2. **Create GitHub release:**
   - Go to https://github.com/epistates/turbovault/releases/new
   - Select tag `v1.1.6`
   - Title: `TurboVault v1.1.6`
   - Add release notes describing changes

3. **Update documentation:**
   - Update installation instructions in README.md if needed
   - Update docs.rs links to point to new version

## Troubleshooting

### "already uploaded" error
If a crate was already published, skip it and continue with the next one.

### Dependency version errors
If you get errors about dependency versions not being found, you didn't wait long enough for crates.io to index the previous crate. Wait another minute and try again.

### Authentication errors
Make sure you're logged in to crates.io:
```bash
cargo login
```

### Uncommitted changes
If you have uncommitted changes, either:
- Commit them: `git add -A && git commit -m "Bump version to 1.1.6"`
- Or use `--allow-dirty` flag (not recommended for production releases)

## Version History

- **1.1.6** - Current version (TurboMCP 2.2.1, version cleanup and stabilization)
- **1.1.5** - Previous version (accidental release, skipped)
- **1.1.4** - Prior version (TurboMCP 2.1.0, improved features)
- **0.1.3** - Initial public release series
- **0.1.2** - Initial public release
- **0.1.1** - Early release

## Notes

- All crates share the same version number (workspace versioning)
- The `publish = true` flag is set at the workspace level
- Each crate includes the workspace README.md and LICENSE
- Binary size: ~7.2 MB (release build with default features)
