//! Transport layer abstraction for Mokosh
//!
//! This module defines the core Transport trait that allows Mokosh
//! to work with different network protocols (WebSocket, TCP, QUIC, in-memory, etc.)
//! while keeping the client and server event loops transport-agnostic.

use crate::Envelope;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Transport layer abstraction for network communication
///
/// This trait allows Mokosh to support multiple transport protocols
/// (WebSocket, TCP, QUIC, in-memory channels, etc.) while keeping the
/// event loop implementation transport-agnostic.
///
/// The Transport trait is symmetric - the same trait works for both
/// client-side and server-side transports. The only difference is:
/// - **Client**: Typically connects to a server
/// - **Server**: Typically accepts connections from clients
///
/// # Example: Implementing a custom transport
///
/// ```no_run
/// use async_trait::async_trait;
/// use mokosh_protocol::transport::Transport;
/// use mokosh_protocol::Envelope;
/// use tokio::sync::mpsc;
///
/// struct MyCustomTransport {
///     // Your transport-specific fields
/// }
///
/// #[async_trait]
/// impl Transport for MyCustomTransport {
///     type Error = std::io::Error;
///
///     async fn run(
///         self,
///         incoming_tx: mpsc::Sender<Envelope>,
///         mut outgoing_rx: mpsc::Receiver<Envelope>,
///     ) -> Result<(), Self::Error> {
///         // Implement your transport logic here
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Transport: Send + 'static {
    /// Error type for this transport
    type Error: std::error::Error + Send + Sync + 'static;

    /// Runs the transport layer, bridging envelope channels
    ///
    /// This method establishes connectivity (connect for clients, listen for servers)
    /// and runs a loop that:
    /// - Receives raw data from the network, decodes it into Envelopes,
    ///   and sends them to `incoming_tx`
    /// - Receives Envelopes from `outgoing_rx`, encodes them,
    ///   and sends them over the network
    ///
    /// # Arguments
    /// * `incoming_tx` - Channel to send received envelopes to the event loop
    /// * `outgoing_rx` - Channel to receive envelopes from the event loop
    ///
    /// # Returns
    /// Returns `Ok(())` when the transport is shut down gracefully,
    /// or an error if something went wrong.
    async fn run(
        self,
        incoming_tx: mpsc::Sender<Envelope>,
        outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error>;
}
