//! Message registry for type-safe message handling
//!
//! This module provides a type-safe abstraction over the raw Envelope protocol,
//! allowing developers to define game messages with compile-time route ID and
//! schema hash checking.
//!
//! # Example
//!
//! ```
//! use mokosh_protocol::message_registry::GameMessage;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct PlayerInput {
//!     x: f32,
//!     y: f32,
//! }
//!
//! impl GameMessage for PlayerInput {
//!     const ROUTE_ID: u16 = 100;
//!     const SCHEMA_HASH: u64 = 0x1234_5678_90AB_CDEF;
//! }
//! ```

use serde::{de::DeserializeOwned, Serialize};

/// Trait for type-safe game messages
///
/// Implement this trait for all game messages (route_id >= 100) to enable
/// type-safe sending and receiving with automatic route ID and schema hash.
///
/// # Example
///
/// ```
/// use mokosh_protocol::message_registry::GameMessage;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct ChatMessage {
///     sender: String,
///     text: String,
/// }
///
/// impl GameMessage for ChatMessage {
///     const ROUTE_ID: u16 = 102;
///     const SCHEMA_HASH: u64 = 0xABCD_EF12_3456_7890;
/// }
/// ```
///
/// # Schema Hash
///
/// For MVP, schema hashes are defined manually. In the future, a derive macro
/// will automatically generate stable hashes from the struct definition.
///
/// Manual hash guidelines:
/// - Use a unique 64-bit value for each message type
/// - Change the hash when the message structure changes (breaking change)
/// - Keep the hash the same for compatible changes (adding optional fields)
pub trait GameMessage: Serialize + DeserializeOwned + Send + Sync {
    /// Route ID for this message type (must be >= 100)
    const ROUTE_ID: u16;

    /// Schema hash for compatibility checking
    ///
    /// This hash identifies the structure of the message. Clients and servers
    /// must have matching schema hashes to communicate.
    const SCHEMA_HASH: u64;
}

/// Calculates a global schema hash from all registered message types
///
/// This hash is used during the HELLO handshake to ensure client and server
/// have compatible message schemas.
///
/// # Algorithm
///
/// For simplicity, we XOR all individual message schema hashes together.
/// This provides a single hash that changes whenever any message schema changes.
///
/// # Example
///
/// ```
/// use mokosh_protocol::message_registry::calculate_global_schema_hash;
///
/// let hashes = vec![
///     0x1111_1111_1111_1111, // PlayerInput::SCHEMA_HASH
///     0x2222_2222_2222_2222, // PlayerState::SCHEMA_HASH
///     0x3333_3333_3333_3333, // ChatMessage::SCHEMA_HASH
/// ];
///
/// let global_hash = calculate_global_schema_hash(&hashes);
/// ```
pub fn calculate_global_schema_hash(message_hashes: &[u64]) -> u64 {
    message_hashes.iter().fold(0u64, |acc, &hash| acc ^ hash)
}

/// Message registry for tracking registered message types
///
/// This is a simple registry for future extensibility. Currently it only stores
/// registered message types for validation purposes.
///
/// In future phases, this registry can be extended with:
/// - Message direction validation (C→S, S→C, bidirectional)
/// - Rate limiting per message type
/// - Max message size validation
/// - Default envelope flags per message type
#[derive(Debug, Default)]
pub struct MessageRegistry {
    /// Registered message route IDs and their schema hashes
    registered_routes: Vec<(u16, u64)>,
}

impl MessageRegistry {
    /// Creates a new empty message registry
    pub fn new() -> Self {
        Self {
            registered_routes: Vec::new(),
        }
    }

    /// Registers a message type
    ///
    /// # Example
    ///
    /// ```
    /// use mokosh_protocol::message_registry::{GameMessage, MessageRegistry};
    /// use serde::{Serialize, Deserialize};
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct PlayerInput { x: f32, y: f32 }
    ///
    /// impl GameMessage for PlayerInput {
    ///     const ROUTE_ID: u16 = 100;
    ///     const SCHEMA_HASH: u64 = 0x1234;
    /// }
    ///
    /// let mut registry = MessageRegistry::new();
    /// registry.register::<PlayerInput>();
    /// ```
    pub fn register<T: GameMessage>(&mut self) {
        self.registered_routes.push((T::ROUTE_ID, T::SCHEMA_HASH));
    }

    /// Returns the global schema hash for all registered messages
    pub fn global_schema_hash(&self) -> u64 {
        let hashes: Vec<u64> = self.registered_routes.iter().map(|(_, hash)| *hash).collect();
        calculate_global_schema_hash(&hashes)
    }

    /// Returns the number of registered message types
    pub fn len(&self) -> usize {
        self.registered_routes.len()
    }

    /// Checks if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.registered_routes.is_empty()
    }

    /// Checks if a route ID is registered
    pub fn is_registered(&self, route_id: u16) -> bool {
        self.registered_routes.iter().any(|(id, _)| *id == route_id)
    }

    /// Gets the schema hash for a given route ID
    pub fn get_schema_hash(&self, route_id: u16) -> Option<u64> {
        self.registered_routes
            .iter()
            .find(|(id, _)| *id == route_id)
            .map(|(_, hash)| *hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestMessage1 {
        value: u32,
    }

    impl GameMessage for TestMessage1 {
        const ROUTE_ID: u16 = 100;
        const SCHEMA_HASH: u64 = 0x1111_1111_1111_1111;
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestMessage2 {
        text: String,
    }

    impl GameMessage for TestMessage2 {
        const ROUTE_ID: u16 = 101;
        const SCHEMA_HASH: u64 = 0x2222_2222_2222_2222;
    }

    #[test]
    fn test_game_message_constants() {
        assert_eq!(TestMessage1::ROUTE_ID, 100);
        assert_eq!(TestMessage1::SCHEMA_HASH, 0x1111_1111_1111_1111);
        assert_eq!(TestMessage2::ROUTE_ID, 101);
        assert_eq!(TestMessage2::SCHEMA_HASH, 0x2222_2222_2222_2222);
    }

    #[test]
    fn test_calculate_global_schema_hash_empty() {
        let hashes: Vec<u64> = vec![];
        let global = calculate_global_schema_hash(&hashes);
        assert_eq!(global, 0);
    }

    #[test]
    fn test_calculate_global_schema_hash_single() {
        let hashes = vec![0xAAAA_BBBB_CCCC_DDDD];
        let global = calculate_global_schema_hash(&hashes);
        assert_eq!(global, 0xAAAA_BBBB_CCCC_DDDD);
    }

    #[test]
    fn test_calculate_global_schema_hash_multiple() {
        let hashes = vec![
            0x1111_1111_1111_1111,
            0x2222_2222_2222_2222,
            0x3333_3333_3333_3333,
        ];
        let global = calculate_global_schema_hash(&hashes);
        // XOR: 0x1111 ^ 0x2222 ^ 0x3333 = 0x0000
        assert_eq!(global, 0x0000_0000_0000_0000);
    }

    #[test]
    fn test_calculate_global_schema_hash_order_independent() {
        let hashes1 = vec![
            0x1234_5678_90AB_CDEF,
            0xFEDC_BA98_7654_3210,
        ];
        let hashes2 = vec![
            0xFEDC_BA98_7654_3210,
            0x1234_5678_90AB_CDEF,
        ];

        assert_eq!(
            calculate_global_schema_hash(&hashes1),
            calculate_global_schema_hash(&hashes2)
        );
    }

    #[test]
    fn test_message_registry_empty() {
        let registry = MessageRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        assert_eq!(registry.global_schema_hash(), 0);
    }

    #[test]
    fn test_message_registry_register() {
        let mut registry = MessageRegistry::new();

        registry.register::<TestMessage1>();
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
        assert!(registry.is_registered(100));
        assert!(!registry.is_registered(101));
        assert_eq!(registry.get_schema_hash(100), Some(0x1111_1111_1111_1111));

        registry.register::<TestMessage2>();
        assert_eq!(registry.len(), 2);
        assert!(registry.is_registered(101));
        assert_eq!(registry.get_schema_hash(101), Some(0x2222_2222_2222_2222));
    }

    #[test]
    fn test_message_registry_global_hash() {
        let mut registry = MessageRegistry::new();
        registry.register::<TestMessage1>();
        registry.register::<TestMessage2>();

        let expected = 0x1111_1111_1111_1111 ^ 0x2222_2222_2222_2222;
        assert_eq!(registry.global_schema_hash(), expected);
    }
}
