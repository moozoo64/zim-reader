use thiserror::Error;

/// Errors returned by `zim-reader`.
///
/// Variants cover I/O failures, malformed headers or dirents, out-of-range
/// offsets, decompression errors, and MD5 mismatch. The `#[error]` message
/// on each variant is the user-facing description.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid magic number: expected 0x044D495A, got 0x{0:08X}")]
    InvalidMagic(u32),

    #[error("Unsupported major version: {0} (supported: 5, 6)")]
    UnsupportedVersion(u16),

    #[error("Truncated header: file is too small ({0} bytes)")]
    TruncatedHeader(u64),

    #[error("Invalid header field '{field}': {reason}")]
    InvalidHeader { field: &'static str, reason: String },

    #[error("Offset out of file bounds: offset {offset} in field '{field}'")]
    OffsetOutOfBounds { offset: u64, field: &'static str },

    #[error("Invalid UTF-8 in string at offset {0}")]
    InvalidUtf8(u64),

    #[error("Unknown compression type: 0x{0:02X}")]
    UnknownCompression(u8),

    #[error("Blob {blob_number} out of range (cluster has {blob_count} blobs)")]
    BlobOutOfRange { blob_number: u32, blob_count: usize },

    #[error("Extended cluster encountered in v5 archive (illegal)")]
    ExtendedClusterInV5,

    #[error("Redirect loop detected at entry index {0}")]
    RedirectLoop(u32),

    #[error("Redirect target index {0} is out of range")]
    RedirectIndexOutOfRange(u32),

    #[error("MD5 checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("LZMA2 decompression failed: {0}")]
    LzmaDecompress(String),

    #[error("Zstandard decompression failed: {0}")]
    ZstdDecompress(String),

    #[error("Split ZIM archives are not yet supported")]
    SplitArchiveNotSupported,
}

pub type Result<T> = std::result::Result<T, Error>;
