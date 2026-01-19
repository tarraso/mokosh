//! Connection state machine for GodotNetLink protocol
//!
//! State transitions:
//! ```text
//! CLOSED → CONNECTING → HELLO_SENT → CONNECTED
//!   ↑          ↓            ↓             ↓
//!   └──────────┴────────────┴─────────────┘
//!              (any error/disconnect)
//! ```

use crate::error::{ProtocolError, Result};

/// Connection state in the protocol state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// No connection established
    Closed,

    /// Transport connection established, but HELLO not sent yet
    Connecting,

    /// HELLO message sent, waiting for HELLO_OK or HELLO_ERROR
    HelloSent,

    /// HELLO_OK received, connection fully established
    Connected,
}

impl ConnectionState {
    /// Validates a state transition
    pub fn can_transition_to(&self, next: ConnectionState) -> bool {
        use ConnectionState::*;

        match (self, next) {
            // From Closed
            (Closed, Connecting) => true,

            // From Connecting
            (Connecting, HelloSent) => true,
            (Connecting, Closed) => true, // connection failed

            // From HelloSent
            (HelloSent, Connected) => true, // HELLO_OK received
            (HelloSent, Closed) => true,    // HELLO_ERROR or timeout

            // From Connected
            (Connected, Closed) => true, // disconnect

            // Any state can stay in same state
            (a, b) if a == &b => true,

            // All other transitions are invalid
            _ => false,
        }
    }

    /// Attempts to transition to a new state
    ///
    /// Returns Ok(()) if transition is valid, Err otherwise
    pub fn transition_to(&mut self, next: ConnectionState) -> Result<()> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(ProtocolError::InvalidStateTransition {
                from: *self,
                to: next,
            })
        }
    }

    /// Returns true if the connection is in a connected state
    #[inline]
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }

    /// Returns true if the connection is closed
    #[inline]
    pub fn is_closed(&self) -> bool {
        matches!(self, ConnectionState::Closed)
    }

    /// Returns true if we're waiting for HELLO response
    #[inline]
    pub fn is_hello_pending(&self) -> bool {
        matches!(self, ConnectionState::HelloSent)
    }
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Closed
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Closed => write!(f, "Closed"),
            ConnectionState::Connecting => write!(f, "Connecting"),
            ConnectionState::HelloSent => write!(f, "HelloSent"),
            ConnectionState::Connected => write!(f, "Connected"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut state = ConnectionState::Closed;

        // Closed → Connecting
        assert!(state.transition_to(ConnectionState::Connecting).is_ok());
        assert_eq!(state, ConnectionState::Connecting);

        // Connecting → HelloSent
        assert!(state.transition_to(ConnectionState::HelloSent).is_ok());
        assert_eq!(state, ConnectionState::HelloSent);

        // HelloSent → Connected
        assert!(state.transition_to(ConnectionState::Connected).is_ok());
        assert_eq!(state, ConnectionState::Connected);

        // Connected → Closed
        assert!(state.transition_to(ConnectionState::Closed).is_ok());
        assert_eq!(state, ConnectionState::Closed);
    }

    #[test]
    fn test_invalid_transitions() {
        let mut state = ConnectionState::Closed;

        // Closed → Connected (skip handshake)
        assert!(state.transition_to(ConnectionState::Connected).is_err());
        assert_eq!(state, ConnectionState::Closed); // state unchanged

        // Closed → HelloSent (skip Connecting)
        assert!(state.transition_to(ConnectionState::HelloSent).is_err());
        assert_eq!(state, ConnectionState::Closed);
    }

    #[test]
    fn test_error_recovery() {
        let mut state = ConnectionState::Connecting;

        // Connecting → Closed (connection failed)
        assert!(state.transition_to(ConnectionState::Closed).is_ok());
        assert_eq!(state, ConnectionState::Closed);

        // HelloSent → Closed (HELLO_ERROR)
        state = ConnectionState::HelloSent;
        assert!(state.transition_to(ConnectionState::Closed).is_ok());
        assert_eq!(state, ConnectionState::Closed);
    }

    #[test]
    fn test_state_predicates() {
        assert!(ConnectionState::Closed.is_closed());
        assert!(!ConnectionState::Connecting.is_connected());
        assert!(ConnectionState::HelloSent.is_hello_pending());
        assert!(ConnectionState::Connected.is_connected());
    }

    #[test]
    fn test_default() {
        let state = ConnectionState::default();
        assert_eq!(state, ConnectionState::Closed);
    }

    #[test]
    fn test_display() {
        assert_eq!(ConnectionState::Closed.to_string(), "Closed");
        assert_eq!(ConnectionState::Connecting.to_string(), "Connecting");
        assert_eq!(ConnectionState::HelloSent.to_string(), "HelloSent");
        assert_eq!(ConnectionState::Connected.to_string(), "Connected");
    }
}
