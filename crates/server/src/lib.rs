//! # GodotNetLink Server
//!
//! Server-side event loop for GodotNetLink protocol.
//!
//! ## Example
//!
//! ```no_run
//! use godot_netlink_server::Server;
//! use tokio::sync::mpsc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let (incoming_tx, incoming_rx) = mpsc::channel(100);
//!     let (outgoing_tx, outgoing_rx) = mpsc::channel(100);
//!
//!     let server = Server::new(incoming_rx, outgoing_tx);
//!     server.run().await;
//! }
//! ```

use godot_netlink_protocol::Envelope;
use tokio::sync::mpsc;

/// Server event loop handler
///
/// The server receives envelopes from incoming channel, processes them,
/// and sends responses through the outgoing channel.
pub struct Server {
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<Envelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<Envelope>,
}

impl Server {
    /// Creates a new server with the given channels
    pub fn new(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
    ) -> Self {
        Self {
            incoming_rx,
            outgoing_tx,
        }
    }

    /// Runs the main event loop
    ///
    /// This method will block until the incoming channel is closed.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(envelope) = self.incoming_rx.recv() => {
                    if let Err(e) = self.handle_envelope(envelope).await {
                        eprintln!("Error handling envelope: {}", e);
                    }
                }

                else => {
                    println!("Server shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles a single envelope
    async fn handle_envelope(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        println!(
            "Server received: route_id={}, msg_id={}, payload_len={}",
            envelope.route_id, envelope.msg_id, envelope.payload_len
        );

        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }
}

/// Server errors
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to send envelope through channel")]
    ChannelSendError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use godot_netlink_protocol::EnvelopeFlags;

    #[tokio::test]
    async fn test_server_echo() {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

        let server = Server::new(incoming_rx, outgoing_tx);
        tokio::spawn(async move {
            server.run().await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        incoming_tx.send(test_envelope.clone()).await.unwrap();

        let received = outgoing_rx.recv().await.unwrap();
        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.msg_id, test_envelope.msg_id);
        assert_eq!(received.payload, test_envelope.payload);
    }
}
