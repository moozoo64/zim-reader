use crate::error::Result;
use crate::util::read_cstring;

/// Parse the MIME type list starting at `offset`. The list is a sequence of
/// null-terminated UTF-8 strings terminated by a lone null (empty string).
pub(crate) fn parse_mime_table(buf: &[u8], offset: u64) -> Result<Vec<String>> {
    let mut cursor = offset as usize;
    let mut types = Vec::new();
    loop {
        let (s, consumed) = read_cstring(buf, cursor)?;
        cursor += consumed;
        if s.is_empty() {
            break;
        }
        types.push(s);
    }
    Ok(types)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn parses_two_entries() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"text/html\0");
        buf.extend_from_slice(b"image/png\0");
        buf.push(0); // terminator
        let types = parse_mime_table(&buf, 0).unwrap();
        assert_eq!(
            types,
            vec!["text/html".to_string(), "image/png".to_string()]
        );
    }

    #[test]
    fn parses_empty_list() {
        let buf = [0u8];
        let types = parse_mime_table(&buf, 0).unwrap();
        assert!(types.is_empty());
    }

    #[test]
    fn parses_with_offset() {
        let mut buf = vec![0xFF; 10];
        buf.extend_from_slice(b"text/plain\0");
        buf.push(0);
        let types = parse_mime_table(&buf, 10).unwrap();
        assert_eq!(types, vec!["text/plain".to_string()]);
    }

    #[test]
    fn rejects_invalid_utf8() {
        let buf = [0xFF, 0xFE, 0x00, 0x00];
        assert!(matches!(
            parse_mime_table(&buf, 0),
            Err(Error::InvalidUtf8(_))
        ));
    }

    #[test]
    fn rejects_missing_terminator() {
        let buf = b"text/html\0image/png"; // no trailing \0
        assert!(matches!(
            parse_mime_table(buf, 0),
            Err(Error::OffsetOutOfBounds { .. })
        ));
    }
}
