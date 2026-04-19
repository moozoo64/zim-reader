# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial public release of `zim-reader`.
- ZIM v5 and v6 archive support: header parse, MIME list, namespace detection.
- Directory entry parsing (content + redirect) with binary search by path
  and by title, plus prefix iteration.
- Cluster decompression via pure-Rust LZMA2 (`lzma-rs`) and Zstandard
  (`ruzstd`); uncompressed clusters handled directly.
- Blob extraction for both standard (u32) and extended (u64) offsets.
- `Archive::get_article`, `main_page`, `metadata`, with redirect resolution
  (cycle-detected, bounded to 8 hops).
- LRU cluster cache keyed by cluster number, behind `Mutex<LruCache<_>>`
  so `Archive` stays `Send + Sync`.
- MD5 checksum verification on open, streamed in 4 MB chunks. Opt out via
  `ArchiveOptions { verify_checksum: VerifyChecksum::Skip, .. }`.
- `compression-pure` feature (default) selecting pure-Rust decoders.
- Integration tests against the [openzim/zim-testing-suite] v5 and v6
  `small.zim` fixtures, wired in as a shallow git submodule at
  `crates/zim-reader/tests/fixtures/`. Tests skip gracefully when the
  submodule is not initialised.

[openzim/zim-testing-suite]: https://github.com/openzim/zim-testing-suite
