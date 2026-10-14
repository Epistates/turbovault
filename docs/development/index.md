# Development guide

See [CONTRIBUTING.md](../../CONTRIBUTING.md) for the contribution process. [CLAUDE.md](../../CLAUDE.md) is the detailed architecture guide.

## Requirements

- Rust 1.90 or later to build the crates (edition 2024); 1.93.1 or later to run the test suite (`serial_test`).
- `just` for the shortcuts below (optional; each is a cargo command).
- Nightly Rust and `cargo-fuzz` for fuzzing.

## Everyday commands

```bash
just build                 # cargo build
just test                  # fmt check, clippy, the whole suite
just test-quick            # tests only
just test-one module::name # one test
cargo run -- --vault /path/to/vault --init
```

Before opening a pull request, run what CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo test --workspace --all-features
cargo machete
cargo deny check && cargo audit --deny warnings
```

CI also checks every feature on its own, builds the release feature set on Linux, macOS and Windows, keeps line coverage above 90%, and ratchets the write-safety suite's pending cells. Clippy runs on current stable, so keep yours current.

## Tests

- **Unit tests** sit next to the code; **integration tests** are in each crate's `tests/`.
- **Protocol tests**: `crates/turbovault/tests/test_mcp_wire_e2e.rs` and `test_mcp_surface_wire.rs` run the real binary over stdio; `test_network_transports.rs` covers HTTP, WebSocket and TCP.
- **Tool contract tests**: `test_tool_output_schemas.rs` calls every tool and validates its result against its `outputSchema`; `test_tool_catalog_quality.rs` requires descriptions on every parameter and complete annotations.
- **Write safety**: `wss_matrix.rs` and `write_safety_suite.rs`; see the [write-safety suite](../write-safety-suite/README.md).
- **Property tests**: `turbovault-parser/tests/proptest_parser.rs`, `turbovault-tools/tests/proptest_rewriter.rs`, `turbovault-vault/tests/proptest_edit.rs`. Raise the case count with `PROPTEST_CASES=4096`.

Tests use real temporary directories and Git repositories, no mocks.

## Snapshots

```bash
# The tool catalog: every tool's name, description and input and output schemas
UPDATE_TOOL_CATALOG=1 cargo test -p turbovault --lib

# The parser's output on a fixture covering the whole syntax
UPDATE_PARSER_SNAPSHOT=1 cargo test -p turbovault-parser --test test_contract_snapshot
```

Review the diff before committing a re-blessed snapshot.

## Fuzzing

The `fuzz/` crate (outside the workspace) has five targets: `parse_note`, `parse_blocks`, `frontmatter`, `edit_blocks` and `rewrite_links`, seeded from the test fixtures.

```bash
cd fuzz
cargo +nightly fuzz run parse_note -- -max_total_time=60
```

A scheduled workflow runs each target nightly. See [fuzz/README.md](../../fuzz/README.md).

## Benchmarks

```bash
cargo bench -p turbovault --bench vault_10k        # 10,000-note vault
cargo bench -p turbovault --bench tool_benchmarks
cargo bench -p turbovault-git --bench vault_size   # Git writes by vault size
```

See [Building from source](building.md#benchmarking) for comparing runs.

## Documentation

```bash
cargo doc --workspace --no-deps --open
```

Crates with `#![warn(missing_docs)]` in their `lib.rs` fail CI on an undocumented public item, since CI turns warnings into errors. Document every public item you add.

## Adding a tool

1. Add the method to the provider that owns its group in `crates/turbovault/src/tools/providers/`, with `#[tool(...)]`: a description that says when to use it rather than a sibling, a `title`, all four annotation hints (`read_only`, `destructive`, `idempotent`, `open_world`) and `#[description]` on every parameter.
2. Put the logic in `turbovault-tools` or a domain crate, and return a typed result that derives `JsonSchema` behind the crate's `schemars` feature, so the tool advertises an `outputSchema`.
3. Follow the conventions: `limit`/`offset` and `meta.pagination` for lists, errors that map to an `ErrorCategory`, vault-relative paths. The `vault` argument is added for you.
4. Add fixture coverage in `test_tool_output_schemas.rs`, decide whether the tool belongs in `tool_visibility::ADVANCED_TOOLS`, and re-bless the catalog.

## Writing to the vault

Never write files directly. Build a `ChangePlan` and call `VaultManager::apply_changes`, which enforces `allowed_operations` and the protected-path policy and dispatches to the vault's backend. Serve derived state (search, graph, similarity) only after `ensure_fresh`.
