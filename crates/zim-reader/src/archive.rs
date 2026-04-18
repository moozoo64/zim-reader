use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use memmap2::{Mmap, MmapOptions};

use crate::article::Article;
use crate::cluster::{decompress, extract_blob, ClusterInfo};
use crate::dirent::{read_path_sort_key, read_title_sort_key, ContentEntry, Dirent, RedirectEntry};
use crate::error::{Error, Result};
use crate::header::{Header, HEADER_SIZE};
use crate::mime::parse_mime_table;
use crate::namespace::{
    article_namespace, detect_namespace_mode, metadata_namespace, NamespaceMode,
};
use crate::pointer_list::{cluster_ptr, path_ptr, title_ptr};

const MAX_REDIRECT_DEPTH: usize = 8;

pub(crate) struct ClusterData {
    pub bytes: Vec<u8>,
    pub extended: bool,
}

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
    cluster_cache: Mutex<LruCache<u32, Arc<ClusterData>>>,
}

impl Archive {
    /// Open a ZIM archive with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, ArchiveOptions::default())
    }

    /// Open a ZIM archive with explicit options.
    pub fn open_with_options(path: impl AsRef<Path>, opts: ArchiveOptions) -> Result<Self> {
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

        let cap = NonZeroUsize::new(opts.cluster_cache_size.max(1)).unwrap();
        let cluster_cache = Mutex::new(LruCache::new(cap));

        Ok(Archive {
            mmap,
            path: path.to_path_buf(),
            header,
            mime_types,
            namespace_mode,
            cluster_cache,
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

    // ── Content retrieval ────────────────────────────────────────────────

    /// Retrieve the raw blob bytes for a content entry.
    pub fn get_blob(&self, entry: &ContentEntry) -> Result<Vec<u8>> {
        let cluster = self.get_cluster_data(entry.cluster_number)?;
        extract_blob(&cluster.bytes, entry.blob_number, cluster.extended)
    }

    /// Follow a redirect chain to its final content entry. Returns
    /// [`Error::RedirectLoop`] on a cycle or after [`MAX_REDIRECT_DEPTH`]
    /// hops, and [`Error::RedirectIndexOutOfRange`] when the chain lands on
    /// a missing or deprecated entry.
    pub fn resolve_redirect(&self, entry: &RedirectEntry) -> Result<ContentEntry> {
        let mut visited: HashSet<u32> = HashSet::new();
        let mut current = entry.redirect_index;
        for _ in 0..MAX_REDIRECT_DEPTH {
            if !visited.insert(current) {
                return Err(Error::RedirectLoop(current));
            }
            if current >= self.header.entry_count {
                return Err(Error::RedirectIndexOutOfRange(current));
            }
            match self.dirent_at(current)? {
                Some(Dirent::Content(c)) => return Ok(c),
                Some(Dirent::Redirect(r)) => current = r.redirect_index,
                None => return Err(Error::RedirectIndexOutOfRange(current)),
            }
        }
        Err(Error::RedirectLoop(current))
    }

    /// Look up an article by `path` in the archive's article namespace,
    /// transparently following any redirect, and return its decompressed
    /// bytes wrapped in an [`Article`]. Returns `Ok(None)` if the path is
    /// not present.
    pub fn get_article(&self, path: &str) -> Result<Option<Article>> {
        let Some(dirent) = self.find_by_path(None, path)? else {
            return Ok(None);
        };
        self.article_from_dirent(dirent).map(Some)
    }

    /// Return the archive's main page, if one is declared in the header.
    pub fn main_page(&self) -> Result<Option<Article>> {
        let Some(idx) = self.header.main_page else {
            return Ok(None);
        };
        if idx >= self.header.entry_count {
            return Err(Error::RedirectIndexOutOfRange(idx));
        }
        let Some(dirent) = self.dirent_at(idx)? else {
            return Ok(None);
        };
        self.article_from_dirent(dirent).map(Some)
    }

    /// Read a metadata value by key (e.g. "Title", "Language"). Looks in
    /// namespace `M` on v6.1+ archives and `-` on v5. Returns `Ok(None)`
    /// when the key is absent.
    pub fn metadata(&self, key: &str) -> Result<Option<String>> {
        let ns = metadata_namespace(self.namespace_mode);
        let Some(dirent) = self.find_by_path(Some(ns), key)? else {
            return Ok(None);
        };
        let content = match dirent {
            Dirent::Content(c) => c,
            Dirent::Redirect(r) => self.resolve_redirect(&r)?,
        };
        let bytes = self.get_blob(&content)?;
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::InvalidUtf8(0))
    }

    fn article_from_dirent(&self, dirent: Dirent) -> Result<Article> {
        let content = match dirent {
            Dirent::Content(c) => c,
            Dirent::Redirect(r) => self.resolve_redirect(&r)?,
        };
        let data = self.get_blob(&content)?;
        Ok(Article {
            entry: content,
            data,
        })
    }

    fn get_cluster_data(&self, cluster_number: u32) -> Result<Arc<ClusterData>> {
        if cluster_number >= self.header.cluster_count {
            return Err(Error::OffsetOutOfBounds {
                offset: cluster_number as u64,
                field: "cluster_number",
            });
        }
        {
            let mut cache = self.cluster_cache.lock().unwrap();
            if let Some(data) = cache.get(&cluster_number) {
                return Ok(Arc::clone(data));
            }
        }

        let start = cluster_ptr(&self.mmap, self.header.cluster_ptr_pos, cluster_number)?;
        let end = if cluster_number + 1 < self.header.cluster_count {
            cluster_ptr(&self.mmap, self.header.cluster_ptr_pos, cluster_number + 1)?
        } else {
            self.header.checksum_pos
        };
        if start >= end || end > self.mmap.len() as u64 {
            return Err(Error::OffsetOutOfBounds {
                offset: end,
                field: "cluster_range",
            });
        }

        let info_byte = self.mmap[start as usize];
        let info = ClusterInfo::from_byte(info_byte, self.header.major_version)?;
        let payload = &self.mmap[(start as usize + 1)..end as usize];
        let bytes = decompress(&info, payload)?;
        let data = Arc::new(ClusterData {
            bytes,
            extended: info.extended,
        });

        let mut cache = self.cluster_cache.lock().unwrap();
        cache.put(cluster_number, Arc::clone(&data));
        Ok(data)
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

    #[allow(clippy::too_many_arguments)]
    fn content_at(
        namespace: char,
        path: &str,
        title: &str,
        mime_idx: u16,
        cluster: u32,
        blob: u32,
    ) -> EntrySpec {
        EntrySpec {
            namespace,
            path: path.into(),
            title: title.into(),
            kind: EntryKind::Content {
                cluster,
                blob,
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

    #[derive(Clone, Debug)]
    struct ClusterSpec {
        compression: u8,
        extended: bool,
        blobs: Vec<Vec<u8>>,
    }

    fn uncompressed_cluster(blobs: &[&[u8]]) -> ClusterSpec {
        ClusterSpec {
            compression: 0x00,
            extended: false,
            blobs: blobs.iter().map(|b| b.to_vec()).collect(),
        }
    }

    fn lzma2_cluster(blobs: &[&[u8]]) -> ClusterSpec {
        ClusterSpec {
            compression: 0x04,
            extended: false,
            blobs: blobs.iter().map(|b| b.to_vec()).collect(),
        }
    }

    fn zstd_cluster(blobs: &[&[u8]]) -> ClusterSpec {
        ClusterSpec {
            compression: 0x05,
            extended: false,
            blobs: blobs.iter().map(|b| b.to_vec()).collect(),
        }
    }

    fn encode_cluster(spec: &ClusterSpec) -> Vec<u8> {
        let n = spec.blobs.len();
        let offset_size: usize = if spec.extended { 8 } else { 4 };
        let header_bytes = ((n + 1) * offset_size) as u64;

        let mut payload = Vec::new();
        let mut running = header_bytes;
        for b in &spec.blobs {
            if spec.extended {
                payload.extend_from_slice(&running.to_le_bytes());
            } else {
                payload.extend_from_slice(&(running as u32).to_le_bytes());
            }
            running += b.len() as u64;
        }
        // sentinel offset
        if spec.extended {
            payload.extend_from_slice(&running.to_le_bytes());
        } else {
            payload.extend_from_slice(&(running as u32).to_le_bytes());
        }
        for b in &spec.blobs {
            payload.extend_from_slice(b);
        }

        let compressed = match spec.compression {
            0x00 | 0x01 => payload,
            0x04 => {
                use std::io::Write;
                use xz2::write::XzEncoder;
                let mut enc = XzEncoder::new(Vec::new(), 6);
                enc.write_all(&payload).unwrap();
                enc.finish().unwrap()
            }
            0x05 => zstd::encode_all(&payload[..], 3).unwrap(),
            other => panic!("unsupported test cluster compression: 0x{other:02X}"),
        };

        let info_byte = spec.compression | if spec.extended { 0x10 } else { 0 };
        let mut out = Vec::with_capacity(1 + compressed.len());
        out.push(info_byte);
        out.extend_from_slice(&compressed);
        out
    }

    /// Build a complete ZIM archive with the given entries (no clusters, no
    /// main page). Thin wrapper over [`build_zim_full`].
    fn build_zim(major: u16, minor: u16, mimes: &[&str], entries: &[EntrySpec]) -> Vec<u8> {
        build_zim_full(major, minor, mimes, entries, &[], None)
    }

    /// Build a complete ZIM archive. `main_page` is resolved by
    /// `(namespace, path)` against the sorted entries.
    fn build_zim_full(
        major: u16,
        minor: u16,
        mimes: &[&str],
        entries: &[EntrySpec],
        clusters: &[ClusterSpec],
        main_page: Option<(char, &str)>,
    ) -> Vec<u8> {
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

        // 4. Encode clusters and compute layout:
        //    header | mime_list | path_ptr_list | title_ptr_list |
        //    dirents | cluster_ptr_list | cluster_0 ... cluster_{N-1} | checksum
        let entry_count = indexed.len();
        let encoded_clusters: Vec<Vec<u8>> = clusters.iter().map(encode_cluster).collect();
        let cluster_count = encoded_clusters.len();

        let mime_list_pos = HEADER_SIZE as u64;
        let path_ptr_pos = mime_list_pos + mime_bytes.len() as u64;
        let title_ptr_pos = path_ptr_pos + (entry_count as u64) * 8;
        let dirents_pos = title_ptr_pos + (entry_count as u64) * 4;
        let cluster_ptr_pos = dirents_pos + dirent_bytes.len() as u64;
        let clusters_start = cluster_ptr_pos + (cluster_count as u64) * 8;

        let mut cluster_offsets = Vec::with_capacity(cluster_count);
        let mut running = clusters_start;
        for c in &encoded_clusters {
            cluster_offsets.push(running);
            running += c.len() as u64;
        }
        let checksum_pos = running;
        let file_len = checksum_pos + 16;

        let main_page_idx = main_page
            .and_then(|(ns, p)| path_to_idx.get(&(ns, p.to_string())).copied())
            .unwrap_or(0xFFFF_FFFFu32);

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
        buf.extend_from_slice(&(cluster_count as u32).to_le_bytes());
        buf.extend_from_slice(&path_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&title_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&cluster_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&mime_list_pos.to_le_bytes());
        buf.extend_from_slice(&main_page_idx.to_le_bytes());
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
        assert_eq!(buf.len() as u64, cluster_ptr_pos);
        for off in &cluster_offsets {
            buf.extend_from_slice(&off.to_le_bytes());
        }
        assert_eq!(buf.len() as u64, clusters_start);
        for c in &encoded_clusters {
            buf.extend_from_slice(c);
        }
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

    // ── Phase 3: blob retrieval, redirects, articles, metadata ───────────

    /// Pull the content dirent at the given path, expecting success.
    fn content_at_path(a: &Archive, ns: char, p: &str) -> ContentEntry {
        match a.find_by_path(Some(ns), p).unwrap().unwrap() {
            Dirent::Content(c) => c,
            Dirent::Redirect(_) => panic!("expected Content at {ns}/{p}, got redirect"),
        }
    }

    #[test]
    fn get_blob_uncompressed_cluster() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "one", "", 0, 0, 0),
                content_at('C', "two", "", 0, 0, 1),
            ],
            &[uncompressed_cluster(&[
                b"<html>one</html>",
                b"<html>two</html>",
            ])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let one = content_at_path(&a, 'C', "one");
        let two = content_at_path(&a, 'C', "two");
        assert_eq!(a.get_blob(&one).unwrap(), b"<html>one</html>");
        assert_eq!(a.get_blob(&two).unwrap(), b"<html>two</html>");
    }

    #[test]
    fn get_blob_lzma2_cluster() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "page", "", 0, 0, 0)],
            &[lzma2_cluster(&[b"lzma-compressed content"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let c = content_at_path(&a, 'C', "page");
        assert_eq!(a.get_blob(&c).unwrap(), b"lzma-compressed content");
    }

    #[test]
    fn get_blob_zstd_cluster() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "page", "", 0, 0, 0)],
            &[zstd_cluster(&[b"zstd-compressed content"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let c = content_at_path(&a, 'C', "page");
        assert_eq!(a.get_blob(&c).unwrap(), b"zstd-compressed content");
    }

    #[test]
    fn get_blob_extended_cluster_on_v6() {
        let spec = ClusterSpec {
            compression: 0x00,
            extended: true,
            blobs: vec![b"aa".to_vec(), b"bbbb".to_vec()],
        };
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "a", "", 0, 0, 0),
                content_at('C', "b", "", 0, 0, 1),
            ],
            &[spec],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let ca = content_at_path(&a, 'C', "a");
        let cb = content_at_path(&a, 'C', "b");
        assert_eq!(a.get_blob(&ca).unwrap(), b"aa");
        assert_eq!(a.get_blob(&cb).unwrap(), b"bbbb");
    }

    #[test]
    fn get_blob_extended_rejected_on_v5() {
        let spec = ClusterSpec {
            compression: 0x00,
            extended: true,
            blobs: vec![b"x".to_vec()],
        };
        let bytes = build_zim_full(
            5,
            0,
            &["text/html"],
            &[content_at('A', "p", "", 0, 0, 0)],
            &[spec],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let c = content_at_path(&a, 'A', "p");
        assert!(matches!(a.get_blob(&c), Err(Error::ExtendedClusterInV5)));
    }

    #[test]
    fn resolve_redirect_simple_chain() {
        // Home → Main_Page. Sorted: ["Home", "Main_Page"] → indices 0, 1.
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "Main_Page", "Main Page", 0, 0, 0),
                redirect('C', "Home", "Home", 'C', "Main_Page"),
            ],
            &[uncompressed_cluster(&[b"main page html"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let d = a.find_by_path(None, "Home").unwrap().unwrap();
        let Dirent::Redirect(r) = d else {
            panic!("expected redirect");
        };
        let resolved = a.resolve_redirect(&r).unwrap();
        assert_eq!(resolved.path, "Main_Page");
        assert_eq!(a.get_blob(&resolved).unwrap(), b"main page html");
    }

    #[test]
    fn resolve_redirect_two_hop_chain() {
        // r1 → r2 → c.
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "final", "", 0, 0, 0),
                redirect('C', "a_r1", "", 'C', "b_r2"),
                redirect('C', "b_r2", "", 'C', "final"),
            ],
            &[uncompressed_cluster(&[b"final bytes"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let Dirent::Redirect(r) = a.find_by_path(None, "a_r1").unwrap().unwrap() else {
            panic!("expected redirect");
        };
        let resolved = a.resolve_redirect(&r).unwrap();
        assert_eq!(resolved.path, "final");
    }

    #[test]
    fn resolve_redirect_cycle_is_detected() {
        // alpha → beta → alpha.
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                redirect('C', "alpha", "", 'C', "beta"),
                redirect('C', "beta", "", 'C', "alpha"),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let Dirent::Redirect(r) = a.find_by_path(None, "alpha").unwrap().unwrap() else {
            panic!("expected redirect");
        };
        assert!(matches!(
            a.resolve_redirect(&r),
            Err(Error::RedirectLoop(_))
        ));
    }

    #[test]
    fn resolve_redirect_target_is_deprecated() {
        let bytes = build_zim(
            6,
            1,
            &["text/html"],
            &[
                redirect('C', "alias", "", 'C', "gone"),
                deprecated('C', "gone"),
            ],
        );
        let (_f, a) = open_bytes(&bytes);
        let Dirent::Redirect(r) = a.find_by_path(None, "alias").unwrap().unwrap() else {
            panic!("expected redirect");
        };
        assert!(matches!(
            a.resolve_redirect(&r),
            Err(Error::RedirectIndexOutOfRange(_))
        ));
    }

    #[test]
    fn get_article_follows_redirect() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "Main_Page", "Main Page", 0, 0, 0),
                redirect('C', "Home", "Home", 'C', "Main_Page"),
            ],
            &[uncompressed_cluster(&[b"<h1>Main</h1>"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let art = a.get_article("Home").unwrap().unwrap();
        assert_eq!(art.entry.path, "Main_Page");
        assert_eq!(art.data, b"<h1>Main</h1>");
        assert_eq!(art.as_text(), Some("<h1>Main</h1>"));
        assert_eq!(art.mime_type(&a), "text/html");
        assert!(!art.is_binary());
    }

    #[test]
    fn get_article_missing_returns_none() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "Exists", "", 0, 0, 0)],
            &[uncompressed_cluster(&[b"x"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        assert!(a.get_article("Missing").unwrap().is_none());
    }

    #[test]
    fn main_page_when_set() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "Apple", "", 0, 0, 0),
                content_at('C', "Main_Page", "", 0, 0, 1),
            ],
            &[uncompressed_cluster(&[b"apple", b"main page body"])],
            Some(('C', "Main_Page")),
        );
        let (_f, a) = open_bytes(&bytes);
        let mp = a.main_page().unwrap().unwrap();
        assert_eq!(mp.entry.path, "Main_Page");
        assert_eq!(mp.data, b"main page body");
    }

    #[test]
    fn main_page_when_absent() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "only", "", 0, 0, 0)],
            &[uncompressed_cluster(&[b"only bytes"])],
            None, // main_page sentinel 0xFFFFFFFF
        );
        let (_f, a) = open_bytes(&bytes);
        assert!(a.main_page().unwrap().is_none());
    }

    #[test]
    fn metadata_v6_uses_m_namespace() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/plain"],
            &[content_at('M', "Title", "", 0, 0, 0)],
            &[uncompressed_cluster(&[b"Wikipedia"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        assert_eq!(a.metadata("Title").unwrap().as_deref(), Some("Wikipedia"));
        assert!(a.metadata("Missing").unwrap().is_none());
    }

    #[test]
    fn metadata_v5_uses_dash_namespace() {
        let bytes = build_zim_full(
            5,
            0,
            &["text/plain"],
            &[content_at('-', "Title", "", 0, 0, 0)],
            &[uncompressed_cluster(&[b"OldZim"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        assert_eq!(a.metadata("Title").unwrap().as_deref(), Some("OldZim"));
    }

    #[test]
    fn metadata_invalid_utf8_errors() {
        let bytes = build_zim_full(
            6,
            1,
            &["application/octet-stream"],
            &[content_at('M', "Raw", "", 0, 0, 0)],
            &[uncompressed_cluster(&[&[0xFF, 0xFE, 0xFD]])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        assert!(matches!(a.metadata("Raw"), Err(Error::InvalidUtf8(_))));
    }

    #[test]
    fn cache_smoke_get_blob_is_idempotent() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "p", "", 0, 0, 0)],
            &[zstd_cluster(&[b"cached"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let c = content_at_path(&a, 'C', "p");
        let first = a.get_blob(&c).unwrap();
        let second = a.get_blob(&c).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, b"cached");
    }

    #[test]
    fn get_blob_across_multiple_clusters() {
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[
                content_at('C', "a", "", 0, 0, 0),
                content_at('C', "b", "", 0, 1, 0),
                content_at('C', "c", "", 0, 1, 1),
            ],
            &[
                uncompressed_cluster(&[b"aaa"]),
                zstd_cluster(&[b"bbb", b"ccc"]),
            ],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        assert_eq!(a.get_blob(&content_at_path(&a, 'C', "a")).unwrap(), b"aaa");
        assert_eq!(a.get_blob(&content_at_path(&a, 'C', "b")).unwrap(), b"bbb");
        assert_eq!(a.get_blob(&content_at_path(&a, 'C', "c")).unwrap(), b"ccc");
    }

    #[test]
    fn get_blob_rejects_out_of_range_cluster() {
        // Construct a content entry whose cluster_number is 99 but only 1
        // cluster exists.  The dirent itself parses fine; get_blob must fail.
        let bytes = build_zim_full(
            6,
            1,
            &["text/html"],
            &[content_at('C', "p", "", 0, 99, 0)],
            &[uncompressed_cluster(&[b"x"])],
            None,
        );
        let (_f, a) = open_bytes(&bytes);
        let c = content_at_path(&a, 'C', "p");
        assert!(matches!(
            a.get_blob(&c),
            Err(Error::OffsetOutOfBounds {
                field: "cluster_number",
                ..
            })
        ));
    }
}
