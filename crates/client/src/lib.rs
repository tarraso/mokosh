//! # GodotNetLink Client
//!
//! Client-side event loop for GodotNetLink protocol.
//!
//! ## Example
//!
//! ```no_run
//! use godot_netlink_client::Client;
//! use tokio::sync::mpsc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let (incoming_tx, incoming_rx) = mpsc::channel(100);
//!     let (outgoing_tx, outgoing_rx) = mpsc::channel(100);
//!
//!     let client = Client::new(incoming_rx, outgoing_tx);
//!     client.run().await;
//! }
//! ```

pub mod transport;

use godot_netlink_protocol::Envelope;
use tokio::sync::mpsc;

/// Client event loop handler
///
/// The client sends envelopes through the outgoing channel and receives
/// responses through the incoming channel.
pub struct Client {
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<Envelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<Envelope>,
}

impl Client {
    /// Creates a new client with the given channels
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
                    self.handle_envelope(envelope).await;
                }

                else => {
                    println!("Client shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles a single incoming envelope
    async fn handle_envelope(&mut self, envelope: Envelope) {
        println!(
            "Client received: route_id={}, msg_id={}, payload_len={}",
            envelope.route_id, envelope.msg_id, envelope.payload_len
        );
    }

    /// Sends an envelope to the server
    pub async fn send(&self, envelope: Envelope) -> Result<(), ClientError> {
        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)
    }
}

/// Client errors
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Failed to send envelope through channel")]
    ChannelSendError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use godot_netlink_protocol::EnvelopeFlags;

    #[tokio::test]
    async fn test_client_send() {
        let (_incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

        let client = Client::new(incoming_rx, outgoing_tx.clone());

        tokio::spawn(async move {
            client.run().await;
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

        let client_handle = Client::new(mpsc::channel(1).1, outgoing_tx);
        client_handle.send(test_envelope.clone()).await.unwrap();

        let received = outgoing_rx.recv().await.unwrap();
        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.payload, test_envelope.payload);
    }

    #[tokio::test]
    async fn test_client_receive() {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, _outgoing_rx) = mpsc::channel(10);

        let client = Client::new(incoming_rx, outgoing_tx);

        let handle = tokio::spawn(async move {
            client.run().await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"response"),
        );

        incoming_tx.send(test_envelope).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        drop(incoming_tx);

        handle.await.unwrap();
    }
}
