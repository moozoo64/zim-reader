# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this repo is

`zim-reader` — a pure-Rust, read-only library for ZIM archive files. Single-crate workspace under [crates/zim-reader/](crates/zim-reader/). The authoritative format reference and phased implementation plan live in [docs/zim-reader-spec.md](docs/zim-reader-spec.md); the spec is the source of truth when the code and the spec disagree.

## Verification commands

Run all three before declaring work done:

```bash
cargo test -p zim-reader
cargo clippy -p zim-reader --all-targets --all-features -- -D warnings
cargo fmt --check
```

Clippy is run with `-D warnings` — do not introduce new warnings. `cargo fmt` (no `--check`) is fine to apply before the check.

## Repo conventions

- **No binary fixtures in git.** Synthetic ZIMs are built in-process inside `#[cfg(test)]` helpers (see `build_zim_full` in [crates/zim-reader/src/archive.rs](crates/zim-reader/src/archive.rs)). Compressed test clusters are encoded at test time with the `xz2` / `zstd` dev-dependencies.
- **Pure-Rust decoders only on the default path.** `compression-pure` (default feature) pulls in `lzma-rs` and `ruzstd`. Do not add native codec deps as non-optional dependencies.
- **Public API shape is deliberately narrow.** Prefer adding helpers as `pub(crate)` first; only re-export through [lib.rs](crates/zim-reader/src/lib.rs) when the spec's public surface calls for it.
- **Bounds-check everything.** All reads off the mmap go through [util.rs](crates/zim-reader/src/util.rs) helpers that return `Error::OffsetOutOfBounds` with a field name. New code that reads slices should do the same rather than indexing directly.
- **`Archive` must stay `Send + Sync`.** The cluster cache is `Mutex<LruCache<u32, Arc<ClusterData>>>` for this reason. Don't introduce `Rc` or non-`Sync` interior mutability.
- **No `unsafe`.** The `memmap2::Mmap` is the boundary; everything above it is safe Rust.

## Error handling

The `Error` enum in [error.rs](crates/zim-reader/src/error.rs) is the vocabulary. Reuse existing variants before adding new ones — `UnknownCompression`, `BlobOutOfRange`, `RedirectLoop`, `RedirectIndexOutOfRange`, `ExtendedClusterInV5`, `LzmaDecompress`, `ZstdDecompress`, `InvalidUtf8`, `OffsetOutOfBounds` already cover most cases. If a new variant is genuinely needed, update the spec alongside the code.

## Phased implementation

The spec lays out phases; work is landing phase-by-phase.

- Phase 1 (header, MIME, namespace) — **done**
- Phase 2 (dirent parsing, binary search, iteration) — **done**
- Phase 3 (cluster decompression, blob extraction, `get_article`/`main_page`/`metadata`, LRU cache) — **done**
- Phase 4 (MD5 checksum verification, `compression-native` feature, fuzz targets, split archives) — not started
- Phase 5 (repo polish: license files, CI workflow, CHANGELOG) — not started

When starting a new phase, first write a plan in `~/.claude/plans/` and get it approved before implementing.

## Testing style

- Unit tests live in `#[cfg(test)] mod tests` at the bottom of each module.
- Integration-style tests (anything that needs a full synthetic archive) live in `archive.rs` alongside the `build_zim_full` helper — not in a separate `tests/` directory yet.
- Tests should exercise both v5 and v6 archives where namespace conventions differ (metadata namespace `-` vs `M`, etc.).
- Prefer asserting on specific `Error` variants with `matches!` rather than stringifying the error.

## Small things that have bitten us

- The ZIM spec document in this repo has an off-by-one in the cluster blob-count formula. The openZIM wiki is correct: `n_blobs = (first_offset / OFFSET_SIZE) - 1`. [cluster.rs](crates/zim-reader/src/cluster.rs) follows the wiki.
- The last cluster's compressed end is not stored explicitly — use `header.checksum_pos` as the implicit end offset when computing `cluster_ptr[n] .. cluster_ptr[n+1]` for the final cluster.
- Header `main_page` is stored as `0xFFFFFFFF` when absent and decoded to `Option<u32>`. A `Some(idx)` that points at a deprecated dirent is still a valid archive and should return `Ok(None)` from `main_page()`, not an error.
- v5 archives must reject the cluster info extended-offset bit (`0x10`) — that bit is reserved on v5 and only meaningful on v6.

## What not to do here

- Don't add backwards-compatibility shims for API shapes that haven't shipped.
- Don't introduce a separate `tests/` directory until there's a concrete reason the in-module tests can't cover it.
- Don't commit anything into `samples/` — it's gitignored for a reason (real ZIMs are hundreds of MB to multiple GB).
- Don't bypass `cargo fmt` or `clippy -D warnings` to land a change faster.
