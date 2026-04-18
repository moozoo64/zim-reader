use crate::header::Header;

/// Which namespace convention an archive uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceMode {
    /// v6.1+ unified namespace (`C` for content, `M` for metadata, `W` for well-known).
    New,
    /// v5 / pre-2021 multi-namespace (`A` for articles, `I` for images, etc.).
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// `C` in New mode, `A` in Legacy mode.
    Content,
    /// `I` (Legacy only).
    Images,
    /// `M` in New mode, `-` in Legacy mode.
    Metadata,
    /// `W` in New mode (absent in Legacy).
    WellKnown,
    /// `X` — Xapian full-text search index.
    Search,
    Other(char),
}

pub(crate) fn detect_namespace_mode(header: &Header) -> NamespaceMode {
    if header.major_version == 6 && header.minor_version >= 1 {
        NamespaceMode::New
    } else {
        NamespaceMode::Legacy
    }
}

/// The namespace character used for user-facing articles in `mode`.
pub fn article_namespace(mode: NamespaceMode) -> char {
    match mode {
        NamespaceMode::New => 'C',
        NamespaceMode::Legacy => 'A',
    }
}

/// The namespace character used for metadata entries in `mode`.
pub fn metadata_namespace(mode: NamespaceMode) -> char {
    match mode {
        NamespaceMode::New => 'M',
        NamespaceMode::Legacy => '-',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_with_version(major: u16, minor: u16) -> Header {
        Header {
            magic_number: 0x044D_495A,
            major_version: major,
            minor_version: minor,
            uuid: [0; 16],
            entry_count: 0,
            cluster_count: 0,
            path_ptr_pos: 0,
            title_ptr_pos: 0,
            cluster_ptr_pos: 0,
            mime_list_pos: 80,
            main_page: None,
            layout_page: None,
            checksum_pos: 0,
        }
    }

    #[test]
    fn v6_1_is_new() {
        assert_eq!(
            detect_namespace_mode(&header_with_version(6, 1)),
            NamespaceMode::New
        );
    }

    #[test]
    fn v6_0_is_legacy() {
        assert_eq!(
            detect_namespace_mode(&header_with_version(6, 0)),
            NamespaceMode::Legacy
        );
    }

    #[test]
    fn v5_is_legacy() {
        assert_eq!(
            detect_namespace_mode(&header_with_version(5, 0)),
            NamespaceMode::Legacy
        );
    }

    #[test]
    fn namespace_helpers() {
        assert_eq!(article_namespace(NamespaceMode::New), 'C');
        assert_eq!(article_namespace(NamespaceMode::Legacy), 'A');
        assert_eq!(metadata_namespace(NamespaceMode::New), 'M');
        assert_eq!(metadata_namespace(NamespaceMode::Legacy), '-');
    }
}
