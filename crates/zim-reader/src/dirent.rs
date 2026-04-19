use crate::error::{Error, Result};
use crate::util::{read_cstring, read_u16_le, read_u32_le};

const MIME_REDIRECT: u16 = 0xFFFF;
const MIME_LINKTARGET: u16 = 0xFFFE;
const MIME_DELETED: u16 = 0xFFFD;

const CONTENT_PATH_OFFSET: usize = 16;
const REDIRECT_PATH_OFFSET: usize = 12;

/// A directory entry: either a content entry pointing at a blob in a
/// cluster, or a redirect pointing at another entry.
///
/// Deprecated entries (linktargets, deleted placeholders) are not
/// represented here — parsing code returns `None` for them, and callers
/// skip them.
#[derive(Debug, Clone)]
pub enum Dirent {
    /// A content entry: path + title + location of its blob.
    Content(ContentEntry),
    /// A redirect entry: path + title + index of the target dirent.
    Redirect(RedirectEntry),
}

/// A directory entry pointing at an actual piece of content.
#[derive(Debug, Clone)]
pub struct ContentEntry {
    /// Index into the archive's MIME table.
    pub mime_type_idx: u16,
    /// Single-character namespace (e.g. `C` in v6.1+, `A` in v5).
    pub namespace: char,
    /// Revision number, unused by most archives (typically `0`).
    pub revision: u32,
    /// Cluster holding this entry's blob.
    pub cluster_number: u32,
    /// Blob index within `cluster_number`.
    pub blob_number: u32,
    /// URL-safe path, unique within `namespace`.
    pub path: String,
    /// Human-readable title. Falls back to `path` when the stored title is empty.
    pub title: String,
}

/// A directory entry that redirects to another dirent.
#[derive(Debug, Clone)]
pub struct RedirectEntry {
    /// Namespace of this redirect (not of the target).
    pub namespace: char,
    /// Revision number, unused by most archives (typically `0`).
    pub revision: u32,
    /// Entry index of the redirect target in the path pointer list.
    pub redirect_index: u32,
    /// URL-safe path of this redirect.
    pub path: String,
    /// Human-readable title. Falls back to `path` when the stored title is empty.
    pub title: String,
}

impl Dirent {
    /// Namespace character of this entry.
    pub fn namespace(&self) -> char {
        match self {
            Dirent::Content(c) => c.namespace,
            Dirent::Redirect(r) => r.namespace,
        }
    }

    /// URL-safe path of this entry.
    pub fn path(&self) -> &str {
        match self {
            Dirent::Content(c) => &c.path,
            Dirent::Redirect(r) => &r.path,
        }
    }

    /// Human-readable title of this entry (falls back to path when absent).
    pub fn title(&self) -> &str {
        match self {
            Dirent::Content(c) => &c.title,
            Dirent::Redirect(r) => &r.title,
        }
    }

    /// Parse a directory entry starting at `offset`. Returns `Ok(None)` for
    /// deprecated entries (`mime_type_idx` == 0xFFFE or 0xFFFD), which the
    /// caller should skip.
    pub(crate) fn parse_at(buf: &[u8], offset: u64, mime_count: usize) -> Result<Option<Dirent>> {
        let base = offset as usize;
        let mime_type_idx = read_u16_le(buf, base, "mime_type_idx")?;

        if mime_type_idx == MIME_LINKTARGET || mime_type_idx == MIME_DELETED {
            return Ok(None);
        }

        // parameter_len at base+2 is not needed for parsing; extra_data follows
        // the title and is skipped implicitly.
        let namespace = read_namespace(buf, base + 3)?;
        let revision = read_u32_le(buf, base + 4, "revision")?;

        if mime_type_idx == MIME_REDIRECT {
            let redirect_index = read_u32_le(buf, base + 8, "redirect_index")?;
            let (path, path_len) = read_cstring(buf, base + REDIRECT_PATH_OFFSET)?;
            let (title_raw, _) = read_cstring(buf, base + REDIRECT_PATH_OFFSET + path_len)?;
            let title = if title_raw.is_empty() {
                path.clone()
            } else {
                title_raw
            };
            return Ok(Some(Dirent::Redirect(RedirectEntry {
                namespace,
                revision,
                redirect_index,
                path,
                title,
            })));
        }

        if (mime_type_idx as usize) >= mime_count {
            return Err(Error::InvalidHeader {
                field: "dirent.mime_type_idx",
                reason: format!(
                    "mime index {mime_type_idx} out of range (archive has {mime_count} types)"
                ),
            });
        }

        let cluster_number = read_u32_le(buf, base + 8, "cluster_number")?;
        let blob_number = read_u32_le(buf, base + 12, "blob_number")?;
        let (path, path_len) = read_cstring(buf, base + CONTENT_PATH_OFFSET)?;
        let (title_raw, _) = read_cstring(buf, base + CONTENT_PATH_OFFSET + path_len)?;
        let title = if title_raw.is_empty() {
            path.clone()
        } else {
            title_raw
        };

        Ok(Some(Dirent::Content(ContentEntry {
            mime_type_idx,
            namespace,
            revision,
            cluster_number,
            blob_number,
            path,
            title,
        })))
    }
}

/// Read just `(namespace, path)` at `offset`. Works uniformly for content,
/// redirect, and deprecated entries — all share the same prefix layout and
/// place the path CString at a position determined by `mime_type_idx`.
pub(crate) fn read_path_sort_key(buf: &[u8], offset: u64) -> Result<(char, String)> {
    let base = offset as usize;
    let mime_type_idx = read_u16_le(buf, base, "mime_type_idx")?;
    let namespace = read_namespace(buf, base + 3)?;
    let path_off = base + path_offset_for(mime_type_idx);
    let (path, _) = read_cstring(buf, path_off)?;
    Ok((namespace, path))
}

/// Read just `(namespace, title)` at `offset`. If the stored title CString is
/// empty, returns the path string instead (matching the canonical title rule).
pub(crate) fn read_title_sort_key(buf: &[u8], offset: u64) -> Result<(char, String)> {
    let base = offset as usize;
    let mime_type_idx = read_u16_le(buf, base, "mime_type_idx")?;
    let namespace = read_namespace(buf, base + 3)?;
    let path_off = base + path_offset_for(mime_type_idx);
    let (path, path_len) = read_cstring(buf, path_off)?;
    let (title, _) = read_cstring(buf, path_off + path_len)?;
    let effective = if title.is_empty() { path } else { title };
    Ok((namespace, effective))
}

fn path_offset_for(mime_type_idx: u16) -> usize {
    if mime_type_idx == MIME_REDIRECT {
        REDIRECT_PATH_OFFSET
    } else {
        CONTENT_PATH_OFFSET
    }
}

fn read_namespace(buf: &[u8], off: usize) -> Result<char> {
    let byte = *buf.get(off).ok_or(Error::OffsetOutOfBounds {
        offset: off as u64,
        field: "dirent.namespace",
    })?;
    if !byte.is_ascii_graphic() {
        return Err(Error::InvalidHeader {
            field: "dirent.namespace",
            reason: format!("non-graphic ASCII byte 0x{byte:02X}"),
        });
    }
    Ok(byte as char)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a content entry into `buf`, returning its start offset.
    #[allow(clippy::too_many_arguments)]
    fn write_content(
        buf: &mut Vec<u8>,
        mime: u16,
        namespace: char,
        revision: u32,
        cluster: u32,
        blob: u32,
        path: &str,
        title: &str,
    ) -> u64 {
        let start = buf.len() as u64;
        buf.extend_from_slice(&mime.to_le_bytes());
        buf.push(0); // parameter_len
        buf.push(namespace as u8);
        buf.extend_from_slice(&revision.to_le_bytes());
        buf.extend_from_slice(&cluster.to_le_bytes());
        buf.extend_from_slice(&blob.to_le_bytes());
        buf.extend_from_slice(path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(title.as_bytes());
        buf.push(0);
        start
    }

    fn write_redirect(
        buf: &mut Vec<u8>,
        namespace: char,
        revision: u32,
        target_idx: u32,
        path: &str,
        title: &str,
    ) -> u64 {
        let start = buf.len() as u64;
        buf.extend_from_slice(&MIME_REDIRECT.to_le_bytes());
        buf.push(0); // parameter_len
        buf.push(namespace as u8);
        buf.extend_from_slice(&revision.to_le_bytes());
        buf.extend_from_slice(&target_idx.to_le_bytes());
        buf.extend_from_slice(path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(title.as_bytes());
        buf.push(0);
        start
    }

    fn write_deprecated(buf: &mut Vec<u8>, mime: u16, namespace: char, path: &str) -> u64 {
        let start = buf.len() as u64;
        buf.extend_from_slice(&mime.to_le_bytes());
        buf.push(0);
        buf.push(namespace as u8);
        buf.extend_from_slice(&0u32.to_le_bytes()); // revision
        // Use content layout; ZIM stores deprecated entries with the same
        // shape, differing only in mime_type_idx.
        buf.extend_from_slice(&0u32.to_le_bytes()); // cluster
        buf.extend_from_slice(&0u32.to_le_bytes()); // blob
        buf.extend_from_slice(path.as_bytes());
        buf.push(0);
        buf.push(0); // empty title
        start
    }

    #[test]
    fn parses_content_entry() {
        let mut buf = Vec::new();
        let off = write_content(&mut buf, 0, 'C', 1, 5, 2, "Main_Page", "Main Page");
        let d = Dirent::parse_at(&buf, off, 4).unwrap().unwrap();
        match d {
            Dirent::Content(c) => {
                assert_eq!(c.mime_type_idx, 0);
                assert_eq!(c.namespace, 'C');
                assert_eq!(c.revision, 1);
                assert_eq!(c.cluster_number, 5);
                assert_eq!(c.blob_number, 2);
                assert_eq!(c.path, "Main_Page");
                assert_eq!(c.title, "Main Page");
            }
            other => panic!("expected Content, got {other:?}"),
        }
    }

    #[test]
    fn empty_title_falls_back_to_path() {
        let mut buf = Vec::new();
        let off = write_content(&mut buf, 0, 'C', 0, 0, 0, "article", "");
        let d = Dirent::parse_at(&buf, off, 1).unwrap().unwrap();
        assert_eq!(d.title(), "article");
        assert_eq!(d.path(), "article");
    }

    #[test]
    fn parses_redirect_entry() {
        let mut buf = Vec::new();
        let off = write_redirect(&mut buf, 'C', 0, 42, "Old_Name", "");
        let d = Dirent::parse_at(&buf, off, 1).unwrap().unwrap();
        match d {
            Dirent::Redirect(r) => {
                assert_eq!(r.namespace, 'C');
                assert_eq!(r.redirect_index, 42);
                assert_eq!(r.path, "Old_Name");
                assert_eq!(r.title, "Old_Name"); // empty title fell back
            }
            other => panic!("expected Redirect, got {other:?}"),
        }
    }

    #[test]
    fn deprecated_entries_return_none() {
        let mut buf = Vec::new();
        let off1 = write_deprecated(&mut buf, MIME_LINKTARGET, 'C', "gone");
        let off2 = write_deprecated(&mut buf, MIME_DELETED, 'C', "gone2");
        assert!(Dirent::parse_at(&buf, off1, 1).unwrap().is_none());
        assert!(Dirent::parse_at(&buf, off2, 1).unwrap().is_none());
    }

    #[test]
    fn mime_index_out_of_range() {
        let mut buf = Vec::new();
        let off = write_content(&mut buf, 5, 'C', 0, 0, 0, "foo", "");
        let err = Dirent::parse_at(&buf, off, 3).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidHeader {
                field: "dirent.mime_type_idx",
                ..
            }
        ));
    }

    #[test]
    fn non_graphic_namespace_rejected() {
        let mut buf = Vec::new();
        let off = write_content(&mut buf, 0, '\0', 0, 0, 0, "foo", "");
        // Overwrite namespace byte with 0x00 directly (write_content used '\0' so this is already 0).
        let err = Dirent::parse_at(&buf, off, 1).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidHeader {
                field: "dirent.namespace",
                ..
            }
        ));
    }

    #[test]
    fn multibyte_utf8_in_path_and_title() {
        let mut buf = Vec::new();
        let off = write_content(&mut buf, 0, 'C', 0, 0, 0, "café", "Café 🎉");
        let d = Dirent::parse_at(&buf, off, 1).unwrap().unwrap();
        assert_eq!(d.path(), "café");
        assert_eq!(d.title(), "Café 🎉");
    }

    #[test]
    fn parameter_len_nonzero_does_not_affect_parse() {
        let mut buf = Vec::new();
        let start = buf.len() as u64;
        buf.extend_from_slice(&0u16.to_le_bytes()); // mime
        buf.push(4); // parameter_len = 4 (we don't read past title, so ignored)
        buf.push(b'C');
        buf.extend_from_slice(&0u32.to_le_bytes()); // revision
        buf.extend_from_slice(&0u32.to_le_bytes()); // cluster
        buf.extend_from_slice(&0u32.to_le_bytes()); // blob
        buf.extend_from_slice(b"p\0");
        buf.extend_from_slice(b"t\0");
        let d = Dirent::parse_at(&buf, start, 1).unwrap().unwrap();
        assert_eq!(d.path(), "p");
        assert_eq!(d.title(), "t");
    }

    #[test]
    fn sort_key_helpers_match_full_parse() {
        let mut buf = Vec::new();
        let c = write_content(&mut buf, 0, 'C', 0, 0, 0, "Apple", "Apple pie");
        let r = write_redirect(&mut buf, 'C', 0, 0, "Zebra", "");
        let dep = write_deprecated(&mut buf, MIME_LINKTARGET, 'C', "Mango");

        assert_eq!(read_path_sort_key(&buf, c).unwrap(), ('C', "Apple".into()));
        assert_eq!(read_path_sort_key(&buf, r).unwrap(), ('C', "Zebra".into()));
        assert_eq!(
            read_path_sort_key(&buf, dep).unwrap(),
            ('C', "Mango".into())
        );

        assert_eq!(
            read_title_sort_key(&buf, c).unwrap(),
            ('C', "Apple pie".into())
        );
        // empty title → falls back to path
        assert_eq!(read_title_sort_key(&buf, r).unwrap(), ('C', "Zebra".into()));
        assert_eq!(
            read_title_sort_key(&buf, dep).unwrap(),
            ('C', "Mango".into())
        );
    }
}
