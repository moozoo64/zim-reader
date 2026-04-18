use crate::archive::Archive;
use crate::dirent::ContentEntry;

/// A fully resolved article: its content dirent plus the decompressed blob
/// bytes. Produced by [`Archive::get_article`] and [`Archive::main_page`].
#[derive(Debug, Clone)]
pub struct Article {
    /// Content dirent this article resolves to. When the caller-provided
    /// path was a redirect, this is the final target after chain
    /// resolution, not the original alias.
    pub entry: ContentEntry,
    /// Decompressed blob bytes.
    pub data: Vec<u8>,
}

impl Article {
    /// Interpret `data` as UTF-8 text, returning `None` if invalid.
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    /// MIME string resolved through the archive's MIME table. Returns the
    /// empty string if the index is out of range.
    pub fn mime_type<'a>(&self, archive: &'a Archive) -> &'a str {
        archive
            .mime_types()
            .get(self.entry.mime_type_idx as usize)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// True if `data` is not valid UTF-8. Callers that need a mime-aware
    /// decision should inspect [`Article::mime_type`] directly.
    pub fn is_binary(&self) -> bool {
        std::str::from_utf8(&self.data).is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_entry(mime_idx: u16) -> ContentEntry {
        ContentEntry {
            mime_type_idx: mime_idx,
            namespace: 'C',
            revision: 0,
            cluster_number: 0,
            blob_number: 0,
            path: "p".into(),
            title: "p".into(),
        }
    }

    #[test]
    fn as_text_on_valid_utf8() {
        let a = Article {
            entry: fake_entry(0),
            data: b"<html></html>".to_vec(),
        };
        assert_eq!(a.as_text(), Some("<html></html>"));
        assert!(!a.is_binary());
    }

    #[test]
    fn as_text_on_invalid_utf8() {
        let a = Article {
            entry: fake_entry(0),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
        };
        assert_eq!(a.as_text(), None);
        assert!(a.is_binary());
    }
}
