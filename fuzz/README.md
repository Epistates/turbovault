# Fuzzing

Coverage-guided fuzz targets (cargo-fuzz, libFuzzer) for the code that reads
untrusted note text. The crate is its own workspace and needs a nightly
toolchain; the main build never compiles it.

| Target | What it drives |
|---|---|
| `parse_blocks` | `parse_blocks` and `to_plain_text`, the block parser behind outlines and grounding |
| `parse_note` | The full per-file parse a vault runs, asserting every link span is a span of the note |
| `frontmatter` | `parse_frontmatter_yaml` on arbitrary YAML |
| `edit_blocks` | `EditEngine` parsing and applying SEARCH/REPLACE blocks, fuzzy matching included (input: note, NUL, edits) |
| `rewrite_links` | The link rewriter a note move and delete run, with arbitrary note paths |

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run parse_note fuzz/corpus/parse_note fuzz/seeds/markdown -- -max_total_time=60
```

The first corpus directory collects new inputs; `seeds/` holds the hand-written
starting points and is never written to. A crash leaves its input under
`fuzz/artifacts/<target>/`; replay it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`, then add the
minimized case as a regular test next to the code it breaks.

The scheduled workflow (`.github/workflows/scheduled.yml`) runs every target
for five minutes each night and keeps the corpus in the Actions cache.
