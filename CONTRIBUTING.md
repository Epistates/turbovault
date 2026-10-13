# Contributing to TurboVault

Thanks for helping. Bug reports, fixes, docs and new tools are all welcome.
For anything larger than a fix, open an issue first so we can agree on the
shape before you write it.

## Setup

You need Rust (the crates build on 1.90; the test suite needs 1.93.1 or newer,
since `serial_test` 4 does), plus `cmake` and a C compiler for the vendored
libgit2. [just](https://github.com/casey/just) runs the common recipes.

```sh
just build        # cargo build
just test         # fmt check, clippy, every test
just test-quick   # tests only
```

## Before you open a pull request

CI runs all of these; running them first saves a round trip.

```sh
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --all-features --doc
```

- **Add a test with every fix,** one that fails without it. Tests sit next to
  the code in `#[cfg(test)]` modules, or under each crate's `tests/`.
- **Add a CHANGELOG entry** under `## [Unreleased]`, written for someone using
  TurboVault: what was wrong or what's new, and what they'll notice.
- **Keep public APIs compatible.** CI runs `cargo semver-checks` against the
  latest release tag. A breaking change needs a major version and a note in
  the changelog.

## Snapshots and fixtures

Some tests compare against a committed snapshot. When a change is meant to
alter one, re-bless it and review the diff like any other change:

```sh
# The MCP tool catalog (names, descriptions, input schemas)
UPDATE_TOOL_CATALOG=1 cargo test -p turbovault --lib

# The parser's output contract on its fixture note
UPDATE_PARSER_SNAPSHOT=1 cargo test -p turbovault-parser --test test_contract_snapshot
```

## The write-safety matrix

`crates/turbovault/tests/wss_matrix.rs` holds the clobber-safety behavior every
write must have, cell by cell (see `docs/write-safety-suite/README.md`). Cells
the code doesn't meet yet are `pending`. CI's ratchet fails when an active
cell fails, when a pending cell passes in every world and backend (activate
it), or when the number of pending trials rises above
`docs/write-safety-suite/pending-baseline`. Run it locally with:

```sh
python3 scripts/wss-report.py --ratchet docs/write-safety-suite/pending-baseline
```

Lower the baseline when you activate cells. Raising it means adding a known
gap, so say why in the pull request.

## Fuzzing, property tests and benchmarks

- `fuzz/` has cargo-fuzz targets for the parsers, the edit engine and the link
  rewriter (nightly toolchain, see `fuzz/README.md`). They run nightly in CI.
- Property tests (`proptest_*.rs` under `tests/`) run with the normal suite.
- `cargo bench -p turbovault --bench vault_10k` measures a 10,000-note vault;
  `docs/development/building.md` shows how to compare two runs.

## Commits

Commit subjects follow the repository's existing style: a type prefix and a
plain description of the change, such as `fix: rewrite reference links on move`
or `ci: gate semver on every published crate`. Types in use: `feat`, `fix`,
`test`, `docs`, `ci`, `bench`, `fuzz`, `release`, `chore`, with an optional
scope, `fix(parser): ...`. The body says why, in prose.

## Security issues

Please don't open a public issue for a vulnerability. See
[SECURITY.md](SECURITY.md).

By contributing you agree your work is licensed under the [MIT License](LICENSE)
and that you'll follow the [Code of Conduct](CODE_OF_CONDUCT.md).
