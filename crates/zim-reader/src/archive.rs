use std::cmp::Ordering;
use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::{Mmap, MmapOptions};

use crate::dirent::{read_path_sort_key, read_title_sort_key, ContentEntry, Dirent};
use crate::error::{Error, Result};
use crate::header::{Header, HEADER_SIZE};
use crate::mime::parse_mime_table;
use crate::namespace::{article_namespace, detect_namespace_mode, NamespaceMode};
use crate::pointer_list::{path_ptr, title_ptr};

/// Whether to verify the archive's MD5 checksum when opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyChecksum {
    Yes,
    Skip,
}

/// Configuration for opening a ZIM archive.
#[derive(Debug, Clone)]
pub struct ArchiveOptions {
    /// Whether to verify the MD5 checksum on open.
    ///
    /// Currently ignored; checksum verification is implemented in a later
    /// release. The field is accepted now so callers don't need to migrate
    /// once enforcement is added.
    pub verify_checksum: VerifyChecksum,

    /// Maximum number of decompressed clusters to hold in the LRU cache.
    ///
    /// Currently unused; the cluster cache is implemented in a later release.
    pub cluster_cache_size: usize,
}

impl Default for ArchiveOptions {
    fn default() -> Self {
        Self {
            verify_checksum: VerifyChecksum::Yes,
            cluster_cache_size: 8,
        }
    }
}

/// A handle to an open ZIM archive.
///
/// Backed by a memory-mapped read of the underlying file. `Archive` is
/// `Send + Sync` and can be wrapped in an `Arc` for concurrent access.
pub struct Archive {
    mmap: Mmap,
    path: PathBuf,
    header: Header,
    mime_types: Vec<String>,
    namespace_mode: NamespaceMode,
}

impl Archive {
    /// Open a ZIM archive with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, ArchiveOptions::default())
    }

    /// Open a ZIM archive with explicit options.
    pub fn open_with_options(path: impl AsRef<Path>, _opts: ArchiveOptions) -> Result<Self> {
        let path = path.as_ref();

        if is_split_archive_path(path) {
            return Err(Error::SplitArchiveNotSupported);
        }

        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        if mmap.len() < HEADER_SIZE {
            return Err(Error::TruncatedHeader(mmap.len() as u64));
        }

        let header = Header::parse(&mmap[..HEADER_SIZE])?;
        validate_offsets(&header, mmap.len() as u64)?;

        let mime_types = parse_mime_table(&mmap, header.mime_list_pos)?;
        let namespace_mode = detect_namespace_mode(&header);

        Ok(Archive {
            mmap,
            path: path.to_path_buf(),
            header,
            mime_types,
            namespace_mode,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entry_count(&self) -> u32 {
        self.header.entry_count
    }

    pub fn cluster_count(&self) -> u32 {
        self.header.cluster_count
    }

    pub fn mime_types(&self) -> &[String] {
        &self.mime_types
    }

    pub fn namespace_mode(&self) -> NamespaceMode {
        self.namespace_mode
    }

    // ── Directory entries ────────────────────────────────────────────────

    /// Read the directory entry at index `entry_idx` in the path pointer list.
    /// Returns `Ok(None)` if the entry is a deprecated linktarget or deleted
    /// entry, which callers should skip.
    pub fn dirent_at(&self, entry_idx: u32) -> Result<Option<Dirent>> {
        if entry_idx >= self.header.entry_count {
            return Err(Error::OffsetOutOfBounds {
                offset: entry_idx as u64,
                field: "entry_idx",
            });
        }
        let offset = path_ptr(&self.mmap, self.header.path_ptr_pos, entry_idx)?;
        Dirent::parse_at(&self.mmap, offset, self.mime_types.len())
    }

    /// Iterate over every directory entry in path order. Deprecated entries
    /// are skipped silently.
    pub fn entries(&self) -> EntryIter<'_> {
        EntryIter {
            archive: self,
            next: 0,
            end: self.header.entry_count,
        }
    }

    /// Iterate over content entries in the archive's article namespace
    /// (`C` for v6.1+, `A` for v5). Redirects and non-article namespaces are
    /// filtered out.
    pub fn articles(&self) -> ArticleIter<'_> {
        ArticleIter {
            inner: self.entries(),
            article_ns: article_namespace(self.namespace_mode),
        }
    }

    // ── Lookup ────────────────────────────────────────────────────────────

    /// Find the dirent for `(namespace, path)`. If `namespace` is `None`,
    /// defaults to the archive's article namespace.
    pub fn find_by_path(&self, namespace: Option<char>, path: &str) -> Result<Option<Dirent>> {
        let ns = namespace.unwrap_or_else(|| article_namespace(self.namespace_mode));
        let Some(idx) = self.search_path(ns, path)? else {
            return Ok(None);
        };
        self.dirent_at(idx)
    }

    /// Find the dirent for `(namespace, title)`. If `namespace` is `None`,
    /// defaults to the archive's article namespace.
    pub fn find_by_title(&self, namespace: Option<char>, title: &str) -> Result<Option<Dirent>> {
        let ns = namespace.unwrap_or_else(|| article_namespace(self.namespace_mode));
        let Some(idx) = self.search_title(ns, title)? else {
            return Ok(None);
        };
        self.dirent_at(idx)
    }

    /// Binary-search the path pointer list for an entry whose
    /// `(namespace, path)` exactly equals the target. Returns the entry's
    /// index in the path pointer list.
    pub fn search_path(&self, namespace: char, path: &str) -> Result<Option<u32>> {
        let (mut lo, mut hi) = (0u32, self.header.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let offset = path_ptr(&self.mmap, self.header.path_ptr_pos, mid)?;
            let (ns, p) = read_path_sort_key(&self.mmap, offset)?;
            match cmp_key((ns, p.as_str()), (namespace, path)) {
                Ordering::Equal => return Ok(Some(mid)),
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Binary-search the title pointer list for an entry whose
    /// `(namespace, title)` exactly equals the target. Returns the resolved
    /// entry index (not the title rank), suitable for passing to
    /// [`Archive::dirent_at`].
    pub fn search_title(&self, namespace: char, title: &str) -> Result<Option<u32>> {
        let (mut lo, mut hi) = (0u32, self.header.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry_idx = title_ptr(&self.mmap, self.header.title_ptr_pos, mid)?;
            let offset = path_ptr(&self.mmap, self.header.path_ptr_pos, entry_idx)?;
            let (ns, t) = read_title_sort_key(&self.mmap, offset)?;
            match cmp_key((ns, t.as_str()), (namespace, title)) {
                Ordering::Equal => return Ok(Some(entry_idx)),
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }

    /// Return up to `limit` dirents whose title starts with `prefix` in the
    /// given namespace, ordered by title. Deprecated entries are skipped.
    pub fn search_title_prefix(
        &self,
        namespace: char,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<Dirent>> {
        let n = self.header.entry_count;

        // Lower-bound binary search for (namespace, prefix).
        let (mut lo, mut hi) = (0u32, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry_idx = title_ptr(&self.mmap, self.header.title_ptr_pos, mid)?;
            let offset = path_ptr(&self.mmap, self.header.path_ptr_pos, entry_idx)?;
            let (ns, t) = read_title_sort_key(&self.mmap, offset)?;
            if cmp_key((ns, t.as_str()), (namespace, prefix)) == Ordering::Less {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        // Forward scan; stop when namespace changes or title no longer has prefix.
        let mut results = Vec::new();
        let mut i = lo;
        while i < n && results.len() < limit {
            let entry_idx = title_ptr(&self.mmap, self.header.title_ptr_pos, i)?;
            let offset = path_ptr(&self.mmap, self.header.path_ptr_pos, entry_idx)?;
            let (ns, t) = read_title_sort_key(&self.mmap, offset)?;
            if ns != namespace {
                break;
            }
            if !t.starts_with(prefix) {
                // Title sort is ascending within a namespace; once we've passed
                // the prefix range we can stop.
                if t.as_str() > prefix {
                    break;
                }
                i += 1;
                continue;
            }
            match Dirent::parse_at(&self.mmap, offset, self.mime_types.len())? {
                Some(d) => results.push(d),
                None => { /* deprecated: skip */ }
            }
            i += 1;
        }
        Ok(results)
    }

    #[cfg(test)]
    pub(crate) fn mmap_len(&self) -> usize {
        self.mmap.len()
    }
}

fn cmp_key(a: (char, &str), b: (char, &str)) -> Ordering {
    match (a.0 as u32).cmp(&(b.0 as u32)) {
        Ordering::Equal => a.1.cmp(b.1),
        other => other,
    }
}

// ── Iterators ────────────────────────────────────────────────────────────

/// Lazy iterator over every dirent, in path-pointer-list order. Yields
/// `Err` on a parse failure but remains usable; yields `None` at the end.
pub struct EntryIter<'a> {
    archive: &'a Archive,
    next: u32,
    end: u32,
}

impl<'a> Iterator for EntryIter<'a> {
    type Item = Result<Dirent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.next >= self.end {
                return None;
            }
            let idx = self.next;
            self.next += 1;
            match self.archive.dirent_at(idx) {
                Ok(Some(d)) => return Some(Ok(d)),
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Lazy iterator over content entries in the archive's article namespace.
pub struct ArticleIter<'a> {
    inner: EntryIter<'a>,
    article_ns: char,
}

impl<'a> Iterator for ArticleIter<'a> {
    type Item = Result<ContentEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next()? {
                Ok(Dirent::Content(c)) if c.namespace == self.article_ns => {
                    return Some(Ok(c));
                }
                Ok(_) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────────

fn is_split_archive_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    // Split archives use extensions like "zimaa", "zimab", ..., "zimzz".
    if ext.len() != 5 {
        return false;
    }
    let bytes = ext.as_bytes();
    &bytes[..3] == b"zim" && bytes[3].is_ascii_lowercase() && bytes[4].is_ascii_lowercase()
}

fn validate_offsets(header: &Header, file_len: u64) -> Result<()> {
    check_in_bounds(header.path_ptr_pos, file_len, "path_ptr_pos")?;
    check_in_bounds(header.title_ptr_pos, file_len, "title_ptr_pos")?;
    check_in_bounds(header.cluster_ptr_pos, file_len, "cluster_ptr_pos")?;
    check_in_bounds(header.checksum_pos, file_len, "checksum_pos")?;

    if header.checksum_pos + 16 != file_len {
        return Err(Error::InvalidHeader {
            field: "checksum_pos",
            reason: format!(
                "expected checksum_pos + 16 == file length {file_len}, got {}",
                header.checksum_pos + 16
            ),
        });
    }
    Ok(())
}

fn check_in_bounds(offset: u64, file_len: u64, field: &'static str) -> Result<()> {
    if offset >= file_len {
        return Err(Error::OffsetOutOfBounds { offset, field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirent::RedirectEntry;
    use crate::header::ZIM_MAGIC;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── Phase 1 minimal-ZIM tests ────────────────────────────────────────

    /// Build a synthetic, well-formed minimal ZIM file with zero entries.
    fn build_minimal_zim(major: u16, minor: u16) -> Vec<u8> {
        let mime_entries: [&[u8]; 2] = [b"text/html\0", b"image/png\0"];
        let mime_list_len: usize = mime_entries.iter().map(|s| s.len()).sum::<usize>() + 1;
        let body_start = HEADER_SIZE + mime_list_len;

        let path_ptr_pos = body_start as u64;
        let title_ptr_pos = (body_start + 16) as u64;
        let cluster_ptr_pos = (body_start + 32) as u64;
        let padding_end = body_start + 64;
        let checksum_pos = padding_end as u64;
        let file_len = padding_end + 16;

        let mut buf = Vec::with_capacity(file_len);
        buf.extend_from_slice(&ZIM_MAGIC.to_le_bytes());
        buf.extend_from_slice(&major.to_le_bytes());
        buf.extend_from_slice(&minor.to_le_bytes());
        buf.extend_from_slice(&[0x11; 16]);
        buf.extend_from_slice(&0u32.to_le_bytes()); // entry_count
        buf.extend_from_slice(&0u32.to_le_bytes()); // cluster_count
        buf.extend_from_slice(&path_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&title_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&cluster_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&80u64.to_le_bytes());
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        buf.extend_from_slice(&checksum_pos.to_le_bytes());
        assert_eq!(buf.len(), HEADER_SIZE);

        for entry in mime_entries {
            buf.extend_from_slice(entry);
        }
        buf.push(0);
        buf.resize(padding_end, 0);
        buf.extend_from_slice(&[0u8; 16]);
        assert_eq!(buf.len(), file_len);
        buf
    }

    fn write_tempfile(bytes: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(bytes).expect("write");
        f.flush().expect("flush");
        f
    }

    #[test]
    fn open_happy_path_v6_1() {
        let bytes = build_minimal_zim(6, 1);
        let f = write_tempfile(&bytes);
        let a = Archive::open(f.path()).expect("open");
        assert_eq!(a.header().major_version, 6);
        assert_eq!(a.header().minor_version, 1);
        assert_eq!(a.namespace_mode(), NamespaceMode::New);
        assert_eq!(
            a.mime_types(),
            &["text/html".to_string(), "image/png".to_string()]
        );
        assert_eq!(a.entry_count(), 0);
        assert_eq!(a.cluster_count(), 0);
        assert_eq!(a.path(), f.path());
        assert_eq!(a.mmap_len(), bytes.len());
    }

    #[test]
    fn open_happy_path_v5_is_legacy() {
        let bytes = build_minimal_zim(5, 0);
        let f = write_tempfile(&bytes);
        let a = Archive::open(f.path()).expect("open");
        assert_eq!(a.namespace_mode(), NamespaceMode::Legacy);
    }

    #[test]
    fn open_rejects_bad_magic() {
        let mut bytes = build_minimal_zim(6, 1);
        bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
        let f = write_tempfile(&bytes);
        assert!(matches!(
            Archive::open(f.path()),
            Err(Error::InvalidMagic(_))
        ));
    }

    #[test]
    fn open_rejects_truncated_file() {
        let f = write_tempfile(&[0u8; 10]);
        assert!(matches!(
            Archive::open(f.path()),
            Err(Error::TruncatedHeader(10))
        ));
    }

    #[test]
    fn open_rejects_checksum_pos_mismatch() {
        let mut bytes = build_minimal_zim(6, 1);
        let mut cp = u64::from_le_bytes(bytes[72..80].try_into().unwrap());
        cp -= 1;
        bytes[72..80].copy_from_slice(&cp.to_le_bytes());
        let f = write_tempfile(&bytes);
        assert!(matches!(
            Archive::open(f.path()),
            Err(Error::InvalidHeader {
                field: "checksum_pos",
                ..
            })
        ));
    }

    #[test]
    fn split_archive_is_rejected() {
        let tmpdir = tempfile::tempdir().unwrap();
        let p = tmpdir.path().join("wikipedia.zimaa");
        std::fs::write(&p, b"irrelevant").unwrap();
        assert!(matches!(
            Archive::open(&p),
            Err(Error::SplitArchiveNotSupported)
        ));
    }

    #[test]
    fn split_archive_detector() {
        assert!(is_split_archive_path(Path::new("foo.zimaa")));
        assert!(is_split_archive_path(Path::new("foo.zimzz")));
        assert!(!is_split_archive_path(Path::new("foo.zim")));
        assert!(!is_split_archive_path(Path::new("foo.zima")));
        assert!(!is_split_archive_path(Path::new("foo.ZIMAA")));
        assert!(!is_split_archive_path(Path::new("foo.zim00")));
    }

    // ── Phase 2: multi-entry synthetic ZIMs ──────────────────────────────

    #[derive(Clone, Debug)]
    enum EntryKind {
        Content {
            cluster: u32,
            blob: u32,
            mime_idx: u16,
        },
        Redirect {
            target_path_namespace: char,
            target_path: String,
        },
        Deprecated, // linktarget sentinel
    }

    #[derive(Clone, Debug)]
    struct EntrySpec {
        namespace: char,
        path: String,
        title: String, // "" means use path as title
        kind: EntryKind,
    }

    fn content(namespace: char, path: &str, title: &str, mime_idx: u16) -> EntrySpec {
        EntrySpec {
            namespace,
            path: path.into(),
            title: title.into(),
            kind: EntryKind::Content {
                cluster: 0,
                blob: 0,
                mime_idx,
            },
        }
    }

    fn redirect(
        namespace: char,
        path: &str,
        title: &str,
        target_ns: char,
        target: &str,
    ) -> EntrySpec {
        EntrySpec {
            namespace,
            path: path.into(),
            title: title.into(),
            kind: EntryKind::Redirect {
                target_path_namespace: target_ns,
                target_path: target.into(),
            },
        }
    }

    fn deprecated(namespace: char, path: &str) -> EntrySpec {
        EntrySpec {
            namespace,
            path: path.into(),
            title: String::new(),
            kind: EntryKind::Deprecated,
        }
    }

    /// Build a complete ZIM archive with the given entries.
    fn build_zim(major: u16, minor: u16, mimes: &[&str], entries: &[EntrySpec]) -> Vec<u8> {
        // 1. Sort entries by (namespace, path) — this is the path pointer order
        //    and also gives us the entry_index for each input entry.
        let mut indexed: Vec<(usize, EntrySpec)> = entries.iter().cloned().enumerate().collect();
        indexed.sort_by(|(_, a), (_, b)| {
            (a.namespace as u32, &a.path).cmp(&(b.namespace as u32, &b.path))
        });

        // Map from (namespace, path) → final entry_index in the sorted order.
        let mut path_to_idx = std::collections::HashMap::new();
        for (sorted_i, (_, e)) in indexed.iter().enumerate() {
            path_to_idx.insert((e.namespace, e.path.clone()), sorted_i as u32);
        }

        // 2. Encode each dirent (in sorted order) into a body buffer. Record
        //    each dirent's file offset (relative to the final file start,
        //    which we'll compute below).
        let mut dirent_bytes = Vec::<u8>::new();
        let mut dirent_offsets_in_body = Vec::<u64>::new(); // offsets within dirent_bytes

        for (_, e) in &indexed {
            dirent_offsets_in_body.push(dirent_bytes.len() as u64);
            match &e.kind {
                EntryKind::Content {
                    cluster,
                    blob,
                    mime_idx,
                } => {
                    dirent_bytes.extend_from_slice(&mime_idx.to_le_bytes());
                    dirent_bytes.push(0); // parameter_len
                    dirent_bytes.push(e.namespace as u8);
                    dirent_bytes.extend_from_slice(&0u32.to_le_bytes()); // revision
                    dirent_bytes.extend_from_slice(&cluster.to_le_bytes());
                    dirent_bytes.extend_from_slice(&blob.to_le_bytes());
                    dirent_bytes.extend_from_slice(e.path.as_bytes());
                    dirent_bytes.push(0);
                    dirent_bytes.extend_from_slice(e.title.as_bytes());
                    dirent_bytes.push(0);
                }
                EntryKind::Redirect {
                    target_path_namespace,
                    target_path,
                } => {
                    let target_idx = path_to_idx
                        .get(&(*target_path_namespace, target_path.clone()))
                        .copied()
                        .expect("redirect target must be one of the entries");
                    dirent_bytes.extend_from_slice(&0xFFFFu16.to_le_bytes());
                    dirent_bytes.push(0);
                    dirent_bytes.push(e.namespace as u8);
                    dirent_bytes.extend_from_slice(&0u32.to_le_bytes());
                    dirent_bytes.extend_from_slice(&target_idx.to_le_bytes());
                    dirent_bytes.extend_from_slice(e.path.as_bytes());
                    dirent_bytes.push(0);
                    dirent_bytes.extend_from_slice(e.title.as_bytes());
                    dirent_bytes.push(0);
                }
                EntryKind::Deprecated => {
                    dirent_bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
                    dirent_bytes.push(0);
                    dirent_bytes.push(e.namespace as u8);
                    dirent_bytes.extend_from_slice(&0u32.to_le_bytes());
                    dirent_bytes.extend_from_slice(&0u32.to_le_bytes());
                    dirent_bytes.extend_from_slice(&0u32.to_le_bytes());
                    dirent_bytes.extend_from_slice(e.path.as_bytes());
                    dirent_bytes.push(0);
                    dirent_bytes.push(0); // empty title
                }
            }
        }

        // 3. Build the MIME list bytes.
        let mut mime_bytes = Vec::new();
        for m in mimes {
            mime_bytes.extend_from_slice(m.as_bytes());
            mime_bytes.push(0);
        }
        mime_bytes.push(0); // terminator

        // 4. Layout: header | mime_list | path_ptr_list | title_ptr_list |
        //            dirents | cluster_ptr_list | checksum
        let entry_count = indexed.len();
        let mime_list_pos = HEADER_SIZE as u64;
        let path_ptr_pos = mime_list_pos + mime_bytes.len() as u64;
        let title_ptr_pos = path_ptr_pos + (entry_count as u64) * 8;
        let dirents_pos = title_ptr_pos + (entry_count as u64) * 4;
        let cluster_ptr_pos = dirents_pos + dirent_bytes.len() as u64;
        let checksum_pos = cluster_ptr_pos; // zero clusters in Phase-2 tests
        let file_len = checksum_pos + 16;

        // Path pointers: absolute file offsets to each dirent.
        let mut path_ptr_bytes = Vec::with_capacity(entry_count * 8);
        for off in &dirent_offsets_in_body {
            let abs = dirents_pos + off;
            path_ptr_bytes.extend_from_slice(&abs.to_le_bytes());
        }

        // Title pointers: entry indices sorted by (namespace, effective_title).
        // The sort order must match what read_title_sort_key would return
        // (empty stored title → path).
        let mut title_sort: Vec<(u32, (char, String))> = indexed
            .iter()
            .enumerate()
            .map(|(sorted_i, (_, e))| {
                let effective_title = if e.title.is_empty() {
                    e.path.clone()
                } else {
                    e.title.clone()
                };
                (sorted_i as u32, (e.namespace, effective_title))
            })
            .collect();
        title_sort.sort_by(|a, b| (a.1 .0 as u32, &a.1 .1).cmp(&(b.1 .0 as u32, &b.1 .1)));

        let mut title_ptr_bytes = Vec::with_capacity(entry_count * 4);
        for (entry_idx, _) in &title_sort {
            title_ptr_bytes.extend_from_slice(&entry_idx.to_le_bytes());
        }

        // Assemble the file.
        let mut buf = Vec::with_capacity(file_len as usize);
        buf.extend_from_slice(&ZIM_MAGIC.to_le_bytes());
        buf.extend_from_slice(&major.to_le_bytes());
        buf.extend_from_slice(&minor.to_le_bytes());
        buf.extend_from_slice(&[0x22; 16]);
        buf.extend_from_slice(&(entry_count as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // cluster_count
        buf.extend_from_slice(&path_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&title_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&cluster_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&mime_list_pos.to_le_bytes());
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // main_page
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // layout_page
        buf.extend_from_slice(&checksum_pos.to_le_bytes());
        assert_eq!(buf.len(), HEADER_SIZE);

        buf.extend_from_slice(&mime_bytes);
        assert_eq!(buf.len() as u64, path_ptr_pos);
        buf.extend_from_slice(&path_ptr_bytes);
        assert_eq!(buf.len() as u64, title_ptr_pos);
        buf.extend_from_slice(&title_ptr_bytes);
        assert_eq!(buf.len() as u64, dirents_pos);
        buf.extend_from_slice(&dirent_bytes);
        assert_eq!(buf.len() as u64, checksum_pos);
        buf.extend_from_slice(&[0u8; 16]);
        assert_eq!(buf.len() as u64, file_len);
        buf
    }

    fn open_bytes(bytes: &[u8]) -> (NamedTempFile, Archive) {
        let f = write_tempfile(bytes);
        let a = Archive::open(f.path()).expect("open");
        (f, a)
    }

    #[test]
    fn find_by_path_default_namespace_v6() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "Apple", "", 0),
                content('C', "Main_Page", "Main Page", 0),
                content('C', "Zebra", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(None, "Main_Page").unwrap().unwrap();
        match d {
            Dirent::Content(c) => {
                assert_eq!(c.namespace, 'C');
                assert_eq!(c.path, "Main_Page");
                assert_eq!(c.title, "Main Page");
            }
            _ => panic!("expected Content"),
        }
    }

    #[test]
    fn find_by_path_v5_default_is_article_a() {
        let bytes = build_zim(
            5,
            0,
            &["text/html"],
            &[
                content('A', "Main_Page", "", 0),
                content('I', "img.png", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(None, "Main_Page").unwrap().unwrap();
        assert_eq!(d.namespace(), 'A');
    }

    #[test]
    fn find_by_path_explicit_namespace() {
        let bytes = build_zim(
            5,
            0,
            &["image/png"],
            &[
                content('A', "article", "", 0),
                content('I', "img.png", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(Some('I'), "img.png").unwrap().unwrap();
        assert_eq!(d.namespace(), 'I');
        assert_eq!(d.path(), "img.png");
    }

    #[test]
    fn find_by_path_missing_returns_none() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[content('C', "Apple", "", 0), content('C', "Zebra", "", 0)],
        );
        let (_f, a) = open_bytes(&bytes);
        assert!(a.find_by_path(None, "Middle").unwrap().is_none());
        assert!(a.find_by_path(None, "Aardvark").unwrap().is_none()); // before all
        assert!(a.find_by_path(None, "Zzzzz").unwrap().is_none()); // after all
    }

    #[test]
    fn find_by_title_hits() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "page_a", "Alpha", 0),
                content('C', "page_b", "Bravo", 0),
                content('C', "page_c", "Charlie", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_title(None, "Bravo").unwrap().unwrap();
        assert_eq!(d.path(), "page_b");
        assert_eq!(d.title(), "Bravo");
    }

    #[test]
    fn search_title_prefix_returns_matches_in_title_order_with_limit() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "a", "Einstein", 0),
                content('C', "b", "Eiffel", 0),
                content('C', "c", "Edison", 0),
                content('C', "d", "Dante", 0),
                content('C', "e", "Eiger", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        // Prefix "Ei" → Eiffel, Eiger, Einstein in title-sort order.
        let got = a.search_title_prefix('C', "Ei", 10).unwrap();
        let titles: Vec<&str> = got.iter().map(|d| d.title()).collect();
        assert_eq!(titles, &["Eiffel", "Eiger", "Einstein"]);

        // Limit honoured.
        let got_limited = a.search_title_prefix('C', "Ei", 2).unwrap();
        assert_eq!(got_limited.len(), 2);
    }

    #[test]
    fn search_title_prefix_stops_at_namespace_boundary() {
        let bytes = build_zim(
            5,
            0,
            &["text/html"],
            &[
                content('A', "a", "Xavier", 0),
                content('I', "b", "Xenon.png", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let got = a.search_title_prefix('A', "X", 10).unwrap();
        let titles: Vec<&str> = got.iter().map(|d| d.title()).collect();
        assert_eq!(titles, &["Xavier"]); // Xenon.png is in namespace 'I'
    }

    #[test]
    fn entries_skips_deprecated() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "a", "", 0),
                deprecated('C', "b_gone"),
                content('C', "c", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        assert_eq!(a.entry_count(), 3);
        let collected: Vec<_> = a.entries().collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(collected.len(), 2);
        let paths: Vec<&str> = collected.iter().map(|d| d.path()).collect();
        assert_eq!(paths, &["a", "c"]);
    }

    #[test]
    fn articles_filters_redirects_and_non_article_namespace() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "Main", "", 0),
                redirect('C', "Old", "", 'C', "Main"),
                content('M', "Title", "", 0), // metadata namespace
                content('C', "Other", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let titles: Vec<String> = a
            .articles()
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .iter()
            .map(|c| c.path.clone())
            .collect();
        assert_eq!(titles, vec!["Main", "Other"]);
    }

    #[test]
    fn dirent_at_rejects_out_of_range() {
        let bytes = build_zim(6, 1, &["text/html"], &[content('C', "a", "", 0)]);
        let (_f, a) = open_bytes(&bytes);
        assert!(matches!(
            a.dirent_at(1),
            Err(Error::OffsetOutOfBounds {
                field: "entry_idx",
                ..
            })
        ));
    }

    #[test]
    fn redirect_dirent_parses_correctly() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "Main_Page", "Main Page", 0),
                redirect('C', "Home", "Home", 'C', "Main_Page"),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(None, "Home").unwrap().unwrap();
        match d {
            Dirent::Redirect(RedirectEntry {
                namespace,
                path,
                redirect_index,
                ..
            }) => {
                assert_eq!(namespace, 'C');
                assert_eq!(path, "Home");
                // Main_Page comes first in (namespace, path) sort order (H > M... wait "Home" < "Main_Page"),
                // so Home is index 0 and Main_Page is index 1.
                // Actually "H" (0x48) < "M" (0x4D), so Home is at sorted index 0, Main_Page at 1.
                assert_eq!(redirect_index, 1);
            }
            _ => panic!("expected Redirect"),
        }
    }

    #[test]
    fn search_with_deprecated_at_midpoint() {
        // Three entries: "b" is deprecated but its sort key is still well-defined.
        // Searching for "c" must not be confused by the deprecated midpoint.
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                content('C', "a", "", 0),
                deprecated('C', "b"),
                content('C', "c", "", 0),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(None, "c").unwrap().unwrap();
        assert_eq!(d.path(), "c");
        // And "b" returns None because dirent_at returns Ok(None) for deprecated.
        assert!(a.find_by_path(None, "b").unwrap().is_none());
    }
}
