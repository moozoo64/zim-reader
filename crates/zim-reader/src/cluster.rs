use crate::error::{Error, Result};
use crate::util::{read_u32_le, read_u64_le};

#[cfg(not(feature = "compression-pure"))]
compile_error!(
    "zim-reader requires at least one compression feature. \
     Enable `compression-pure`."
);

pub(crate) const UNCOMPRESSED_A: u8 = 0x00;
pub(crate) const UNCOMPRESSED_B: u8 = 0x01;
pub(crate) const COMPRESSION_LZMA2: u8 = 0x04;
pub(crate) const COMPRESSION_ZSTD: u8 = 0x05;

/// Decoded first byte of a cluster.
#[derive(Debug)]
pub(crate) struct ClusterInfo {
    pub compression: u8,
    pub extended: bool,
}

impl ClusterInfo {
    /// Decode a cluster info byte. Rejects the extended-offset flag on v5
    /// archives (spec §3.8).
    pub(crate) fn from_byte(byte: u8, major_version: u16) -> Result<Self> {
        let compression = byte & 0x0F;
        let extended = (byte & 0x10) != 0;
        if extended && major_version == 5 {
            return Err(Error::ExtendedClusterInV5);
        }
        Ok(Self {
            compression,
            extended,
        })
    }
}

/// Decompress a cluster payload (the bytes *after* the info byte).
pub(crate) fn decompress(info: &ClusterInfo, data: &[u8]) -> Result<Vec<u8>> {
    match info.compression {
        UNCOMPRESSED_A | UNCOMPRESSED_B => Ok(data.to_vec()),
        #[cfg(feature = "compression-pure")]
        COMPRESSION_LZMA2 => {
            let mut cursor = std::io::Cursor::new(data);
            let mut out = Vec::new();
            lzma_rs::xz_decompress(&mut cursor, &mut out)
                .map_err(|e| Error::LzmaDecompress(e.to_string()))?;
            Ok(out)
        }
        #[cfg(feature = "compression-pure")]
        COMPRESSION_ZSTD => {
            use std::io::Read;
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(data)
                .map_err(|e| Error::ZstdDecompress(e.to_string()))?;
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::ZstdDecompress(e.to_string()))?;
            Ok(out)
        }
        other => Err(Error::UnknownCompression(other)),
    }
}

/// Extract one blob from an already-decompressed cluster.
///
/// Reads offsets (u32 when `extended=false`, u64 when `extended=true`) from
/// the start of `decompressed`, derives `blob_count = first_offset /
/// offset_size`, bounds-checks `blob_number`, then copies out the slice
/// `decompressed[start..end]`.
pub(crate) fn extract_blob(
    decompressed: &[u8],
    blob_number: u32,
    extended: bool,
) -> Result<Vec<u8>> {
    let offset_size: usize = if extended { 8 } else { 4 };
    let read_off = |i: usize| -> Result<u64> {
        if extended {
            read_u64_le(decompressed, i * offset_size, "cluster_offset")
        } else {
            Ok(read_u32_le(decompressed, i * offset_size, "cluster_offset")? as u64)
        }
    };

    let first = read_off(0)? as usize;
    if first < offset_size || !first.is_multiple_of(offset_size) {
        return Err(Error::OffsetOutOfBounds {
            offset: first as u64,
            field: "cluster_first_offset",
        });
    }
    // There are (first / offset_size) offsets in the header — one for each of
    // `n` blobs plus a sentinel at the end — so `n = first / offset_size - 1`.
    let blob_count = (first / offset_size) - 1;
    if (blob_number as usize) >= blob_count {
        return Err(Error::BlobOutOfRange {
            blob_number,
            blob_count,
        });
    }

    let start = read_off(blob_number as usize)? as usize;
    let end = read_off(blob_number as usize + 1)? as usize;
    if start > end || end > decompressed.len() {
        return Err(Error::OffsetOutOfBounds {
            offset: end as u64,
            field: "blob_end",
        });
    }
    Ok(decompressed[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a standard (u32-offset) uncompressed cluster payload from blobs.
    /// Returns the bytes that would appear *after* the info byte.
    fn build_cluster_payload_u32(blobs: &[&[u8]]) -> Vec<u8> {
        let n = blobs.len();
        let offset_size = 4usize;
        let mut out = Vec::new();
        let header_bytes = (n + 1) * offset_size;
        let mut running = header_bytes as u32;
        for b in blobs {
            out.extend_from_slice(&running.to_le_bytes());
            running += b.len() as u32;
        }
        out.extend_from_slice(&running.to_le_bytes()); // sentinel end
        for b in blobs {
            out.extend_from_slice(b);
        }
        out
    }

    /// Same as `build_cluster_payload_u32` but with u64 offsets (extended).
    fn build_cluster_payload_u64(blobs: &[&[u8]]) -> Vec<u8> {
        let n = blobs.len();
        let offset_size = 8usize;
        let mut out = Vec::new();
        let header_bytes = (n + 1) * offset_size;
        let mut running = header_bytes as u64;
        for b in blobs {
            out.extend_from_slice(&running.to_le_bytes());
            running += b.len() as u64;
        }
        out.extend_from_slice(&running.to_le_bytes());
        for b in blobs {
            out.extend_from_slice(b);
        }
        out
    }

    #[test]
    fn cluster_info_decodes_compression_and_extended_bits() {
        let a = ClusterInfo::from_byte(0x05, 6).unwrap();
        assert_eq!(a.compression, 0x05);
        assert!(!a.extended);

        let b = ClusterInfo::from_byte(0x14, 6).unwrap();
        assert_eq!(b.compression, 0x04);
        assert!(b.extended);

        // Reserved bits (0xE0) are ignored.
        let c = ClusterInfo::from_byte(0xE5, 6).unwrap();
        assert_eq!(c.compression, 0x05);
        assert!(!c.extended);
    }

    #[test]
    fn cluster_info_rejects_extended_on_v5() {
        let err = ClusterInfo::from_byte(0x14, 5).unwrap_err();
        assert!(matches!(err, Error::ExtendedClusterInV5));
    }

    #[test]
    fn decompress_uncompressed_round_trips() {
        for byte in [0x00u8, 0x01] {
            let info = ClusterInfo::from_byte(byte, 6).unwrap();
            let out = decompress(&info, b"raw bytes").unwrap();
            assert_eq!(out, b"raw bytes");
        }
    }

    #[test]
    fn decompress_lzma2_round_trips() {
        use std::io::Write;
        use xz2::write::XzEncoder;
        let mut enc = XzEncoder::new(Vec::new(), 6);
        enc.write_all(b"hello LZMA world").unwrap();
        let compressed = enc.finish().unwrap();

        let info = ClusterInfo::from_byte(0x04, 6).unwrap();
        let out = decompress(&info, &compressed).unwrap();
        assert_eq!(out, b"hello LZMA world");
    }

    #[test]
    fn decompress_zstd_round_trips() {
        let compressed = zstd::encode_all(&b"hello Zstd world"[..], 3).unwrap();
        let info = ClusterInfo::from_byte(0x05, 6).unwrap();
        let out = decompress(&info, &compressed).unwrap();
        assert_eq!(out, b"hello Zstd world");
    }

    #[test]
    fn decompress_unknown_compression() {
        let info = ClusterInfo::from_byte(0x02, 6).unwrap();
        assert!(matches!(
            decompress(&info, &[]),
            Err(Error::UnknownCompression(0x02))
        ));
    }

    #[test]
    fn extract_blob_standard_offsets() {
        let payload = build_cluster_payload_u32(&[b"aaaa", b"bbbbbb", b"c"]);
        assert_eq!(extract_blob(&payload, 0, false).unwrap(), b"aaaa");
        assert_eq!(extract_blob(&payload, 1, false).unwrap(), b"bbbbbb");
        assert_eq!(extract_blob(&payload, 2, false).unwrap(), b"c");
    }

    #[test]
    fn extract_blob_extended_offsets() {
        let payload = build_cluster_payload_u64(&[b"xx", b"yyyy", b"zzz"]);
        assert_eq!(extract_blob(&payload, 0, true).unwrap(), b"xx");
        assert_eq!(extract_blob(&payload, 1, true).unwrap(), b"yyyy");
        assert_eq!(extract_blob(&payload, 2, true).unwrap(), b"zzz");
    }

    #[test]
    fn extract_blob_out_of_range() {
        let payload = build_cluster_payload_u32(&[b"one", b"two"]);
        assert!(matches!(
            extract_blob(&payload, 2, false),
            Err(Error::BlobOutOfRange {
                blob_number: 2,
                blob_count: 2,
            })
        ));
    }

    #[test]
    fn extract_blob_empty_blobs_ok() {
        // Edge case: blobs are allowed to be empty (zero-length slices).
        let payload = build_cluster_payload_u32(&[b"", b"x", b""]);
        assert_eq!(extract_blob(&payload, 0, false).unwrap(), b"");
        assert_eq!(extract_blob(&payload, 1, false).unwrap(), b"x");
        assert_eq!(extract_blob(&payload, 2, false).unwrap(), b"");
    }
}
