use crate::error::{Error, Result};
use crate::util::{read_u16_le, read_u32_le, read_u64_le};

pub(crate) const HEADER_SIZE: usize = 80;
pub(crate) const ZIM_MAGIC: u32 = 0x044D_495A;
const U32_ABSENT: u32 = 0xFFFF_FFFF;

/// The 80-byte ZIM file header.
///
/// Parsed once at archive open. All offsets are byte positions from the
/// start of the file.
#[derive(Debug, Clone)]
pub struct Header {
    /// Fixed magic number `0x044D495A` identifying the file as a ZIM archive.
    pub magic_number: u32,
    /// Major version. Supported values are `5` and `6`.
    pub major_version: u16,
    /// Minor version. On major `6`, values `>= 1` select the "New" namespace
    /// convention (see [`crate::NamespaceMode`]).
    pub minor_version: u16,
    /// Archive-wide UUID. Stable across copies of the same archive.
    pub uuid: [u8; 16],
    /// Number of directory entries in the path and title pointer lists.
    pub entry_count: u32,
    /// Number of clusters in the cluster pointer list.
    pub cluster_count: u32,
    /// File offset of the path pointer list (u64 entries, one per dirent).
    pub path_ptr_pos: u64,
    /// File offset of the title pointer list (u32 entries, each indexing
    /// into the path pointer list).
    pub title_ptr_pos: u64,
    /// File offset of the cluster pointer list (u64 entries, one per cluster).
    pub cluster_ptr_pos: u64,
    /// File offset of the MIME type list. Always `80` — the MIME list
    /// immediately follows the header.
    pub mime_list_pos: u64,
    /// Entry index of the main page, or `None` when the on-disk sentinel
    /// `0xFFFFFFFF` indicates no main page.
    pub main_page: Option<u32>,
    /// Entry index of the layout page, or `None` when the on-disk sentinel
    /// `0xFFFFFFFF` indicates no layout page.
    pub layout_page: Option<u32>,
    /// File offset of the 16-byte MD5 checksum trailer. Equal to
    /// `file_len - 16`.
    pub checksum_pos: u64,
}

impl Header {
    pub(crate) fn parse(buf: &[u8]) -> Result<Header> {
        if buf.len() < HEADER_SIZE {
            return Err(Error::TruncatedHeader(buf.len() as u64));
        }

        let magic_number = read_u32_le(buf, 0, "magic_number")?;
        if magic_number != ZIM_MAGIC {
            return Err(Error::InvalidMagic(magic_number));
        }

        let major_version = read_u16_le(buf, 4, "major_version")?;
        if major_version != 5 && major_version != 6 {
            return Err(Error::UnsupportedVersion(major_version));
        }

        let minor_version = read_u16_le(buf, 6, "minor_version")?;

        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&buf[8..24]);

        let entry_count = read_u32_le(buf, 24, "entry_count")?;
        let cluster_count = read_u32_le(buf, 28, "cluster_count")?;
        let path_ptr_pos = read_u64_le(buf, 32, "path_ptr_pos")?;
        let title_ptr_pos = read_u64_le(buf, 40, "title_ptr_pos")?;
        let cluster_ptr_pos = read_u64_le(buf, 48, "cluster_ptr_pos")?;
        let mime_list_pos = read_u64_le(buf, 56, "mime_list_pos")?;

        if mime_list_pos != HEADER_SIZE as u64 {
            return Err(Error::InvalidHeader {
                field: "mime_list_pos",
                reason: format!("expected 80, got {mime_list_pos}"),
            });
        }

        let main_page_raw = read_u32_le(buf, 64, "main_page")?;
        let layout_page_raw = read_u32_le(buf, 68, "layout_page")?;
        let checksum_pos = read_u64_le(buf, 72, "checksum_pos")?;

        Ok(Header {
            magic_number,
            major_version,
            minor_version,
            uuid,
            entry_count,
            cluster_count,
            path_ptr_pos,
            title_ptr_pos,
            cluster_ptr_pos,
            mime_list_pos,
            main_page: (main_page_raw != U32_ABSENT).then_some(main_page_raw),
            layout_page: (layout_page_raw != U32_ABSENT).then_some(layout_page_raw),
            checksum_pos,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_header_bytes() -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE);
        buf.extend_from_slice(&ZIM_MAGIC.to_le_bytes()); // 0..4   magic
        buf.extend_from_slice(&6u16.to_le_bytes()); // 4..6   major
        buf.extend_from_slice(&1u16.to_le_bytes()); // 6..8   minor
        buf.extend_from_slice(&[0xAB; 16]); // 8..24  uuid
        buf.extend_from_slice(&42u32.to_le_bytes()); // 24..28 entry_count
        buf.extend_from_slice(&7u32.to_le_bytes()); // 28..32 cluster_count
        buf.extend_from_slice(&200u64.to_le_bytes()); // 32..40 path_ptr_pos
        buf.extend_from_slice(&400u64.to_le_bytes()); // 40..48 title_ptr_pos
        buf.extend_from_slice(&600u64.to_le_bytes()); // 48..56 cluster_ptr_pos
        buf.extend_from_slice(&80u64.to_le_bytes()); // 56..64 mime_list_pos
        buf.extend_from_slice(&3u32.to_le_bytes()); // 64..68 main_page
        buf.extend_from_slice(&U32_ABSENT.to_le_bytes()); // 68..72 layout_page
        buf.extend_from_slice(&1024u64.to_le_bytes()); // 72..80 checksum_pos
        assert_eq!(buf.len(), HEADER_SIZE);
        buf
    }

    #[test]
    fn parses_good_header() {
        let buf = good_header_bytes();
        let h = Header::parse(&buf).unwrap();
        assert_eq!(h.magic_number, ZIM_MAGIC);
        assert_eq!(h.major_version, 6);
        assert_eq!(h.minor_version, 1);
        assert_eq!(h.uuid, [0xAB; 16]);
        assert_eq!(h.entry_count, 42);
        assert_eq!(h.cluster_count, 7);
        assert_eq!(h.path_ptr_pos, 200);
        assert_eq!(h.title_ptr_pos, 400);
        assert_eq!(h.cluster_ptr_pos, 600);
        assert_eq!(h.mime_list_pos, 80);
        assert_eq!(h.main_page, Some(3));
        assert_eq!(h.layout_page, None);
        assert_eq!(h.checksum_pos, 1024);
    }

    #[test]
    fn rejects_truncated() {
        let buf = vec![0u8; 50];
        assert!(matches!(
            Header::parse(&buf),
            Err(Error::TruncatedHeader(50))
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = good_header_bytes();
        buf[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        assert!(matches!(
            Header::parse(&buf),
            Err(Error::InvalidMagic(0xDEADBEEF))
        ));
    }

    #[test]
    fn rejects_version_4() {
        let mut buf = good_header_bytes();
        buf[4..6].copy_from_slice(&4u16.to_le_bytes());
        assert!(matches!(
            Header::parse(&buf),
            Err(Error::UnsupportedVersion(4))
        ));
    }

    #[test]
    fn rejects_version_7() {
        let mut buf = good_header_bytes();
        buf[4..6].copy_from_slice(&7u16.to_le_bytes());
        assert!(matches!(
            Header::parse(&buf),
            Err(Error::UnsupportedVersion(7))
        ));
    }

    #[test]
    fn rejects_bad_mime_list_pos() {
        let mut buf = good_header_bytes();
        buf[56..64].copy_from_slice(&81u64.to_le_bytes());
        assert!(matches!(
            Header::parse(&buf),
            Err(Error::InvalidHeader {
                field: "mime_list_pos",
                ..
            })
        ));
    }

    #[test]
    fn main_page_absent_sentinel_is_none() {
        let mut buf = good_header_bytes();
        buf[64..68].copy_from_slice(&U32_ABSENT.to_le_bytes());
        let h = Header::parse(&buf).unwrap();
        assert_eq!(h.main_page, None);
    }
}
