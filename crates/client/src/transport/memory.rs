//! In-memory transport for testing and single-player games
//!
//! This transport uses Tokio channels instead of network sockets,
//! making it perfect for:
//! - Unit testing without network overhead
//! - Single-player games where the server runs in the same process
//! - Development and debugging

use super::Transport;
use async_trait::async_trait;
use godot_netlink_protocol::Envelope;
use tokio::sync::mpsc;

/// In-memory client transport that communicates via channels
///
/// This transport is useful for testing and single-player scenarios
/// where the client and server run in the same process.
///
/// # Example
///
/// ```no_run
/// use godot_netlink_client::transport::memory::MemoryTransport;
/// use tokio::sync::mpsc;
///
/// let (to_peer_tx, to_peer_rx) = mpsc::channel(100);
/// let (from_peer_tx, from_peer_rx) = mpsc::channel(100);
///
/// let transport = MemoryTransport::new(to_peer_tx, from_peer_rx);
/// ```
pub struct MemoryTransport {
    /// Channel to send envelopes to the peer
    to_peer: mpsc::Sender<Envelope>,
    /// Channel to receive envelopes from the peer
    from_peer: mpsc::Receiver<Envelope>,
}

impl MemoryTransport {
    /// Creates a new in-memory transport
    ///
    /// # Arguments
    /// * `to_peer` - Channel to send envelopes to the other side
    /// * `from_peer` - Channel to receive envelopes from the other side
    pub fn new(
        to_peer: mpsc::Sender<Envelope>,
        from_peer: mpsc::Receiver<Envelope>,
    ) -> Self {
        Self {
            to_peer,
            from_peer,
        }
    }

    /// Creates a pair of connected transports for client and server
    ///
    /// This is a convenience function for creating both transports with
    /// properly connected channels.
    ///
    /// # Arguments
    /// * `buffer_size` - Size of the channel buffers (default: 100)
    ///
    /// # Returns
    /// A tuple of (client_transport, server_transport)
    ///
    /// # Example
    ///
    /// ```
    /// use godot_netlink_client::transport::memory::MemoryTransport;
    ///
    /// let (client_transport, server_transport) = MemoryTransport::create_pair(100);
    /// ```
    pub fn create_pair(buffer_size: usize) -> (Self, Self) {
        let (client_to_server_tx, client_to_server_rx) = mpsc::channel(buffer_size);
        let (server_to_client_tx, server_to_client_rx) = mpsc::channel(buffer_size);

        let client_transport = Self::new(client_to_server_tx, server_to_client_rx);
        let server_transport = Self::new(server_to_client_tx, client_to_server_rx);

        (client_transport, server_transport)
    }
}

#[async_trait]
impl Transport for MemoryTransport {
    type Error = MemoryTransportError;

    async fn run(
        mut self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                // Receive from peer, forward to event loop
                Some(envelope) = self.from_peer.recv() => {
                    if incoming_tx.send(envelope).await.is_err() {
                        return Err(MemoryTransportError::ChannelClosed);
                    }
                }

                // Receive from event loop, send to peer
                Some(envelope) = outgoing_rx.recv() => {
                    if self.to_peer.send(envelope).await.is_err() {
                        return Err(MemoryTransportError::ChannelClosed);
                    }
                }

                else => {
                    return Ok(());
                }
            }
        }
    }
}

/// Memory transport errors
#[derive(Debug, thiserror::Error)]
pub enum MemoryTransportError {
    #[error("Transport channel closed")]
    ChannelClosed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use godot_netlink_protocol::EnvelopeFlags;

    #[tokio::test]
    async fn test_memory_transport_sends_to_peer() {
        let (to_peer_tx, mut to_peer_rx) = mpsc::channel(10);
        let (_from_peer_tx, from_peer_rx) = mpsc::channel(10);

        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let transport = MemoryTransport::new(to_peer_tx, from_peer_rx);

        tokio::spawn(async move {
            let _ = transport.run(incoming_tx, outgoing_rx).await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            42,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        outgoing_tx.send(test_envelope.clone()).await.unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            to_peer_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.payload, test_envelope.payload);
    }

    #[tokio::test]
    async fn test_memory_transport_receives_from_peer() {
        let (_to_peer_tx, to_peer_rx) = mpsc::channel(10);
        let (from_peer_tx, from_peer_rx) = mpsc::channel(10);

        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let transport = MemoryTransport::new(_to_peer_tx, from_peer_rx);

        tokio::spawn(async move {
            let _ = transport.run(incoming_tx, outgoing_rx).await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            42,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"response"),
        );

        from_peer_tx.send(test_envelope.clone()).await.unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.payload, test_envelope.payload);
    }

    #[tokio::test]
    async fn test_memory_transport_handles_channel_close() {
        let (to_peer_tx, _to_peer_rx) = mpsc::channel(10);
        let (_from_peer_tx, from_peer_rx) = mpsc::channel(10);

        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let transport = MemoryTransport::new(to_peer_tx, from_peer_rx);

        drop(_from_peer_tx);
        drop(_outgoing_tx);

        let result = transport.run(incoming_tx, outgoing_rx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_create_pair() {
        let (client_transport, server_transport) = MemoryTransport::create_pair(100);

        let (client_incoming_tx, mut client_incoming_rx) = mpsc::channel(10);
        let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(10);

        let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(10);
        let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(10);

        tokio::spawn(async move {
            let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
        });

        tokio::spawn(async move {
            let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
        });

        // Client sends to server
        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            42,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"client->server"),
        );

        client_outgoing_tx.send(test_envelope.clone()).await.unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            server_incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.payload, test_envelope.payload);

        // Server sends back to client
        let response = Envelope::new_simple(
            1,
            1,
            0,
            43,
            2,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"server->client"),
        );

        server_outgoing_tx.send(response.clone()).await.unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            client_incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, response.route_id);
        assert_eq!(received.payload, response.payload);
    }
}
