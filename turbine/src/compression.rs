use std::io::Read;

use crate::error::TurbineError;

/// Tag byte prefixed to all wire messages.
const TAG_RAW: u8 = 0x00;
const TAG_ZSTD: u8 = 0x01;

/// Payloads at or below this size are sent raw (compression overhead not worth it).
const COMPRESSION_THRESHOLD: usize = 256;

/// Zstd compression level (3 = fast with good ratio).
const ZSTD_LEVEL: i32 = 3;

/// Maximum allowed decompressed output size (4 MiB).
/// Prevents decompression bombs where a tiny compressed payload expands to gigabytes.
const MAX_DECOMPRESS_SIZE: usize = 4 * 1024 * 1024;

/// Compress `data` with a 1-byte tag prefix.
/// Returns `TAG_RAW || data` if below threshold or incompressible,
/// `TAG_ZSTD || zstd(data)` otherwise.
pub fn compress(data: &[u8]) -> Result<Vec<u8>, TurbineError> {
    if data.is_empty() {
        return Err(TurbineError::Compression("empty payload".to_string()));
    }

    if data.len() <= COMPRESSION_THRESHOLD {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(TAG_RAW);
        out.extend_from_slice(data);
        return Ok(out);
    }

    let compressed =
        zstd::encode_all(data, ZSTD_LEVEL).map_err(|e| TurbineError::Compression(e.to_string()))?;

    // Fallback: if compressed >= raw, send raw
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
///
/// Enforces a [`MAX_DECOMPRESS_SIZE`] limit to prevent decompression bombs
/// where a small compressed payload could expand to gigabytes of memory.
pub fn decompress(tagged: &[u8]) -> Result<Vec<u8>, TurbineError> {
    if tagged.is_empty() {
        return Err(TurbineError::Decompression("empty payload".to_string()));
    }

    match tagged[0] {
        TAG_RAW => Ok(tagged[1..].to_vec()),
        TAG_ZSTD => {
            let decoder = zstd::Decoder::new(&tagged[1..])
                .map_err(|e| TurbineError::Decompression(e.to_string()))?;

            // Allow reading one byte past the limit so we can detect overflow.
            let limit = MAX_DECOMPRESS_SIZE as u64 + 1;
            let mut limited = decoder.take(limit);

            let mut buf = Vec::new();
            limited
                .read_to_end(&mut buf)
                .map_err(|e| TurbineError::Decompression(e.to_string()))?;

            if buf.len() > MAX_DECOMPRESS_SIZE {
                metrics::counter!("nusantara_turbine_decompression_bomb_rejected").increment(1);
                return Err(TurbineError::Decompression(format!(
                    "decompressed output exceeds limit of {MAX_DECOMPRESS_SIZE} bytes"
                )));
            }

            Ok(buf)
        }
        tag => Err(TurbineError::Decompression(format!(
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
        let decompressed = decompress(&compressed).unwrap();
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
    fn incompressible_sent_raw() {
        // Random-ish data that won't compress well
        let data: Vec<u8> = (0..512).map(|i| (i * 7 + 13) as u8).collect();
        let compressed = compress(&data).unwrap();
        // Should either be raw or compressed — both are valid roundtrips
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(data, decompressed);
    }

    #[test]
    fn empty_payload_error() {
        assert!(compress(&[]).is_err());
        assert!(decompress(&[]).is_err());
    }

    #[test]
    fn invalid_tag_error() {
        let bad = vec![0xFF, 1, 2, 3];
        assert!(decompress(&bad).is_err());
    }

    #[test]
    fn decompression_bomb_rejected() {
        // Create a payload that compresses well but decompresses to > 4 MiB.
        // Repeating zeros compress extremely well with zstd.
        let oversized = vec![0u8; MAX_DECOMPRESS_SIZE + 1];
        let compressed =
            zstd::encode_all(oversized.as_slice(), ZSTD_LEVEL).expect("compression must succeed");

        // Prepend the TAG_ZSTD byte to simulate wire format
        let mut tagged = Vec::with_capacity(1 + compressed.len());
        tagged.push(TAG_ZSTD);
        tagged.extend_from_slice(&compressed);

        let result = decompress(&tagged);
        assert!(result.is_err(), "decompression bomb should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds limit"),
            "error should mention limit: {err_msg}"
        );
    }

    #[test]
    fn decompression_within_limit_succeeds() {
        // Create a payload that compresses well and is exactly at the limit
        let at_limit = vec![0u8; MAX_DECOMPRESS_SIZE];
        let compressed =
            zstd::encode_all(at_limit.as_slice(), ZSTD_LEVEL).expect("compression must succeed");

        let mut tagged = Vec::with_capacity(1 + compressed.len());
        tagged.push(TAG_ZSTD);
        tagged.extend_from_slice(&compressed);

        let result = decompress(&tagged);
        assert!(result.is_ok(), "payload at exactly the limit should succeed");
        assert_eq!(result.unwrap().len(), MAX_DECOMPRESS_SIZE);
    }
}
