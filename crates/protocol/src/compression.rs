//! Compression support for Mokosh protocol
//!
//! This module provides compression functionality for reducing payload sizes.
//! Supported algorithms:
//! - **Zstd**: Fast compression with good ratio (default level: 3)
//! - **Lz4**: Fastest compression with moderate ratio
//!
//! ## Example
//!
//! ```
//! use mokosh_protocol::compression::{Compressor, ZstdCompressor, CompressionType};
//!
//! let compressor = ZstdCompressor::new();
//! let data = b"Hello, Godot! This is a test message.".repeat(10);
//!
//! let compressed = compressor.compress(&data).unwrap();
//! println!("Original: {} bytes, Compressed: {} bytes", data.len(), compressed.len());
//!
//! let decompressed = compressor.decompress(&compressed).unwrap();
//! assert_eq!(data, decompressed.as_ref());
//! ```

use bytes::Bytes;
use thiserror::Error;

/// Compression errors
#[derive(Debug, Error)]
pub enum CompressionError {
    /// Zstd compression failed
    #[error("Zstd compression failed: {0}")]
    ZstdCompressionFailed(#[from] std::io::Error),

    /// Lz4 compression failed
    #[error("Lz4 compression failed: {0}")]
    Lz4CompressionFailed(String),

    /// Decompression failed
    #[error("Decompression failed: {0}")]
    DecompressionFailed(String),
}

pub type CompressionResult<T> = Result<T, CompressionError>;

/// Compression type identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionType {
    /// No compression
    None,
    /// Zstd compression (fast, good ratio)
    Zstd,
    /// Lz4 compression (fastest, moderate ratio)
    Lz4,
}

impl CompressionType {
    /// Returns the name of the compression algorithm
    pub fn name(&self) -> &'static str {
        match self {
            CompressionType::None => "None",
            CompressionType::Zstd => "Zstd",
            CompressionType::Lz4 => "Lz4",
        }
    }
}

/// Trait for compression/decompression operations
pub trait Compressor: Send + Sync + Clone {
    /// Compresses the input data
    fn compress(&self, data: &[u8]) -> CompressionResult<Bytes>;

    /// Decompresses the input data
    fn decompress(&self, data: &[u8]) -> CompressionResult<Bytes>;

    /// Returns the compression type
    fn compression_type(&self) -> CompressionType;
}

/// No-op compressor that passes data through unchanged
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCompressor;

impl Compressor for NoCompressor {
    fn compress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        Ok(Bytes::copy_from_slice(data))
    }

    fn decompress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        Ok(Bytes::copy_from_slice(data))
    }

    fn compression_type(&self) -> CompressionType {
        CompressionType::None
    }
}

/// Zstd compressor implementation
///
/// Uses zstd compression with default compression level (3).
/// Provides good balance between speed and compression ratio.
#[derive(Debug, Clone)]
pub struct ZstdCompressor {
    level: i32,
}

impl ZstdCompressor {
    /// Creates a new Zstd compressor with default level (3)
    pub fn new() -> Self {
        Self { level: 3 }
    }

    /// Creates a new Zstd compressor with custom compression level (1-22)
    ///
    /// - Level 1: Fastest, lowest ratio
    /// - Level 3: Default, good balance
    /// - Level 22: Slowest, highest ratio
    pub fn with_level(level: i32) -> Self {
        Self { level }
    }
}

impl Default for ZstdCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for ZstdCompressor {
    fn compress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        let compressed = zstd::encode_all(data, self.level)?;
        Ok(Bytes::from(compressed))
    }

    fn decompress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        let decompressed = zstd::decode_all(data)?;
        Ok(Bytes::from(decompressed))
    }

    fn compression_type(&self) -> CompressionType {
        CompressionType::Zstd
    }
}

/// Lz4 compressor implementation
///
/// Uses lz4 compression for maximum speed.
/// Provides fastest compression with moderate ratio.
#[derive(Debug, Clone)]
pub struct Lz4Compressor;

impl Lz4Compressor {
    /// Creates a new Lz4 compressor
    pub fn new() -> Self {
        Self
    }
}

impl Default for Lz4Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Lz4Compressor {
    fn compress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        // Prepend original size (u32 big-endian) for decompression
        let original_size = data.len() as u32;
        let compressed_data = lz4::block::compress(data, Some(lz4::block::CompressionMode::DEFAULT), false)
            .map_err(|e| CompressionError::Lz4CompressionFailed(e.to_string()))?;

        // Combine size prefix + compressed data
        let mut result = Vec::with_capacity(4 + compressed_data.len());
        result.extend_from_slice(&original_size.to_be_bytes());
        result.extend_from_slice(&compressed_data);

        Ok(Bytes::from(result))
    }

    fn decompress(&self, data: &[u8]) -> CompressionResult<Bytes> {
        // Extract original size from first 4 bytes
        if data.len() < 4 {
            return Err(CompressionError::DecompressionFailed(
                "Lz4 compressed data too short (missing size prefix)".to_string()
            ));
        }

        let size_bytes: [u8; 4] = data[0..4].try_into().unwrap();
        let original_size = u32::from_be_bytes(size_bytes) as usize;

        // Decompress the rest
        let compressed_data = &data[4..];
        let decompressed = lz4::block::decompress(compressed_data, Some(original_size as i32))
            .map_err(|e| CompressionError::DecompressionFailed(e.to_string()))?;

        Ok(Bytes::from(decompressed))
    }

    fn compression_type(&self) -> CompressionType {
        CompressionType::Lz4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zstd_compression() {
        let compressor = ZstdCompressor::new();
        let data = b"Hello, Godot! This is a test message.".repeat(10);

        let compressed = compressor.compress(&data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
        // Compression should reduce size for repeated data
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_zstd_empty_data() {
        let compressor = ZstdCompressor::new();
        let data = b"";

        let compressed = compressor.compress(data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
    }

    #[test]
    fn test_zstd_compression_levels() {
        let data = b"Test data for compression".repeat(100);

        let level1 = ZstdCompressor::with_level(1);
        let level10 = ZstdCompressor::with_level(10);

        let compressed1 = level1.compress(&data).unwrap();
        let compressed10 = level10.compress(&data).unwrap();

        // Higher level should produce smaller output
        assert!(compressed10.len() <= compressed1.len());

        // Both should decompress correctly
        assert_eq!(data.as_slice(), level1.decompress(&compressed1).unwrap().as_ref());
        assert_eq!(data.as_slice(), level10.decompress(&compressed10).unwrap().as_ref());
    }

    #[test]
    fn test_lz4_compression() {
        let compressor = Lz4Compressor::new();
        let data = b"Hello, Godot! This is a test message.".repeat(10);

        let compressed = compressor.compress(&data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
        // Compression should reduce size for repeated data
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_lz4_empty_data() {
        let compressor = Lz4Compressor::new();
        let data = b"";

        let compressed = compressor.compress(data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
    }

    #[test]
    fn test_compression_ratio() {
        let data = b"A".repeat(1000); // Highly compressible

        let zstd = ZstdCompressor::new();
        let lz4 = Lz4Compressor::new();

        let zstd_compressed = zstd.compress(&data).unwrap();
        let lz4_compressed = lz4.compress(&data).unwrap();

        println!("Original: {} bytes", data.len());
        println!("Zstd: {} bytes ({}% of original)",
                 zstd_compressed.len(),
                 (zstd_compressed.len() * 100) / data.len());
        println!("Lz4: {} bytes ({}% of original)",
                 lz4_compressed.len(),
                 (lz4_compressed.len() * 100) / data.len());

        // Both should achieve significant compression
        assert!(zstd_compressed.len() < data.len() / 10);
        assert!(lz4_compressed.len() < data.len() / 10);
    }

    #[test]
    fn test_compression_type() {
        let zstd = ZstdCompressor::new();
        let lz4 = Lz4Compressor::new();

        assert_eq!(zstd.compression_type(), CompressionType::Zstd);
        assert_eq!(lz4.compression_type(), CompressionType::Lz4);
    }

    #[test]
    fn test_zstd_large_data() {
        let compressor = ZstdCompressor::new();
        let data = vec![0xAB; 100_000]; // 100KB

        let compressed = compressor.compress(&data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
    }

    #[test]
    fn test_lz4_large_data() {
        let compressor = Lz4Compressor::new();
        let data = vec![0xAB; 100_000]; // 100KB

        let compressed = compressor.compress(&data).unwrap();
        let decompressed = compressor.decompress(&compressed).unwrap();

        assert_eq!(data.as_slice(), decompressed.as_ref());
    }
}
