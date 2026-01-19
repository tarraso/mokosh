//! Protocol version negotiation for GodotNetLink
//!
//! Version format: `MAJOR << 8 | MINOR`
//! - v1.0 = 0x0100 (256)
//! - v1.5 = 0x0105 (261)
//! - v2.0 = 0x0200 (512)
//!
//! ## Compatibility Rules
//! - MAJOR change = breaking changes (incompatible)
//! - MINOR change = backward-compatible additions
//!
//! ## Negotiation Process
//! 1. Client sends: protocol_version (desired), min_protocol_version (minimum supported)
//! 2. Server checks: do version ranges overlap?
//! 3. If yes: use min(client.protocol_version, server.protocol_version)
//! 4. If no: return VersionMismatch error

use crate::error::{ProtocolError, Result};

/// Current protocol version (v1.0)
pub const CURRENT_PROTOCOL_VERSION: u16 = 0x0100;

/// Minimum supported protocol version (v1.0)
pub const MIN_PROTOCOL_VERSION: u16 = 0x0100;

/// Extracts the major version number
#[inline]
pub fn major_version(version: u16) -> u8 {
    (version >> 8) as u8
}

/// Extracts the minor version number
#[inline]
pub fn minor_version(version: u16) -> u8 {
    (version & 0xFF) as u8
}

/// Creates a version number from major and minor components
#[inline]
pub fn make_version(major: u8, minor: u8) -> u16 {
    ((major as u16) << 8) | (minor as u16)
}

/// Negotiates protocol version between client and server
///
/// # Arguments
/// * `client_version` - Client's preferred protocol version
/// * `client_min` - Client's minimum supported version
/// * `server_version` - Server's current protocol version
/// * `server_min` - Server's minimum supported version
///
/// # Returns
/// * `Ok(version)` - Negotiated version to use
/// * `Err(VersionMismatch)` - No compatible version found
///
/// # Example
/// ```
/// use godot_netlink_protocol::version::negotiate_version;
///
/// // Client supports v1.0-v1.5, Server supports v1.0-v2.0
/// let version = negotiate_version(0x0105, 0x0100, 0x0200, 0x0100).unwrap();
/// assert_eq!(version, 0x0105); // Use v1.5 (client's preferred)
/// ```
pub fn negotiate_version(
    client_version: u16,
    client_min: u16,
    server_version: u16,
    server_min: u16,
) -> Result<u16> {
    // Check if version ranges overlap
    // Client range: [client_min, client_version]
    // Server range: [server_min, server_version]

    let ranges_overlap = client_version >= server_min && server_version >= client_min;

    if !ranges_overlap {
        return Err(ProtocolError::VersionMismatch {
            client_min,
            client_max: client_version,
            server_min,
            server_max: server_version,
        });
    }

    // Use the minimum of both preferred versions (most conservative choice)
    let negotiated = client_version.min(server_version);

    // Verify the negotiated version is within both ranges
    if negotiated >= client_min && negotiated >= server_min {
        Ok(negotiated)
    } else {
        Err(ProtocolError::VersionMismatch {
            client_min,
            client_max: client_version,
            server_min,
            server_max: server_version,
        })
    }
}

/// Checks if two versions are compatible (same major version)
#[inline]
pub fn is_compatible(version_a: u16, version_b: u16) -> bool {
    major_version(version_a) == major_version(version_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_encoding() {
        assert_eq!(make_version(1, 0), 0x0100); // v1.0
        assert_eq!(make_version(1, 5), 0x0105); // v1.5
        assert_eq!(make_version(2, 0), 0x0200); // v2.0
    }

    #[test]
    fn test_version_decoding() {
        assert_eq!(major_version(0x0100), 1);
        assert_eq!(minor_version(0x0100), 0);

        assert_eq!(major_version(0x0105), 1);
        assert_eq!(minor_version(0x0105), 5);

        assert_eq!(major_version(0x0200), 2);
        assert_eq!(minor_version(0x0200), 0);
    }

    #[test]
    fn test_successful_negotiation_same_version() {
        // Both support exactly v1.0
        let result = negotiate_version(0x0100, 0x0100, 0x0100, 0x0100);
        assert_eq!(result.unwrap(), 0x0100);
    }

    #[test]
    fn test_successful_negotiation_overlapping_ranges() {
        // Client: v1.0-v1.5, Server: v1.0-v2.0
        // Should use v1.5 (client's max)
        let result = negotiate_version(0x0105, 0x0100, 0x0200, 0x0100);
        assert_eq!(result.unwrap(), 0x0105);
    }

    #[test]
    fn test_successful_negotiation_server_lower() {
        // Client: v1.0-v2.0, Server: v1.0-v1.5
        // Should use v1.5 (server's max)
        let result = negotiate_version(0x0200, 0x0100, 0x0105, 0x0100);
        assert_eq!(result.unwrap(), 0x0105);
    }

    #[test]
    fn test_negotiation_failure_client_too_old() {
        // Client: v1.0-v1.0, Server: v2.0-v2.0
        // No overlap
        let result = negotiate_version(0x0100, 0x0100, 0x0200, 0x0200);
        assert!(result.is_err());

        if let Err(ProtocolError::VersionMismatch { client_min, client_max, server_min, server_max }) = result {
            assert_eq!(client_min, 0x0100);
            assert_eq!(client_max, 0x0100);
            assert_eq!(server_min, 0x0200);
            assert_eq!(server_max, 0x0200);
        } else {
            panic!("Expected VersionMismatch error");
        }
    }

    #[test]
    fn test_negotiation_failure_server_too_old() {
        // Client: v2.0-v2.0, Server: v1.0-v1.0
        // No overlap
        let result = negotiate_version(0x0200, 0x0200, 0x0100, 0x0100);
        assert!(result.is_err());
    }

    #[test]
    fn test_negotiation_failure_gap_in_range() {
        // Client: v1.0-v1.2, Server: v1.5-v2.0
        // Gap between ranges
        let result = negotiate_version(0x0102, 0x0100, 0x0200, 0x0105);
        assert!(result.is_err());
    }

    #[test]
    fn test_compatibility_same_major() {
        assert!(is_compatible(0x0100, 0x0105)); // v1.0 and v1.5
        assert!(is_compatible(0x0105, 0x0100)); // v1.5 and v1.0
    }

    #[test]
    fn test_incompatibility_different_major() {
        assert!(!is_compatible(0x0100, 0x0200)); // v1.0 and v2.0
        assert!(!is_compatible(0x0200, 0x0100)); // v2.0 and v1.0
    }

    #[test]
    fn test_constants() {
        assert_eq!(CURRENT_PROTOCOL_VERSION, 0x0100);
        assert_eq!(MIN_PROTOCOL_VERSION, 0x0100);
        assert_eq!(major_version(CURRENT_PROTOCOL_VERSION), 1);
        assert_eq!(minor_version(CURRENT_PROTOCOL_VERSION), 0);
    }
}
