# zim-reader

A pure-Rust, read-only library for [ZIM archive files] — the offline content format used by [Kiwix] for offline Wikipedia, Wiktionary, Stack Exchange, and other wiki-style corpora.

`zim-reader` opens a ZIM file, parses its header and indexes, and serves articles and metadata on demand. It is designed as a building block for higher-level tools (search, MCP servers, CLI readers) that need offline ZIM access without shelling out to `libzim`.

## Status

Early, usable. Phase 4 of the implementation plan is complete:

- [x] Header, MIME list, and namespace detection (v5 and v6)
- [x] Directory-entry parsing (content + redirect)
- [x] Binary search by path and by title; prefix iteration
- [x] Cluster decompression (uncompressed, LZMA2/XZ, Zstandard) — pure Rust
- [x] Blob extraction (standard u32 and extended u64 offsets)
- [x] `get_article`, `main_page`, `metadata`, redirect resolution with cycle detection
- [x] LRU cluster cache
- [x] MD5 checksum verification (streamed, opt-out via `VerifyChecksum::Skip`)
- [ ] Optional native codecs (`xz2`, `zstd` as runtime deps) (future phase)
- [ ] Split-archive read support beyond detection (future phase)

## Quick start

```toml
[dependencies]
zim-reader = "0.1"
```

```rust
use zim_reader::Archive;

let archive = Archive::open("wikipedia.zim")?;

println!(
    "version {}.{}, {} MIME types, {} entries, {} clusters",
    archive.header().major_version,
    archive.header().minor_version,
    archive.mime_types().len(),
    archive.entry_count(),
    archive.cluster_count(),
);

if let Some(article) = archive.main_page()? {
    if let Some(html) = article.as_text() {
        println!("{}", &html[..html.len().min(200)]);
    }
}

if let Some(article) = archive.get_article("A/Rust_(programming_language)")? {
    println!("{} bytes, mime: {}", article.data.len(), article.mime_type(&archive));
}

if let Some(title) = archive.metadata("Title")? {
    println!("archive title: {title}");
}
# Ok::<(), zim_reader::Error>(())
```

## Features

| Feature             | Default | Description                                              |
|---------------------|---------|----------------------------------------------------------|
| `compression-pure`  | yes     | Pure-Rust LZMA2 and Zstandard decoders (`lzma-rs`, `ruzstd`) |

Native codecs (`xz2`, `zstd`) land as a non-default `compression-native` feature in a future phase.

## Design notes

- **Memory-mapped I/O.** `Archive` holds a [`memmap2::Mmap`] of the file. Reads are slice operations against the mapped region; nothing is copied until a cluster is decompressed.
- **Pointer lists are not eagerly materialized.** `Archive` remembers only where each list starts in the file; lookups index into the mapping directly. This keeps open cost O(header) rather than O(entry_count).
- **Binary search follows the spec.** Path lookups walk the path-sorted pointer list; title lookups walk the title-sorted pointer list, which references the path list. Both stop at namespace boundaries.
- **Redirect resolution is bounded.** Chains are followed up to `MAX_REDIRECT_DEPTH = 8` with cycle detection via a visited set.
- **Cluster cache.** `LruCache<u32, Arc<ClusterData>>` behind a `Mutex`. Size is configurable via `ArchiveOptions::cluster_cache_size`. `Archive` remains `Send + Sync`.
- **No `unsafe`.** The crate forbids `unsafe` outside the mmap crate boundary.

## Crate layout

```
crates/zim-reader/
├── src/
│   ├── lib.rs          — public re-exports
│   ├── archive.rs      — Archive, open/options, get_blob/get_article/main_page/metadata
│   ├── article.rs      — Article (bytes + resolved entry)
│   ├── cluster.rs      — ClusterInfo, decompress, extract_blob
│   ├── dirent.rs       — Dirent / ContentEntry / RedirectEntry parsing
│   ├── header.rs       — 80-byte header
│   ├── mime.rs         — MIME type list parsing
│   ├── namespace.rs    — v5 vs v6 namespace conventions
│   ├── pointer_list.rs — path/title/cluster pointer helpers
│   ├── error.rs        — Error enum
│   └── util.rs         — bounded little-endian reads, C-string decode
└── Cargo.toml
```

The spec this implementation follows lives in [docs/zim-reader-spec.md](docs/zim-reader-spec.md).

## Development

```bash
# Build
cargo build

# Test (currently ~88 unit + integration tests, all synthetic)
cargo test

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Format
cargo fmt --check
```

All tests build synthetic ZIM archives in-process — no fixture files are checked into the repo. LZMA2 and Zstd test clusters are encoded at test time with `xz2` and `zstd` dev-dependencies.

## License

Dual-licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.

[ZIM archive files]: https://wiki.openzim.org/wiki/ZIM_file_format
[Kiwix]: https://www.kiwix.org/
[`memmap2::Mmap`]: https://docs.rs/memmap2/
