#!/bin/bash
set -e

echo "Publishing turbovault v1.1.5 to crates.io"
echo "=========================================="
echo ""
echo "⚠️  Make sure you have:"
echo "  1. Committed all changes"
echo "  2. Run 'cargo test --workspace --all-features' successfully"
echo "  3. Logged in with 'cargo login'"
echo ""
read -p "Continue? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]
then
    echo "Aborted."
    exit 1
fi

echo ""
echo "Starting publication process..."
echo ""

# 1. Core
echo "📦 [1/8] Publishing turbovault-core..."
cargo publish -p turbovault-core
echo "✓ turbovault-core published"
echo "⏳ Waiting 120 seconds for crates.io to index..."
sleep 10

# 2. Domain crates (parallel)
echo ""
echo "📦 [2/8] Publishing turbovault-parser..."
cargo publish -p turbovault-parser
echo "✓ turbovault-parser published"
sleep 5

echo ""
echo "📦 [3/8] Publishing turbovault-graph..."
cargo publish -p turbovault-graph
echo "✓ turbovault-graph published"
sleep 5

echo ""
echo "📦 [4/8] Publishing turbovault-vault..."
cargo publish -p turbovault-vault
echo "✓ turbovault-vault published"
sleep 5

echo ""
echo "📦 [5/8] Publishing turbovault-batch..."
cargo publish -p turbovault-batch
echo "✓ turbovault-batch published"
sleep 5

echo ""
echo "📦 [6/8] Publishing turbovault-export..."
cargo publish -p turbovault-export
echo "✓ turbovault-export published"

echo "⏳ Waiting 120 seconds for crates.io to index domain crates..."
sleep 5

# 3. Tools
echo ""
echo "📦 [7/8] Publishing turbovault-tools..."
cargo publish -p turbovault-tools
echo "✓ turbovault-tools published"
echo "⏳ Waiting 120 seconds for crates.io to index..."
sleep 10

# 4. Binary
echo ""
echo "📦 [8/8] Publishing turbovault (binary)..."
cargo publish -p turbovault
echo "✓ turbovault published"

echo ""
echo "=========================================="
echo "✅ All crates published successfully!"
echo ""
echo "🔗 Verify at: https://crates.io/crates/turbovault"
echo ""
echo "Next steps:"
echo "  1. git tag v1.1.5"
echo "  2. git push origin v1.1.5"
echo "  3. Create GitHub release at https://github.com/epistates/turbovault/releases/new"
echo ""
