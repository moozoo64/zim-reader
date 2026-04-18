use crate::error::{Error, Result};

#[inline]
fn slice<'a>(buf: &'a [u8], off: usize, len: usize, field: &'static str) -> Result<&'a [u8]> {
    buf.get(off..off + len).ok_or(Error::OffsetOutOfBounds {
        offset: off as u64,
        field,
    })
}

pub(crate) fn read_u16_le(buf: &[u8], off: usize, field: &'static str) -> Result<u16> {
    let s = slice(buf, off, 2, field)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

pub(crate) fn read_u32_le(buf: &[u8], off: usize, field: &'static str) -> Result<u32> {
    let s = slice(buf, off, 4, field)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn read_u64_le(buf: &[u8], off: usize, field: &'static str) -> Result<u64> {
    let s = slice(buf, off, 8, field)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Read a null-terminated UTF-8 string starting at `off`. Returns the decoded
/// string (excluding the null byte) and the total number of bytes consumed
/// (including the null byte).
pub(crate) fn read_cstring(buf: &[u8], off: usize) -> Result<(String, usize)> {
    let rest = buf.get(off..).ok_or(Error::OffsetOutOfBounds {
        offset: off as u64,
        field: "cstring",
    })?;
    let nul = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or(Error::OffsetOutOfBounds {
            offset: off as u64,
            field: "cstring (no null terminator)",
        })?;
    let s = std::str::from_utf8(&rest[..nul])
        .map_err(|_| Error::InvalidUtf8(off as u64))?
        .to_owned();
    Ok((s, nul + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_integers_little_endian() {
        let buf = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u16_le(&buf, 0, "x").unwrap(), 0x0201);
        assert_eq!(read_u32_le(&buf, 0, "x").unwrap(), 0x04030201);
        assert_eq!(read_u64_le(&buf, 0, "x").unwrap(), 0x0807060504030201);
    }

    #[test]
    fn read_u32_out_of_bounds() {
        let buf = [0x01, 0x02, 0x03];
        let err = read_u32_le(&buf, 0, "my_field").unwrap_err();
        match err {
            Error::OffsetOutOfBounds { field, .. } => assert_eq!(field, "my_field"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn cstring_empty() {
        let buf = b"\0rest";
        let (s, n) = read_cstring(buf, 0).unwrap();
        assert_eq!(s, "");
        assert_eq!(n, 1);
    }

    #[test]
    fn cstring_basic() {
        let buf = b"hello\0world\0";
        let (s, n) = read_cstring(buf, 0).unwrap();
        assert_eq!(s, "hello");
        assert_eq!(n, 6);
        let (s2, n2) = read_cstring(buf, n).unwrap();
        assert_eq!(s2, "world");
        assert_eq!(n2, 6);
    }

    #[test]
    fn cstring_multibyte_utf8() {
        let buf = "café\0".as_bytes();
        let (s, n) = read_cstring(buf, 0).unwrap();
        assert_eq!(s, "café");
        assert_eq!(n, buf.len());
    }

    #[test]
    fn cstring_missing_terminator() {
        let buf = b"no null here";
        assert!(matches!(
            read_cstring(buf, 0),
            Err(Error::OffsetOutOfBounds { .. })
        ));
    }

    #[test]
    fn cstring_invalid_utf8() {
        let buf = [0xFF, 0xFE, 0x00];
        assert!(matches!(read_cstring(&buf, 0), Err(Error::InvalidUtf8(_))));
    }
}
