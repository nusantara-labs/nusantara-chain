use std::io::Read;

use crate::error::TpuError;

/// Tag byte prefixed to all wire messages.
const TAG_RAW: u8 = 0x00;
const TAG_ZSTD: u8 = 0x01;

/// Payloads at or below this size are sent raw (compression overhead not worth it).
const COMPRESSION_THRESHOLD: usize = 256;

/// Zstd compression level (3 = fast with good ratio).
const ZSTD_LEVEL: i32 = 3;

/// Maximum decompressed-to-wire ratio to guard against decompression bombs.
pub const MAX_DECOMPRESSION_RATIO: usize = 4;

/// Compress `data` with a 1-byte tag prefix.
pub fn compress(data: &[u8]) -> Result<Vec<u8>, TpuError> {
    if data.is_empty() {
        return Err(TpuError::Compression("empty payload".to_string()));
    }

    if data.len() <= COMPRESSION_THRESHOLD {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(TAG_RAW);
        out.extend_from_slice(data);
        return Ok(out);
    }

    let compressed =
        zstd::encode_all(data, ZSTD_LEVEL).map_err(|e| TpuError::Compression(e.to_string()))?;

    if compressed.len() >= data.len() {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(TAG_RAW);
        out.extend_from_slice(data);
        return Ok(out);
    }

    metrics::counter!("nusantara_wire_compressed_total").increment(1);
    let ratio = compressed.len() as f64 / data.len() as f64;
    metrics::histogram!("nusantara_wire_compression_ratio").record(ratio);

    let mut out = Vec::with_capacity(1 + compressed.len());
    out.push(TAG_ZSTD);
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Decompress a tagged payload produced by [`compress`].
/// `max_decompressed_size` limits the output to prevent decompression bombs.
pub fn decompress(tagged: &[u8], max_decompressed_size: usize) -> Result<Vec<u8>, TpuError> {
    if tagged.is_empty() {
        return Err(TpuError::Decompression("empty payload".to_string()));
    }

    match tagged[0] {
        TAG_RAW => {
            let raw = &tagged[1..];
            if raw.len() > max_decompressed_size {
                return Err(TpuError::Decompression(format!(
                    "raw payload size {} exceeds max {}",
                    raw.len(),
                    max_decompressed_size
                )));
            }
            Ok(raw.to_vec())
        }
        TAG_ZSTD => {
            let decoder = zstd::Decoder::new(&tagged[1..])
                .map_err(|e| TpuError::Decompression(e.to_string()))?;

            // Read one byte past the limit so we can detect overflow
            // without fully decompressing a bomb into memory.
            let limit = max_decompressed_size as u64 + 1;
            let mut limited = decoder.take(limit);

            let mut buf = Vec::new();
            limited
                .read_to_end(&mut buf)
                .map_err(|e| TpuError::Decompression(e.to_string()))?;

            if buf.len() > max_decompressed_size {
                metrics::counter!("nusantara_tpu_decompression_bomb_rejected").increment(1);
                return Err(TpuError::Decompression(format!(
                    "decompressed output exceeds limit of {max_decompressed_size} bytes"
                )));
            }
            Ok(buf)
        }
        tag => Err(TpuError::Decompression(format!(
            "unknown compression tag: 0x{tag:02x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data = vec![42u8; 1024];
        let compressed = compress(&data).unwrap();
        let decompressed = decompress(&compressed, 8192).unwrap();
        assert_eq!(data, decompressed);
    }

    #[test]
    fn below_threshold_raw() {
        let data = vec![1u8; 100];
        let compressed = compress(&data).unwrap();
        assert_eq!(compressed[0], TAG_RAW);
        assert_eq!(&compressed[1..], &data[..]);
    }

    #[test]
    fn decompression_bomb_guard() {
        let data = vec![0u8; 4096];
        let compressed = compress(&data).unwrap();
        // Allow only 100 bytes — should fail
        assert!(decompress(&compressed, 100).is_err());
    }

    #[test]
    fn empty_payload_error() {
        assert!(compress(&[]).is_err());
        assert!(decompress(&[], 1024).is_err());
    }

    #[test]
    fn invalid_tag_error() {
        let bad = vec![0xFF, 1, 2, 3];
        assert!(decompress(&bad, 1024).is_err());
    }

    #[test]
    fn raw_payload_exceeds_max() {
        // Build a raw-tagged payload larger than the limit
        let mut tagged = Vec::with_capacity(1 + 500);
        tagged.push(TAG_RAW);
        tagged.extend_from_slice(&vec![0xAB; 500]);

        let err = decompress(&tagged, 100).unwrap_err();
        assert!(
            err.to_string().contains("exceeds max"),
            "error should mention exceeds max: {err}"
        );
    }
}
