# `zim-reader` — Crate Specification & Implementation Plan

> **Scope:** A standalone, pure-Rust library crate for reading ZIM archive files.  
> **Target consumer:** `zim-mcp` and any other Rust program needing offline ZIM access.  
> **Repository:** `github.com/<org>/zim-reader`

---

## Table of Contents

1. [Goals & Non-Goals](#1-goals--non-goals)
2. [Repository Layout](#2-repository-layout)
3. [Format Specification](#3-format-specification)
   - 3.1 Binary Conventions
   - 3.2 Header
   - 3.3 MIME Type List
   - 3.4 Path Pointer List
   - 3.5 Title Pointer List
   - 3.6 Directory Entries
   - 3.7 Cluster Pointer List
   - 3.8 Clusters & Blobs
   - 3.9 Namespaces
   - 3.10 Checksum
   - 3.11 Split Archives
   - 3.12 Version History & Field Renames
4. [Public API Design](#4-public-api-design)
5. [Internal Architecture](#5-internal-architecture)
6. [Error Handling](#6-error-handling)
7. [Dependencies](#7-dependencies)
8. [Testing Strategy](#8-testing-strategy)
9. [Implementation Phases](#9-implementation-phases)
10. [Performance Considerations](#10-performance-considerations)
11. [CI / Repository Standards](#11-ci--repository-standards)

---

## 1. Goals & Non-Goals

### Goals

- **Read-only.** Parsing, decompression, and random access only. No write support.
- **Pure Rust by default.** No required C dependencies. The default feature set compiles on stable Rust with `cargo build` alone.
- **Memory-efficient.** Works correctly on archives larger than available RAM by using memory-mapped I/O. Never loads the full file into a `Vec<u8>`.
- **Format-complete.** Handles both v5 and v6 ZIM files, both old and new namespace semantics, both LZMA2 and Zstandard compression, and both standard and extended clusters.
- **Ergonomic API.** Consumers should be able to open a file and retrieve an article in ≤ 5 lines of code.
- **Async-safe.** The library is synchronous internally (mmap reads). It provides a `spawn_blocking`-friendly design so Tokio consumers can isolate heavy decompression work.
- **Well-documented.** Every public item carries a doc comment. The crate has a `//! lib.rs` overview with a quick-start example.
- **Publishable.** Will be published to crates.io with a stable `0.x` API before `zim-mcp` depends on it.

### Non-Goals

- **Full-text search.** Xapian index parsing (`X` namespace) is out of scope. Title and path binary-search lookup is in scope.
- **Writing or creating ZIM files.**
- **Async I/O primitives.** The library will not use `tokio::fs` internally. It is designed for `spawn_blocking` integration.
- **Split ZIM archive stitching.** Phase 1 handles single-file archives only. Split archive support is Phase 4.
- **ZIM v4 or earlier.** These are extremely rare in the wild; version validation will reject them with a clear error.

---

## 2. Repository Layout

```
zim-reader/
├── Cargo.toml                   # workspace root
├── Cargo.lock
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── CHANGELOG.md
├── .github/
│   └── workflows/
│       ├── ci.yml               # build, test, clippy, fmt
│       └── publish.yml          # crates.io publish on tag
│
├── crates/
│   └── zim-reader/              # the library crate
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs           # crate-level docs, re-exports
│       │   ├── error.rs         # Error, Result
│       │   ├── archive.rs       # Archive — top-level entry point
│       │   ├── header.rs        # Header struct + parsing
│       │   ├── mime.rs          # MimeTable
│       │   ├── dirent.rs        # Dirent, ContentEntry, RedirectEntry
│       │   ├── cluster.rs       # Cluster, ClusterCache
│       │   ├── pointer_list.rs  # PathPtrList, TitlePtrList
│       │   ├── namespace.rs     # Namespace enum + helpers
│       │   └── util.rs          # read_cstring, read_u64_le, etc.
│       └── tests/
│           ├── fixtures/        # symlink or submodule → zim-testing-suite
│           ├── header_tests.rs
│           ├── dirent_tests.rs
│           ├── cluster_tests.rs
│           ├── search_tests.rs
│           └── integration_tests.rs
│
└── tools/
    └── zim-info/                # optional bin crate: prints ZIM metadata
        ├── Cargo.toml
        └── src/main.rs
```

---

## 3. Format Specification

All integers are **unsigned**, **little-endian**. All strings are **UTF-8**, **null-terminated** (`\0`). References below to "the spec" mean the openZIM wiki at https://wiki.openzim.org/wiki/ZIM_file_format.

---

### 3.1 Binary Conventions

| Convention | Detail |
|---|---|
| Byte order | Little-endian throughout |
| Integer signedness | All unsigned |
| String encoding | UTF-8, null-terminated (`\0`) |
| Absent optional | `0xFFFFFFFF` for u32 fields (e.g. `main_page`), `0xFFFFFFFFFFFFFFFF` for u64 |
| No EOF marker | File ends after the MD5 checksum (16 bytes at `checksum_pos`) |

---

### 3.2 Header

The header is exactly **80 bytes**, located at offset 0. `mimeListPos` always equals 80 (the header size is fixed; the MIME list immediately follows).

```
Offset  Size  Type   Field              Notes
──────  ────  ─────  ─────────────────  ──────────────────────────────────────────
0       4     u32    magic_number       Must equal 0x044D495A (little-endian bytes:
                                        5A 49 4D 04 = "ZIM\x04")
4       2     u16    major_version      5 or 6 (reject anything else)
6       2     u16    minor_version      0..3 for v6; informational only
8       16    [u8]   uuid               Archive UUID
24      4     u32    entry_count        Total directory entries
28      4     u32    cluster_count      Total clusters
32      8     u64    path_ptr_pos       File offset of Path Pointer List
                                        (was urlPtrPos before April 2024)
40      8     u64    title_ptr_pos      File offset of Title Pointer List
48      8     u64    cluster_ptr_pos    File offset of Cluster Pointer List
56      8     u64    mime_list_pos      File offset of MIME Type List
                                        Always 80 (= sizeof(header))
64      4     u32    main_page          Entry index of main page,
                                        or 0xFFFFFFFF if absent
68      4     u32    layout_page        Entry index of layout page,
                                        or 0xFFFFFFFF if absent
72      8     u64    checksum_pos       File offset of MD5 checksum (16 bytes)
                                        checksum covers bytes [0, checksum_pos)
```

**Validation on open:**
1. `magic_number == 0x044D495A`
2. `major_version == 5 || major_version == 6`
3. `mime_list_pos == 80` (sanity check)
4. `checksum_pos < file_len` and `checksum_pos + 16 == file_len`
5. `path_ptr_pos`, `title_ptr_pos`, `cluster_ptr_pos` are all within file bounds

---

### 3.3 MIME Type List

**Location:** `mime_list_pos` (always 80). Immediately after the header.

A sequence of null-terminated UTF-8 strings. The list ends with an **empty string** (a lone `\0` byte — i.e., two consecutive `\0` bytes mark the boundary between the last real MIME type and the terminator).

**Parsing:**
```
let mut mime_types: Vec<String> = Vec::new();
loop {
    let s = read_cstring(buf); // reads until \0
    if s.is_empty() { break; }
    mime_types.push(s);
}
```

**At runtime:** Store as `Vec<String>` eagerly at open time. This list is tiny (typically 10–30 entries).

**Special MIME index values in directory entries:**
- `0xFFFF` — redirect entry (not a real MIME type)
- `0xFFFE` — deprecated linktarget entry (skip silently)
- `0xFFFD` — deprecated deleted entry (skip silently)

---

### 3.4 Path Pointer List

**Location:** `path_ptr_pos`.  
**Size:** `entry_count × 8` bytes.  
**Element:** 8-byte u64 file offset pointing to a directory entry.

The list is **sorted lexicographically** by the entry's full path string `<namespace_char><path>` (e.g. `"CMain_Page"`, `"IEiffel_Tower.jpg"`). This enables **binary search by path**.

> ⚠️ For v5 files using old namespaces, the sort key is `<namespace><path>`. For v6 files with the unified `C` namespace, all content entries sort together under `"C"`. Both cases use the same binary comparison.

**Access strategy:** Do **not** load the full list into memory. Store `(path_ptr_pos, entry_count)` and seek on demand. For binary search, read individual 8-byte pointers via mmap slices.

---

### 3.5 Title Pointer List

**Location:** `title_ptr_pos`.  
**Size:** `entry_count × 4` bytes.  
**Element:** 4-byte u32 **entry index** (not a file offset). Dereference through the Path Pointer List to get the file offset.

The list is **sorted lexicographically** by `<namespace_char><title>`. Enables binary search by title. Because title pointers are 4 bytes (vs. 8 for path pointers), the list is half the size.

**Indirection to reach a file offset:**
```
title_ptr[i]  →  entry_index (u32)
path_ptr[entry_index]  →  file_offset (u64)
file[file_offset]  →  directory entry
```

---

### 3.6 Directory Entries

Directory entries have **variable length** due to null-terminated strings.

#### Content Entry (mime_type < 0xFFFD)

```
Offset  Size  Type    Field
──────  ────  ──────  ──────────────────────────────────────────────────────
0       2     u16     mime_type_idx    Index into MIME type list
2       1     u8      parameter_len    Length of extra_data at end
3       1     u8      namespace        ASCII char: see §3.9
4       4     u32     revision         Content revision (usually 0)
8       4     u32     cluster_number   Which cluster holds the blob
12      4     u32     blob_number      Which blob within the cluster
16      var   CStr    path             Null-terminated UTF-8 (no namespace prefix)
16+P    var   CStr    title            Null-terminated UTF-8; empty = same as path
...     n     [u8]    extra_data       parameter_len bytes (usually 0)
```

#### Redirect Entry (mime_type == 0xFFFF)

```
Offset  Size  Type    Field
──────  ────  ──────  ──────────────────────────────────────────────────────
0       2     u16     mime_type_idx   = 0xFFFF
2       1     u8      parameter_len
3       1     u8      namespace
4       4     u32     revision
8       4     u32     redirect_index  Entry number in path pointer list (target)
12      var   CStr    path
12+P    var   CStr    title
...     n     [u8]    extra_data
```

#### Deprecated Entries (mime_type == 0xFFFE or 0xFFFD)

Skip the entire entry. Do not propagate to callers.

**Effective title rule:** If the `title` CStr is empty, the effective title is identical to `path`.

---

### 3.7 Cluster Pointer List

**Location:** `cluster_ptr_pos`.  
**Size:** `cluster_count × 8` bytes.  
**Element:** 8-byte u64 file offset to the start of a cluster.

Store `(cluster_ptr_pos, cluster_count)`. Access individual pointers via mmap on demand.

---

### 3.8 Clusters & Blobs

A cluster is a **compressed (or uncompressed) container** of one or more blobs. Each blob corresponds to one content entry's data.

#### Cluster Information Byte

The **first byte** of every cluster encodes compression and layout:

```
Bit(s)  Meaning
──────  ──────────────────────────────────────────────────────────
3:0     Compression type:
          0x00 or 0x01  =  uncompressed
          0x04          =  LZMA2 / XZ
          0x05          =  Zstandard  (default since libzim 8.0, 2021)
4       Extended flag:
          0  =  blob offsets are 4 bytes (u32), cluster data ≤ 4 GB
          1  =  blob offsets are 8 bytes (u64), cluster data > 4 GB
          Only valid when major_version == 6.
          If major_version == 5, this bit must be 0.
7:5     Reserved, must be 0
```

The compression byte itself is consumed and **not** part of the compressed data stream.

#### Cluster Data Layout (after decompression)

```
[offset_0 | offset_1 | ... | offset_N | blob_0_data | blob_1_data | ... | blob_{N-1}_data]
```

Where:
- Offsets are either u32 (4 bytes, standard) or u64 (8 bytes, extended)
- `offset_0` is the offset of `blob_0` (= `N+1` × offset_size, since offsets precede data)
- `offset_N` is a sentinel: the total size of all blob data
- Blob count = `offset_0 / offset_size`
- `blob_i` spans bytes `offset_i .. offset_{i+1}` within the decompressed payload

#### Cluster Size

There is no explicit cluster size in the file. The size of cluster `i` (compressed) is:
```
cluster_ptr[i+1] - cluster_ptr[i]   (for all but the last cluster)
checksum_pos - cluster_ptr[last]     (for the last cluster)
```

> ⚠️ This size includes the compression byte. The compressed stream itself starts at `cluster_ptr[i] + 1`.

#### Decompression

| Compression byte | Algorithm | Rust crate (default/pure) | Rust crate (optional/fast) |
|---|---|---|---|
| 0x00 or 0x01 | None | — | — |
| 0x04 | LZMA2 / XZ | `lzma-rs` | `xz2` (feature `xz2`) |
| 0x05 | Zstandard | `ruzstd` | `zstd` (feature `zstd`) |

---

### 3.9 Namespaces

Namespace is a single ASCII character stored in each directory entry.

#### v6 / New namespace semantics (libzim ≥ 7.0, 2021)

| Char | Meaning |
|---|---|
| `C` | Content: all user-facing articles and resources (HTML, images, JS, CSS) |
| `M` | Metadata entries (key/value pairs, e.g. `M/Title`, `M/Language`) |
| `W` | Welcome / well-known entries (`W/mainPage`, `W/favicon`) |
| `X` | Xapian full-text search index (not parsed by this crate) |
| `-` | Reserved / internal |

#### v5 / Old namespace semantics (libzim < 7.0)

| Char | Meaning |
|---|---|
| `A` | Articles (HTML) |
| `B` | Articles (books) |
| `I` | Images |
| `J` | JavaScript-heavy images |
| `U` | Scripts / JS |
| `H` | HTML auxiliary |
| `-` | Metadata |
| `X` | Xapian index |

**Consumer guidance:** The `Archive` API exposes a `NamespaceMode` that callers can query to know which convention applies. Helper methods like `iter_articles()` and `find_article()` abstract over both modes — they search namespace `C` for v6 and namespace `A` for v5, transparently.

---

### 3.10 Checksum

- **Location:** `checksum_pos` (from header)
- **Algorithm:** MD5
- **Covers:** All bytes from offset 0 to `checksum_pos - 1` (i.e., the entire file except the checksum itself)
- **Size:** 16 bytes

Verification is **opt-in** (enabled by default, disableable via `VerifyChecksum::Skip`) because it requires reading the entire file.

---

### 3.11 Split Archives

ZIM archives larger than 4 GB (for FAT32 compatibility) can be split into chunks named `foobar.zimaa`, `foobar.zimab`, etc. Each chunk is a valid ZIM file on its own header-wise, but clusters may span across chunk boundaries (clusters are the split unit).

**Phase 1 scope:** Single-file archives only. `Archive::open()` returns `Error::SplitArchiveNotSupported` if it detects a `.zimaa`-style path. Split support is Phase 4.

---

### 3.12 Version History & Field Renames

| Version / Date | Change |
|---|---|
| v5 | Baseline. 4-byte blob offsets only. Single namespace convention. |
| v6.0 (2016) | Extended cluster flag (bit 4) enabled. 8-byte blob offsets possible. |
| v6.1 (libzim 7.0, 2021) | New namespace semantics (`C`, `M`, `W`). Not a binary format change. |
| v6.2 (libzim 9.1) | Alias entries permitted (multiple entries pointing to same blob). |
| v6.3 (libzim 9.3) | Legacy title index (`listing/titleOrdered/v0`) removed. |
| April 2024 | Spec renamed `urlPtrPos` → `pathPtrPos`, `url` field → `path`. Same bytes. |

---

## 4. Public API Design

```rust
// ── lib.rs re-exports ──────────────────────────────────────────────────────

pub use archive::{Archive, ArchiveOptions, VerifyChecksum};
pub use dirent::{Dirent, ContentEntry, RedirectEntry};
pub use error::{Error, Result};
pub use header::{Header, MajorVersion};
pub use namespace::{Namespace, NamespaceMode};

// ── archive.rs ─────────────────────────────────────────────────────────────

/// Configuration for opening a ZIM archive.
#[derive(Debug, Clone)]
pub struct ArchiveOptions {
    /// Whether to verify the MD5 checksum on open.
    /// Default: VerifyChecksum::Yes
    pub verify_checksum: VerifyChecksum,

    /// Maximum number of decompressed clusters to hold in the LRU cache.
    /// Each slot can hold ~1 MB. Default: 8
    pub cluster_cache_size: usize,
}

impl Default for ArchiveOptions { ... }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyChecksum {
    Yes,
    Skip,
}

/// A handle to an open ZIM archive.
///
/// Internally uses a memory-mapped file; the `Archive` value is `Send + Sync`
/// and can be wrapped in an `Arc` for sharing across threads.
pub struct Archive { ... }

impl Archive {
    /// Open a ZIM archive at `path` with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self>;

    /// Open with explicit options.
    pub fn open_with_options(path: impl AsRef<Path>, opts: ArchiveOptions) -> Result<Self>;

    // ── Metadata ─────────────────────────────────────────────────────────

    /// Parsed header.
    pub fn header(&self) -> &Header;

    /// File path this archive was opened from.
    pub fn path(&self) -> &Path;

    /// Total number of directory entries.
    pub fn entry_count(&self) -> u32;

    /// Total number of clusters.
    pub fn cluster_count(&self) -> u32;

    /// List of MIME types in index order.
    pub fn mime_types(&self) -> &[String];

    /// Whether this file uses new namespace semantics (v6.1+).
    pub fn namespace_mode(&self) -> NamespaceMode;

    // ── High-level convenience ────────────────────────────────────────────

    /// Look up an entry by path. In v5 files, path should include
    /// namespace (e.g. "A/Main_Page"). In v6 files, namespace is
    /// optional and defaults to "C" if omitted.
    pub fn find_by_path(&self, path: &str) -> Result<Option<Dirent>>;

    /// Look up an entry by title (prefix match, first hit).
    pub fn find_by_title(&self, title: &str) -> Result<Option<Dirent>>;

    /// Find an article entry by path, following redirects automatically.
    /// Returns an error if redirect chain exceeds `max_depth` (default 8).
    pub fn get_article(&self, path: &str) -> Result<Option<Article>>;

    /// Retrieve raw blob bytes for a content entry.
    pub fn get_blob(&self, entry: &ContentEntry) -> Result<Vec<u8>>;

    /// Retrieve the main page article (if defined in the header).
    pub fn main_page(&self) -> Result<Option<Article>>;

    /// Read a metadata value (e.g., "Title", "Language", "Description").
    /// Looks in namespace 'M' for v6 and '-' for v5.
    pub fn metadata(&self, key: &str) -> Result<Option<String>>;

    /// Iterate over all directory entries ordered by path.
    pub fn entries(&self) -> EntryIter<'_>;

    /// Iterate over content entries in the article namespace only.
    pub fn articles(&self) -> ArticleIter<'_>;

    // ── Low-level access ──────────────────────────────────────────────────

    /// Read a specific directory entry by its index in the path pointer list.
    pub fn dirent_at(&self, entry_idx: u32) -> Result<Dirent>;

    /// Follow a redirect chain, returning the final content entry.
    /// Returns `Error::RedirectLoop` if a cycle is detected.
    pub fn resolve_redirect(&self, entry: &RedirectEntry) -> Result<ContentEntry>;

    /// Binary search for an entry by full path string (namespace + path).
    /// Returns the entry index in the path pointer list, or None.
    pub fn search_path(&self, full_path: &str) -> Result<Option<u32>>;

    /// Binary search for an entry by full title string (namespace + title).
    /// Returns the entry index in the path pointer list, or None.
    pub fn search_title(&self, full_title: &str) -> Result<Option<u32>>;

    /// Prefix search over titles. Returns up to `limit` results.
    pub fn search_title_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<Dirent>>;
}

// ── header.rs ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Header {
    pub magic_number: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub uuid: [u8; 16],
    pub entry_count: u32,
    pub cluster_count: u32,
    pub path_ptr_pos: u64,
    pub title_ptr_pos: u64,
    pub cluster_ptr_pos: u64,
    pub mime_list_pos: u64,
    pub main_page: Option<u32>,      // None if == 0xFFFFFFFF
    pub layout_page: Option<u32>,    // None if == 0xFFFFFFFF
    pub checksum_pos: u64,
}

// ── dirent.rs ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Dirent {
    Content(ContentEntry),
    Redirect(RedirectEntry),
}

#[derive(Debug, Clone)]
pub struct ContentEntry {
    pub mime_type_idx: u16,
    pub namespace: char,
    pub revision: u32,
    pub cluster_number: u32,
    pub blob_number: u32,
    pub path: String,
    pub title: String,              // pre-filled: same as path if empty in file
}

impl ContentEntry {
    /// Effective MIME type string, resolved from the archive's MIME table.
    pub fn mime_type<'a>(&self, archive: &'a Archive) -> &'a str;
}

#[derive(Debug, Clone)]
pub struct RedirectEntry {
    pub namespace: char,
    pub revision: u32,
    pub redirect_index: u32,        // index into path pointer list
    pub path: String,
    pub title: String,
}

// ── Resolved article ───────────────────────────────────────────────────────

/// A fully resolved article: the content entry plus its raw bytes.
pub struct Article {
    pub entry: ContentEntry,
    pub data: Vec<u8>,
}

impl Article {
    /// Attempt to interpret data as UTF-8 text (for HTML, text/plain, etc.)
    pub fn as_text(&self) -> Option<&str>;

    /// MIME type string.
    pub fn mime_type<'a>(&self, archive: &'a Archive) -> &'a str;

    /// True if the data is binary (images, etc.)
    pub fn is_binary(&self) -> bool;
}

// ── namespace.rs ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceMode {
    /// v6.1+ unified namespace (C for content, M for metadata, W for well-known)
    New,
    /// v5 / pre-2021 multi-namespace (A for articles, I for images, etc.)
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    Content,    // 'C' (new) or 'A' (legacy articles)
    Images,     // 'I' (legacy only)
    Metadata,   // 'M' (new) or '-' (legacy)
    WellKnown,  // 'W' (new only)
    Search,     // 'X'
    Other(char),
}
```

### Iterators

```rust
/// Lazy iterator over directory entries, in path order.
pub struct EntryIter<'a> { ... }
impl<'a> Iterator for EntryIter<'a> {
    type Item = Result<Dirent>;
}

/// Lazy iterator over content entries in the article namespace.
pub struct ArticleIter<'a> { ... }
impl<'a> Iterator for ArticleIter<'a> {
    type Item = Result<ContentEntry>;
}
```

---

## 5. Internal Architecture

### 5.1 `Archive` struct internals

```rust
struct Archive {
    mmap: Mmap,                          // memmap2::Mmap, read-only
    path: PathBuf,
    header: Header,
    mime_types: Vec<String>,
    namespace_mode: NamespaceMode,
    cluster_cache: Mutex<LruCache<u32, Arc<Vec<u8>>>>,  // keyed by cluster_number
}
```

The `Mmap` is never written. It is kept alive for the lifetime of `Archive`. The struct is `Send + Sync` because `Mmap` is `Send + Sync` and all mutation is behind a `Mutex`.

### 5.2 Reading pointer list entries

```rust
// Path pointer list: 8 bytes per entry at (path_ptr_pos + idx * 8)
fn path_ptr(&self, idx: u32) -> Result<u64> {
    let off = self.header.path_ptr_pos + (idx as u64 * 8);
    Ok(u64::from_le_bytes(self.mmap[off..off+8].try_into()?))
}

// Title pointer list: 4 bytes per entry at (title_ptr_pos + idx * 4)
fn title_ptr(&self, idx: u32) -> Result<u32> {
    let off = self.header.title_ptr_pos + (idx as u64 * 4);
    Ok(u32::from_le_bytes(self.mmap[off..off+4].try_into()?))
}
```

### 5.3 Binary search

```rust
fn search_path(&self, full_path: &str) -> Result<Option<u32>> {
    let n = self.header.entry_count;
    let (mut lo, mut hi) = (0u32, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let dirent = self.dirent_at(mid)?;
        let candidate = format!("{}{}", dirent.namespace(), dirent.path());
        match candidate.as_str().cmp(full_path) {
            Ordering::Equal   => return Ok(Some(mid)),
            Ordering::Less    => lo = mid + 1,
            Ordering::Greater => hi = mid,
        }
    }
    Ok(None)
}
```

Title search follows the same pattern but uses the title pointer list with its extra indirection level.

### 5.4 Cluster decompression and caching

```rust
fn get_cluster_data(&self, cluster_number: u32) -> Result<Arc<Vec<u8>>> {
    // 1. Check LRU cache
    {
        let mut cache = self.cluster_cache.lock().unwrap();
        if let Some(data) = cache.get(&cluster_number) {
            return Ok(Arc::clone(data));
        }
    }

    // 2. Compute compressed range
    let start = self.cluster_ptr(cluster_number)?;
    let end = if cluster_number + 1 < self.header.cluster_count {
        self.cluster_ptr(cluster_number + 1)?
    } else {
        self.header.checksum_pos
    };

    // 3. Read compression byte
    let info_byte = self.mmap[start as usize];
    let compression = info_byte & 0x0F;
    let extended    = (info_byte & 0x10) != 0;
    let compressed_data = &self.mmap[(start + 1) as usize..end as usize];

    // 4. Decompress
    let decompressed: Vec<u8> = match compression {
        0x00 | 0x01 => compressed_data.to_vec(),
        0x04        => decompress_xz(compressed_data)?,
        0x05        => decompress_zstd(compressed_data)?,
        other       => return Err(Error::UnknownCompression(other)),
    };

    // 5. Store extended flag alongside data (needed for blob extraction)
    //    Encode as: [1-byte extended flag | decompressed data]
    //    (Or use a ClusterData newtype that carries both fields)

    // 6. Insert into LRU and return
    let arc = Arc::new(decompressed);
    {
        let mut cache = self.cluster_cache.lock().unwrap();
        cache.put(cluster_number, Arc::clone(&arc));
    }
    Ok(arc)
}
```

### 5.5 Blob extraction

```rust
fn get_blob_from_cluster(data: &[u8], blob_number: u32, extended: bool)
    -> Result<Vec<u8>>
{
    let offset_size: usize = if extended { 8 } else { 4 };
    let first_offset = read_u64_or_u32(data, 0, extended)?;
    let blob_count = (first_offset as usize) / offset_size;

    if blob_number as usize >= blob_count {
        return Err(Error::BlobOutOfRange { blob_number, blob_count });
    }

    let start = read_u64_or_u32(data, blob_number as usize * offset_size, extended)? as usize;
    let end   = read_u64_or_u32(data, (blob_number as usize + 1) * offset_size, extended)? as usize;

    Ok(data[start..end].to_vec())
}
```

---

## 6. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ── I/O ──────────────────────────────────────────────────────────────
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // ── Format errors ─────────────────────────────────────────────────────
    #[error("Invalid magic number: expected 0x044D495A, got 0x{0:08X}")]
    InvalidMagic(u32),

    #[error("Unsupported major version: {0} (supported: 5, 6)")]
    UnsupportedVersion(u16),

    #[error("Truncated header: file is too small ({0} bytes)")]
    TruncatedHeader(u64),

    #[error("Offset out of file bounds: offset {offset} in field '{field}'")]
    OffsetOutOfBounds { offset: u64, field: &'static str },

    #[error("Invalid UTF-8 in string at offset {0}")]
    InvalidUtf8(u64),

    #[error("Unknown compression type: 0x{0:02X}")]
    UnknownCompression(u8),

    #[error("Blob {blob_number} out of range (cluster has {blob_count} blobs)")]
    BlobOutOfRange { blob_number: u32, blob_count: usize },

    #[error("Extended cluster encountered in v5 archive (illegal)")]
    ExtendedClusterInV5,

    // ── Logic errors ───────────────────────────────────────────────────────
    #[error("Redirect loop detected at entry index {0}")]
    RedirectLoop(u32),

    #[error("Redirect target index {0} is out of range")]
    RedirectIndexOutOfRange(u32),

    // ── Checksum ───────────────────────────────────────────────────────────
    #[error("MD5 checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    // ── Decompression ──────────────────────────────────────────────────────
    #[error("LZMA2 decompression failed: {0}")]
    LzmaDecompress(String),

    #[error("Zstandard decompression failed: {0}")]
    ZstdDecompress(String),

    // ── Unsupported features ───────────────────────────────────────────────
    #[error("Split ZIM archives are not yet supported")]
    SplitArchiveNotSupported,
}

pub type Result<T> = std::result::Result<T, Error>;
```

---

## 7. Dependencies

### `Cargo.toml` for `zim-reader`

```toml
[package]
name = "zim-reader"
version = "0.1.0"
edition = "2024"
rust-version = "1.95"
description = "Pure-Rust library for reading ZIM archive files"
license = "MIT OR Apache-2.0"
repository = "https://github.com/<org>/zim-reader"
keywords = ["zim", "wikipedia", "kiwix", "openzim", "offline"]
categories = ["encoding", "parsing"]

[dependencies]
# Memory-mapped I/O
memmap2 = "0.9"

# Byte-order reading (used in header/pointer parsing)
byteorder = "1"

# Error types
thiserror = "1"

# LRU cluster cache
lru = "0.12"

# Pure-Rust LZMA2 / XZ decompression (default)
lzma-rs = { version = "0.3", optional = true, default-features = false, features = ["stream"] }

# Pure-Rust Zstandard decompression (default)
ruzstd = { version = "0.7", optional = true }

# C-backed LZMA2 (faster, optional feature)
xz2 = { version = "0.1", optional = true }

# C-backed Zstandard (faster, optional feature)
zstd = { version = "0.13", optional = true, default-features = false }

# MD5 for checksum verification
md-5 = "0.10"

[features]
default = ["compression-pure"]

# Pure-Rust codecs — portable, no C toolchain required
compression-pure = ["dep:lzma-rs", "dep:ruzstd"]

# C-backed codecs — faster, requires C build toolchain
compression-native = ["dep:xz2", "dep:zstd"]

# Enable both (native takes priority at runtime if both are compiled in)
# Not typically useful; pick one.

[dev-dependencies]
# For integration tests
tempfile = "3"
hex = "0.4"
```

**Notes:**
- `compression-pure` and `compression-native` are **mutually exclusive in intent** but not enforced by Cargo feature flags (Cargo cannot express mutex). Document that consumers should enable exactly one. In code, prefer native if both are compiled (a compile-time `cfg` selection).
- A compile error is emitted if neither compression feature is enabled: `compile_error!("Enable either 'compression-pure' or 'compression-native'")`.

---

## 8. Testing Strategy

### 8.1 Test data

Use the **openzim/zim-testing-suite** as a git submodule at `tests/fixtures/`. This provides canonical ZIM files covering:
- Minimal v5 ZIM with a few articles
- v6 ZIM with new namespace scheme
- ZIM with LZMA2 compression
- ZIM with Zstandard compression
- ZIM with extended clusters
- ZIM with redirects
- ZIM with images
- ZIM with metadata

Pull with: `git submodule update --init --recursive`

If the submodule is absent, integration tests are skipped with `eprintln!("SKIP: test fixtures not found")`.

### 8.2 Unit tests

Each module has `#[cfg(test)] mod tests` inline:

| Module | Tests |
|---|---|
| `header.rs` | Parse known-good bytes, reject bad magic, reject unsupported version, reject truncated input |
| `mime.rs` | Parse known list, handle empty list, handle non-ASCII (reject), max 65535 entries |
| `dirent.rs` | Parse content entry, parse redirect, parse entry with long path, `title == path` fallback, deprecated entry returns skip |
| `cluster.rs` | Uncompressed blob extraction, standard (4-byte) offsets, extended (8-byte) offsets, blob out of range error |
| `pointer_list.rs` | Read specific pointers, off-by-one at boundaries |
| `archive.rs` | Invalid magic rejects, unsupported version rejects |
| `namespace.rs` | `detect_namespace_mode` for v5 vs v6.1 |

### 8.3 Integration tests (require fixture files)

```
test_open_v5_zim              — opens v5 file, checks header fields
test_open_v6_zim              — opens v6 file, checks namespace_mode == New
test_find_by_path_v5          — exact path lookup in v5 namespace
test_find_by_path_v6          — exact path lookup in v6 C namespace
test_title_search             — title prefix search returns sorted results
test_redirect_follows         — redirect chain resolves to content
test_redirect_loop_error      — circular redirect returns RedirectLoop error
test_get_article_html         — article data is valid UTF-8 HTML
test_get_image_blob           — image blob is non-empty bytes
test_metadata_title           — M/Title returns expected string
test_main_page                — main_page() returns Some(Article)
test_entry_iter_count         — EntryIter visits entry_count entries
test_article_iter_count       — ArticleIter count <= entry_count
test_lzma_cluster             — LZMA2 compressed cluster decompresses correctly
test_zstd_cluster             — Zstandard compressed cluster decompresses correctly
test_extended_cluster         — 8-byte offset cluster extracts correct blob
test_checksum_valid           — checksum verification passes on good file
test_checksum_invalid         — checksum verification fails on corrupted file
test_entry_count_matches      — entry_count from header matches actual entry count
```

### 8.4 Property / fuzz tests

A `fuzz/` directory (using `cargo-fuzz`) with two fuzz targets:
- `fuzz_open_header` — feed random bytes to header parser, assert no panic
- `fuzz_decompress` — feed random bytes to each decompressor, assert no panic

These run in CI with a 30-second timeout and a corpus seeded from the fixture files.

### 8.5 Benchmark

One Criterion benchmark:
- `bench_get_article_hot` — retrieve 1000 articles sequentially with warm cluster cache
- `bench_get_article_cold` — retrieve 1000 articles with cache size = 0

Target: hot retrieval of a typical Wikipedia article (50 KB HTML) in < 5 ms on a modern workstation (I/O bound, not CPU bound).

---

## 9. Implementation Phases

---

### Phase 1 — Core Parsing & Header (Milestone: `v0.1.0`)

**Goal:** Open a ZIM file and read its header and MIME table correctly.

**Tasks:**

1. **Repository setup**
   - Init workspace `Cargo.toml` with `[workspace]` and `members = ["crates/zim-reader", "tools/zim-info"]`
   - Add `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`, `CHANGELOG.md`
   - Set up `.github/workflows/ci.yml`: build, test, clippy (`-D warnings`), `rustfmt --check`

2. **`util.rs`** — shared byte-reading helpers
   - `fn read_u16_le(buf: &[u8], off: usize) -> Result<u16>`
   - `fn read_u32_le(buf: &[u8], off: usize) -> Result<u32>`
   - `fn read_u64_le(buf: &[u8], off: usize) -> Result<u64>`
   - `fn read_cstring(buf: &[u8], off: usize) -> Result<(String, usize)>` — returns string and bytes consumed (including null)
   - Bounds-check every access; return `Error::OffsetOutOfBounds` on failure

3. **`error.rs`** — full `Error` enum and `Result<T>` alias

4. **`header.rs`** — `Header::parse(buf: &[u8]) -> Result<Header>`
   - Read all 11 fields
   - Validate magic, version
   - Validate `mime_list_pos == 80`
   - Convert `0xFFFFFFFF` → `None` for `main_page` and `layout_page`

5. **`mime.rs`** — `MimeTable::parse(buf: &[u8], offset: u64) -> Result<Vec<String>>`
   - Scan null-terminated strings until empty string
   - Return `Vec<String>`

6. **`archive.rs`** — `Archive::open` (minimal)
   - Open file with `memmap2::MmapOptions`
   - Parse header
   - Parse MIME table
   - Detect `NamespaceMode` (v6.1+ heuristic: check if any `C` namespace entry exists, or use version field)
   - Initialise empty cluster cache
   - Validate offsets are in-bounds

7. **`namespace.rs`** — `Namespace` enum, `NamespaceMode` detection

8. **`tools/zim-info/main.rs`** — prints all header fields and MIME types; used for manual testing

**Deliverable:** `Archive::open()` succeeds on all fixture files. Header fields match known values. `cargo test` passes all unit tests in scope.

---

### Phase 2 — Directory Entries & Binary Search (Milestone: `v0.2.0`)

**Goal:** Look up entries by path or title without loading all entries into memory.

**Tasks:**

1. **`dirent.rs`** — `Dirent::parse(buf: &[u8], offset: u64, mime_count: usize) -> Result<Dirent>`
   - Read `mime_type_idx`; branch on content vs. redirect vs. deprecated
   - Read `parameter_len`, `namespace`, `revision`
   - For content: read `cluster_number`, `blob_number`, then two CStrings
   - For redirect: read `redirect_index`, then two CStrings
   - Apply `title = path if title.is_empty()`
   - Skip deprecated entries (return a sentinel or use `Option`)

2. **`pointer_list.rs`**
   - `fn path_ptr(mmap: &[u8], path_ptr_pos: u64, idx: u32) -> Result<u64>`
   - `fn title_ptr(mmap: &[u8], title_ptr_pos: u64, idx: u32) -> Result<u32>`

3. **`archive.rs`** — `Archive::dirent_at(entry_idx: u32) -> Result<Dirent>`
   - `path_ptr(entry_idx)` → file offset → `Dirent::parse`

4. **`archive.rs`** — `Archive::search_path(full_path: &str) -> Result<Option<u32>>`
   - Binary search over path pointer list
   - Compare `<namespace><path>` for each midpoint

5. **`archive.rs`** — `Archive::search_title(full_title: &str) -> Result<Option<u32>>`
   - Binary search over title pointer list (with double-indirection)
   - Compare `<namespace><title>`

6. **`archive.rs`** — `Archive::search_title_prefix(prefix: &str, limit: usize) -> Result<Vec<Dirent>>`
   - Binary search for lower bound; scan forward until prefix no longer matches or limit reached

7. **`archive.rs`** — `Archive::find_by_path`, `find_by_title`
   - Wrap search functions; abstract namespace differences between v5 and v6

8. **`archive.rs`** — `EntryIter`, `ArticleIter`
   - Iterate sequentially through path pointer list
   - `ArticleIter` filters by article namespace (`C` for v6, `A` for v5)

9. **Tests:** All `test_find_by_path_*`, `test_title_search`, `test_entry_iter_count`, `test_article_iter_count` on fixture files

**Deliverable:** Can look up any article by path or title, and iterate over all entries. No decompression yet.

---

### Phase 3 — Cluster Decompression & Blob Extraction (Milestone: `v0.3.0`)

**Goal:** Read article content and image data.

**Tasks:**

1. **`cluster.rs`** — `ClusterInfo::from_byte(byte: u8, major_version: u16) -> Result<ClusterInfo>`
   - Decode compression type and extended flag
   - Reject extended flag when `major_version == 5`

2. **`cluster.rs`** — decompression dispatch
   - `fn decompress(info: ClusterInfo, data: &[u8]) -> Result<Vec<u8>>`
   - `compression-pure` path: `lzma-rs` (XZ), `ruzstd` (Zstandard)
   - `compression-native` path: `xz2` (XZ), `zstd` (Zstandard)
   - Compile-time selection via `cfg(feature = ...)`
   - Uncompressed: `data.to_vec()`

3. **`cluster.rs`** — blob extraction
   - `fn extract_blob(decompressed: &[u8], blob_number: u32, extended: bool) -> Result<Vec<u8>>`
   - Read first offset to determine blob count and offset size
   - Bounds-check blob_number
   - Return `data[start..end]` as `Vec<u8>`

4. **`archive.rs`** — `Archive::get_cluster_data(cluster_number: u32) -> Result<Arc<Vec<u8>>>`
   - Compute compressed range from cluster pointer list
   - Check LRU cache first
   - Decompress and cache result

5. **`archive.rs`** — `Archive::get_blob(entry: &ContentEntry) -> Result<Vec<u8>>`
   - Call `get_cluster_data(entry.cluster_number)`
   - Call `extract_blob(data, entry.blob_number, extended)`

6. **`archive.rs`** — `Archive::resolve_redirect(entry: &RedirectEntry) -> Result<ContentEntry>`
   - Follow redirect index, with cycle detection (visited `HashSet<u32>`, max depth 8)
   - Return `Error::RedirectLoop` on cycle

7. **`archive.rs`** — `Archive::get_article(path: &str) -> Result<Option<Article>>`
   - `find_by_path` → resolve any redirect → `get_blob` → `Article`

8. **`archive.rs`** — `Archive::main_page() -> Result<Option<Article>>`
   - `header.main_page` → `dirent_at` → `get_article`

9. **`archive.rs`** — `Archive::metadata(key: &str) -> Result<Option<String>>`
   - Construct path: `M/<key>` for v6, `-/<key>` for v5
   - `find_by_path` → `get_blob` → interpret as UTF-8

10. **`Article`** struct — `as_text()`, `mime_type()`, `is_binary()`

11. **Tests:** All cluster, blob, redirect, article, image, metadata integration tests

**Deliverable:** Full read capability. Can extract any article or resource from a ZIM file.

---

### Phase 4 — Checksum, Edge Cases & Hardening (Milestone: `v0.4.0`)

**Goal:** Correctness and robustness under adversarial or unusual inputs.

**Tasks:**

1. **Checksum verification** — `VerifyChecksum::Yes` (default)
   - Stream the mmap in 4 MB chunks through `md5::Context`
   - Compare to the 16-byte suffix
   - Return `Error::ChecksumMismatch` on failure

2. **Deprecated entry handling** — ensure both `0xFFFE` and `0xFFFD` are skipped gracefully in all iteration and search paths

3. **Alias entries** (v6.2) — two directory entries pointing to the same cluster+blob. No special handling needed; the library naturally supports this since blobs are fetched by reference. Add a test.

4. **v6.3 missing title index** — `listing/titleOrdered/v0` may be absent. `search_title` should gracefully handle title pointer list being empty or absent. Add a test.

5. **Split archive detection** — if `path` ends in `.zimaa`, `.zimab` etc., return `Error::SplitArchiveNotSupported` immediately with a helpful message.

6. **Embedded ZIM support** — `Archive::open_at_offset(path, offset)` variant. The mmap starts at `offset` and all positions are relative. (Needed for ZIM files embedded in other containers.)

7. **Fuzz targets** — set up `cargo fuzz` with `fuzz_open_header` and `fuzz_decompress`; add to CI with 30s timeout

8. **Criterion benchmarks** — hot and cold article retrieval

9. **Documentation pass** — every public item has a doc comment; `lib.rs` has a complete quick-start example; `CHANGELOG.md` updated

**Deliverable:** The crate passes all tests including fuzz, handles all known ZIM format edge cases, and has complete documentation suitable for crates.io publication.

---

### Phase 5 — Publication & Stabilisation (Milestone: `v0.5.0` → `v1.0.0`)

**Goal:** A published, stable crate ready for production use by `zim-mcp`.

**Tasks:**

1. **API review** — compare public API against actual usage patterns in `zim-mcp` prototype; adjust before stabilisation
2. **Semver stability commitment** — finalise which types are `#[non_exhaustive]`
3. **Minimum Supported Rust Version (MSRV)** — nail down to `1.95` and test in CI with `rust-toolchain.toml`
4. **crates.io publication** — `cargo publish` with all metadata fields populated
5. **GitHub release** — tag `v0.5.0`, automated by `publish.yml`
6. **README** — full README with:
   - Quick-start code example (open file, find article, print HTML)
   - Feature flags table
   - MSRV badge
   - Format version support table

---

## 10. Performance Considerations

### Memory mapping

Use `memmap2::MmapOptions::new().map()` (read-only). On Linux, this uses `mmap(MAP_SHARED, PROT_READ)`, making individual cluster reads kernel-optimised. For large Wikipedia files (20 GB+), the OS will page in only the accessed regions.

Do **not** call `.populate()` (pre-fault all pages) — Wikipedia ZIM files are too large for this to be useful.

### Cluster cache

The LRU cluster cache is the single most important performance feature. A Wikipedia cluster is ~1 MB compressed, ~3–5 MB decompressed. Without caching, fetching 10 sequential articles from the same cluster would decompress it 10 times.

Default cache size of **8 clusters** ≈ 40 MB decompressed in the worst case. Consumers serving many concurrent users should increase this. The `ArchiveOptions::cluster_cache_size` field is exposed for this.

The `Mutex<LruCache>` is a single contention point. For high-throughput multi-threaded consumers, a `DashMap` or shard-based cache would be better, but is out of scope for v1.

### Decompression codec choice

Benchmarks from the `ruzstd` and `lzma-rs` authors show:
- Zstandard (pure Rust): ~1.5–2× slower than the C backend
- LZMA2 (pure Rust): ~3–4× slower than the C backend

For a single-user MCP server, pure-Rust is fast enough. For high-throughput or interactive usage, expose `compression-native` as a recommended feature in the README.

### Binary search I/O

Each binary search step reads 8 bytes (path pointer) + a variable-length directory entry. On a cold cache, this is O(log N) page faults. For Wikipedia (14M entries), `log2(14M) ≈ 24` steps. With typical 4 KB pages and 64-byte dirents, this is at most 24 page faults per lookup — typically much fewer due to spatial locality.

### `Arc<Vec<u8>>` for cluster data

Returning `Arc<Vec<u8>>` from the cache means multiple concurrent callers can hold references to the same decompressed cluster without copying. Blob extraction slices into this `Vec` and copies only the needed bytes into the returned `Vec<u8>`. This is the right trade-off since blobs are small (typically < 100 KB) relative to the full cluster.

---

## 11. CI / Repository Standards

### `ci.yml` jobs

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { submodules: recursive }   # pull zim-testing-suite
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --all-features
      - run: cargo test --all-features
      - run: cargo clippy --all-features -- -D warnings
      - run: cargo fmt --check

  msrv:
    runs-on: ubuntu-latest
    steps:
      - uses: dtolnay/rust-toolchain@1.95
      - run: cargo build   # default features only

  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz
      - run: cargo fuzz run fuzz_open_header -- -max_total_time=30
      - run: cargo fuzz run fuzz_decompress  -- -max_total_time=30

  no-std-check:
    # Verify the library can be compiled without std if needed in future
    # Skipped for now; placeholder for later
```

### Branch & PR policy

- `main` is always publishable
- PRs require CI green + one reviewer approval
- Squash-merge only
- Version bumps via `cargo release` (the `cargo-release` tool)

### Commit message format

Conventional Commits: `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `perf:`, `chore:`.

---

## Appendix A: Format Quick Reference

```
File layout (top to bottom):
┌─────────────────────────────────┐ ← offset 0
│ Header (80 bytes)               │
├─────────────────────────────────┤ ← offset 80 (= mime_list_pos)
│ MIME Type List (variable)       │ null-terminated strings, double-null end
├─────────────────────────────────┤ ← path_ptr_pos
│ Path Pointer List               │ entry_count × u64
├─────────────────────────────────┤ ← title_ptr_pos
│ Title Pointer List              │ entry_count × u32  (entry indices)
├─────────────────────────────────┤ (variable position, dirents can interleave)
│ Directory Entries               │ variable length, sorted by namespace+path
├─────────────────────────────────┤ ← cluster_ptr_pos
│ Cluster Pointer List            │ cluster_count × u64
├─────────────────────────────────┤
│ Clusters (data)                 │ [compression_byte | compressed_payload]
├─────────────────────────────────┤ ← checksum_pos
│ MD5 Checksum (16 bytes)         │
└─────────────────────────────────┘ ← EOF
```

## Appendix B: Namespace Decision Logic

```rust
pub fn detect_namespace_mode(header: &Header) -> NamespaceMode {
    // v6.1 was introduced with minor_version = 1 of major version 6
    // v5 files always use legacy namespaces
    if header.major_version == 6 && header.minor_version >= 1 {
        NamespaceMode::New
    } else {
        NamespaceMode::Legacy
    }
}

pub fn article_namespace(mode: NamespaceMode) -> char {
    match mode {
        NamespaceMode::New    => 'C',
        NamespaceMode::Legacy => 'A',
    }
}

pub fn metadata_namespace(mode: NamespaceMode) -> char {
    match mode {
        NamespaceMode::New    => 'M',
        NamespaceMode::Legacy => '-',
    }
}
```
