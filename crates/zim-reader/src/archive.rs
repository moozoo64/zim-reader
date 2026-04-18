use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::{Mmap, MmapOptions};

use crate::error::{Error, Result};
use crate::header::{Header, HEADER_SIZE};
use crate::mime::parse_mime_table;
use crate::namespace::{detect_namespace_mode, NamespaceMode};

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
    #[allow(dead_code)] // read by later phases; held to keep pages mapped
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

    #[cfg(test)]
    pub(crate) fn mmap_len(&self) -> usize {
        self.mmap.len()
    }
}

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
    use crate::header::ZIM_MAGIC;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Build a synthetic, well-formed minimal ZIM file. Content after the
    /// MIME list is just padding; it's sized so that `checksum_pos + 16
    /// == file length`.
    fn build_minimal_zim(major: u16, minor: u16) -> Vec<u8> {
        let mime_entries: [&[u8]; 2] = [b"text/html\0", b"image/png\0"];
        let mime_list_len: usize = mime_entries.iter().map(|s| s.len()).sum::<usize>() + 1; // +1 terminator
        let body_start = HEADER_SIZE + mime_list_len;

        // Place pointer lists and clusters immediately after the mime list.
        // Phase 1 doesn't actually read them — we just need in-bounds offsets.
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
        buf.extend_from_slice(&[0x11; 16]); // uuid
        buf.extend_from_slice(&0u32.to_le_bytes()); // entry_count
        buf.extend_from_slice(&0u32.to_le_bytes()); // cluster_count
        buf.extend_from_slice(&path_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&title_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&cluster_ptr_pos.to_le_bytes());
        buf.extend_from_slice(&80u64.to_le_bytes()); // mime_list_pos
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // main_page absent
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // layout_page absent
        buf.extend_from_slice(&checksum_pos.to_le_bytes());
        assert_eq!(buf.len(), HEADER_SIZE);

        for entry in mime_entries {
            buf.extend_from_slice(entry);
        }
        buf.push(0); // MIME list terminator

        buf.resize(padding_end, 0); // placeholder body
        buf.extend_from_slice(&[0u8; 16]); // checksum placeholder
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
        // Shift checksum_pos forward by 1 so the invariant (checksum_pos + 16
        // == file_len) breaks.
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
}
