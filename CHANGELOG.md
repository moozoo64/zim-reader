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
- `zim-info` debug bin crate under `tools/zim-info/` that prints an
  archive's header, MIME list, and counts. `publish = false`; intended
  for manual inspection of fixtures and real-world archives.
- Error-path integration tests exercising six `invalid.*.zim` fixtures
  from the zim-testing-suite submodule, covering truncated headers,
  out-of-bounds dirent pointers, misaligned and out-of-range cluster
  offsets, bad MIME indices, and unsorted dirent tables. Tests use
  `VerifyChecksum::Skip` since the fixtures are modified without
  updating their stored MD5.

### Changed

- All public enums and structs marked `#[non_exhaustive]` to preserve the
  freedom to add variants and fields without a semver-major bump. Downstream
  crates must construct `ArchiveOptions` via `Default::default()` plus field
  assignment; exhaustive matches on `Error` must add a wildcard arm. Affected
  types: `Error`, `ArchiveOptions`, `VerifyChecksum`, `Header`, `Dirent`,
  `ContentEntry`, `RedirectEntry`, `Article`, `Namespace`, `NamespaceMode`.
- Bumped to Rust edition 2024 and MSRV 1.95 (current stable). The
  immediate driver was transitive deps (`lzma-rs → crc 3.4.0` needs
  rustc 1.83+; `ruzstd 0.8.2` uses `u*::is_multiple_of`, stabilised
  in 1.87); aligning on current stable avoids chasing the next one.
- Dependency bumps: `thiserror` 1 → 2, `lru` 0.12 → 0.17, `md-5` 0.10 →
  0.11, `ruzstd` 0.7 → 0.8. `ruzstd::StreamingDecoder` moved under
  `ruzstd::decoding::`.

[openzim/zim-testing-suite]: https://github.com/openzim/zim-testing-suite
