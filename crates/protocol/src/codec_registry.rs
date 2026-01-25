//! Codec registry for managing available codecs
//!
//! The registry provides a central place to register and look up codecs by ID.
//! Applications can use the default registry (with JSON, Postcard, Raw) or
//! create custom registries with application-specific codecs.
//!
//! # Example
//!
//! ```
//! use godot_netlink_protocol::codec_registry::CodecRegistry;
//!
//! // Use the default registry
//! let registry = CodecRegistry::default();
//!
//! // Look up codec by ID
//! let codec = registry.get(1).unwrap();
//! assert_eq!(codec.name(), "JSON");
//! ```

use std::collections::HashMap;

use crate::codec::{Codec, JsonCodec, PostcardCodec, RawCodec};
use crate::error::{ProtocolError, Result};

/// Enumeration of all supported codec types
///
/// This enum allows us to store different codec implementations
/// in a type-safe way without requiring trait objects.
#[derive(Debug, Clone, Copy)]
pub enum CodecType {
    Json(JsonCodec),
    Postcard(PostcardCodec),
    Raw(RawCodec),
}

impl CodecType {
    /// Creates a CodecType from a codec ID
    ///
    /// Returns an error if the codec ID is not recognized.
    pub fn from_id(codec_id: u8) -> Result<Self> {
        match codec_id {
            1 => Ok(CodecType::Json(JsonCodec)),
            2 => Ok(CodecType::Postcard(PostcardCodec)),
            3 => Ok(CodecType::Raw(RawCodec)),
            _ => Err(ProtocolError::CodecError(format!(
                "Unknown codec ID: {}",
                codec_id
            ))),
        }
    }

    /// Returns the codec ID
    pub fn id(&self) -> u8 {
        match self {
            CodecType::Json(c) => c.id(),
            CodecType::Postcard(c) => c.id(),
            CodecType::Raw(c) => c.id(),
        }
    }

    /// Returns the codec name
    pub fn name(&self) -> &'static str {
        match self {
            CodecType::Json(c) => c.name(),
            CodecType::Postcard(c) => c.name(),
            CodecType::Raw(c) => c.name(),
        }
    }

    /// Encodes a message using the appropriate codec
    pub fn encode<T: serde::Serialize>(&self, message: &T) -> Result<bytes::Bytes> {
        match self {
            CodecType::Json(c) => c.encode(message),
            CodecType::Postcard(c) => c.encode(message),
            CodecType::Raw(c) => c.encode(message),
        }
    }

    /// Decodes a message using the appropriate codec
    pub fn decode<T: serde::de::DeserializeOwned>(&self, bytes: &bytes::Bytes) -> Result<T> {
        match self {
            CodecType::Json(c) => c.decode(bytes),
            CodecType::Postcard(c) => c.decode(bytes),
            CodecType::Raw(c) => c.decode(bytes),
        }
    }

    /// Encodes raw bytes (only for Raw codec)
    pub fn encode_raw(&self, bytes: bytes::Bytes) -> Result<bytes::Bytes> {
        match self {
            CodecType::Raw(c) => c.encode_raw(bytes),
            _ => Err(ProtocolError::CodecError(
                "encode_raw only supported for Raw codec".into(),
            )),
        }
    }

    /// Decodes raw bytes (only for Raw codec)
    pub fn decode_raw(&self, bytes: &bytes::Bytes) -> Result<bytes::Bytes> {
        match self {
            CodecType::Raw(c) => c.decode_raw(bytes),
            _ => Err(ProtocolError::CodecError(
                "decode_raw only supported for Raw codec".into(),
            )),
        }
    }
}

/// Thread-safe codec registry
///
/// The registry stores codecs by ID for efficient lookup.
#[derive(Clone)]
pub struct CodecRegistry {
    codecs: HashMap<u8, CodecType>,
}

impl CodecRegistry {
    /// Creates a new empty codec registry
    pub fn new() -> Self {
        Self {
            codecs: HashMap::new(),
        }
    }

    /// Creates the default registry with JSON (1), Postcard (2), and Raw (3) codecs
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(CodecType::Json(JsonCodec));
        registry.register(CodecType::Postcard(PostcardCodec));
        registry.register(CodecType::Raw(RawCodec));
        registry
    }

    /// Registers a codec in the registry
    ///
    /// If a codec with the same ID already exists, it will be replaced.
    pub fn register(&mut self, codec: CodecType) {
        self.codecs.insert(codec.id(), codec);
    }

    /// Gets a codec by ID
    ///
    /// Returns None if no codec with that ID is registered.
    pub fn get(&self, codec_id: u8) -> Option<CodecType> {
        self.codecs.get(&codec_id).copied()
    }

    /// Gets a codec by ID, returning an error if not found
    pub fn get_or_err(&self, codec_id: u8) -> Result<CodecType> {
        self.get(codec_id).ok_or_else(|| {
            ProtocolError::CodecError(format!("Unknown codec ID: {}", codec_id))
        })
    }

    /// Returns a list of all registered codec IDs
    pub fn codec_ids(&self) -> Vec<u8> {
        let mut ids: Vec<u8> = self.codecs.keys().copied().collect();
        ids.sort();
        ids
    }

    /// Returns the number of registered codecs
    pub fn len(&self) -> usize {
        self.codecs.len()
    }

    /// Returns true if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.codecs.is_empty()
    }

    /// Checks if a codec with the given ID is registered
    pub fn has_codec(&self, codec_id: u8) -> bool {
        self.codecs.contains_key(&codec_id)
    }
}

impl Default for CodecRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = CodecRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        assert!(registry.get(1).is_none());
    }

    #[test]
    fn test_default_registry() {
        let registry = CodecRegistry::default();
        assert_eq!(registry.len(), 3);
        assert!(!registry.is_empty());

        // Check all default codecs are present
        assert!(registry.has_codec(1)); // JSON
        assert!(registry.has_codec(2)); // Postcard
        assert!(registry.has_codec(3)); // Raw

        // Verify codec names
        assert_eq!(registry.get(1).unwrap().name(), "JSON");
        assert_eq!(registry.get(2).unwrap().name(), "Postcard");
        assert_eq!(registry.get(3).unwrap().name(), "Raw");
    }

    #[test]
    fn test_codec_ids() {
        let registry = CodecRegistry::default();
        let ids = registry.codec_ids();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn test_get_or_err() {
        let registry = CodecRegistry::default();

        // Should succeed for known codec
        let codec = registry.get_or_err(1);
        assert!(codec.is_ok());
        assert_eq!(codec.unwrap().name(), "JSON");

        // Should fail for unknown codec
        let result = registry.get_or_err(99);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_custom_codec() {
        let mut registry = CodecRegistry::new();

        // Register a custom codec
        let json_codec = CodecType::Json(JsonCodec);
        registry.register(json_codec);

        assert_eq!(registry.len(), 1);
        assert!(registry.has_codec(1));

        let retrieved = registry.get(1).unwrap();
        assert_eq!(retrieved.id(), 1);
        assert_eq!(retrieved.name(), "JSON");
    }

    #[test]
    fn test_register_replaces_existing() {
        let mut registry = CodecRegistry::new();

        // Register JSON codec
        registry.register(CodecType::Json(JsonCodec));
        assert_eq!(registry.len(), 1);

        // Register again - should replace, not add
        registry.register(CodecType::Json(JsonCodec));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_clone_registry() {
        let registry1 = CodecRegistry::default();
        let registry2 = registry1.clone();

        // Both should have the same codecs
        assert_eq!(registry1.len(), registry2.len());
        assert_eq!(registry1.codec_ids(), registry2.codec_ids());
    }
}
