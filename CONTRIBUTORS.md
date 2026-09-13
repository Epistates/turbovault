# Contributors

TurboVault is built by people who did not have to. Thank you.

## Core

**[@dlobue](https://github.com/dlobue)** — the Git write substrate and the single mutation
chokepoint that everything now writes through, atomic `CreateOnly` on both backends, honest
reporting for partially-applied batches, one activation path for publishing a vault manager, and
write-backend selection at registration. Also the clobber-safety matrix, which is the reason we can
say what the write surface guarantees rather than hoping.

**[@ForrestThump](https://github.com/ForrestThump)** — most of the shape of the plugin
architecture, argued out across [#33](https://github.com/Epistates/turbovault/issues/33),
[#42](https://github.com/Epistates/turbovault/issues/42) and
[#43](https://github.com/Epistates/turbovault/issues/43) before any of it was written. Write
provenance at the chokepoint, Obsidian Tasks metadata parsing, cache-first `query_metadata` (10.6x),
tool filtering by tag, and the Windows test fixes that made CI mean something on more than one
platform. Also a long run of work on [turbomcp](https://github.com/Epistates/turbomcp).

## Contributors

- **[@Helfrid](https://github.com/Helfrid)** — partial reads for `read_note`, line slices and
  heading sections, including the call to withhold the content hash from a partial read so it can
  never satisfy a whole-file overwrite.
- **[@AntttMan](https://github.com/AntttMan)**
- **[@esumerfd](https://github.com/esumerfd)**
- **[@hexatriene](https://github.com/hexatriene)**
- **[@slikts](https://github.com/slikts)**
- **[@ttfoley](https://github.com/ttfoley)**
- **[@wingrunr21](https://github.com/wingrunr21)**

## Reporters

Bug reports that came with a repro and saved us the guessing:

- **[@tiborkiss](https://github.com/tiborkiss)** — non-conformant `$defs` placement in tool schemas
  ([#51](https://github.com/Epistates/turbovault/issues/51)), traced upstream to `turbomcp-macros`
  with a minimal curl repro against llama.cpp.

---

If your work is here and you would rather be listed differently, or not at all, open an issue and we
will fix it. If it is missing, that is our mistake, so please tell us.
