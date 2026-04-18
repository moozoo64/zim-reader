use crate::error::Result;
use crate::util::{read_u32_le, read_u64_le};

/// Read the file offset of the dirent for entry `idx` from the path pointer
/// list. Each pointer is 8 bytes; the list has `entry_count` entries.
pub(crate) fn path_ptr(buf: &[u8], path_ptr_pos: u64, idx: u32) -> Result<u64> {
    let off = path_ptr_pos
        .checked_add((idx as u64) * 8)
        .expect("path pointer offset overflow");
    read_u64_le(buf, off as usize, "path_ptr")
}

/// Read the entry index stored at rank `idx` of the title pointer list. Each
/// pointer is 4 bytes; resolve the returned u32 through `path_ptr` to get a
/// file offset.
pub(crate) fn title_ptr(buf: &[u8], title_ptr_pos: u64, idx: u32) -> Result<u32> {
    let off = title_ptr_pos
        .checked_add((idx as u64) * 4)
        .expect("title pointer offset overflow");
    read_u32_le(buf, off as usize, "title_ptr")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn path_ptr_reads_correct_offset() {
        let mut buf = vec![0xAA; 16];
        buf.extend_from_slice(&100u64.to_le_bytes());
        buf.extend_from_slice(&200u64.to_le_bytes());
        buf.extend_from_slice(&300u64.to_le_bytes());
        assert_eq!(path_ptr(&buf, 16, 0).unwrap(), 100);
        assert_eq!(path_ptr(&buf, 16, 1).unwrap(), 200);
        assert_eq!(path_ptr(&buf, 16, 2).unwrap(), 300);
    }

    #[test]
    fn path_ptr_out_of_bounds() {
        let buf = vec![0u8; 16];
        assert!(matches!(
            path_ptr(&buf, 16, 0),
            Err(Error::OffsetOutOfBounds { .. })
        ));
    }

    #[test]
    fn title_ptr_reads_correct_offset() {
        let mut buf = vec![0xFF; 4];
        buf.extend_from_slice(&7u32.to_le_bytes());
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&42u32.to_le_bytes());
        assert_eq!(title_ptr(&buf, 4, 0).unwrap(), 7);
        assert_eq!(title_ptr(&buf, 4, 1).unwrap(), 3);
        assert_eq!(title_ptr(&buf, 4, 2).unwrap(), 42);
    }

    #[test]
    fn title_ptr_out_of_bounds() {
        let buf = vec![0u8; 4];
        assert!(matches!(
            title_ptr(&buf, 4, 0),
            Err(Error::OffsetOutOfBounds { .. })
        ));
    }
}
