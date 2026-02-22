//! NetClient GDExtension class
//!
//! Godot-friendly wrapper around the GodotNetLink Client

use godot::prelude::*;
use godot::classes::{Node, INode};
use std::sync::Arc;
use tokio::sync::mpsc;
use godot_netlink_protocol::{Envelope, CodecType};
use godot_netlink_client::{Client, transport::websocket::WebSocketClient};
use godot_netlink_protocol::Transport;

use crate::runtime::{AsyncRuntime, EventQueue};

/// Events from the client to be processed in Godot's main thread
#[derive(Debug, Clone)]
enum ClientEvent {
    Connected,
    Disconnected { reason: String },
    Error { error: String },
}

/// NetClient - Godot GDExtension class for client networking
///
/// Usage in GDScript:
/// ```gdscript
/// var client = NetClient.new()
/// add_child(client)  # Add as child node
/// client.connected.connect(_on_connected)
/// client.message_received.connect(_on_message)
/// client.connect_to_server("ws://localhost:8080")
/// ```
#[derive(GodotClass)]
#[class(base=Node)]
pub struct NetClient {
    base: Base<Node>,

    /// Shared async runtime
    runtime: Option<Arc<AsyncRuntime>>,

    /// Channel to send envelopes to the client (for sending messages from Godot)
    outgoing_tx: Option<mpsc::Sender<Envelope>>,

    /// Channel to receive game messages from the client
    game_messages_rx: Option<Arc<tokio::sync::Mutex<mpsc::Receiver<Envelope>>>>,

    /// Event queue for connection events (connected/disconnected/error)
    events: Arc<EventQueue<ClientEvent>>,

    /// Current connection state
    is_connected: bool,

    /// Codec to use for serialization (JSON by default)
    codec: CodecType,

    /// Message ID counter for outgoing messages
    msg_id_counter: u64,
}

#[godot_api]
impl INode for NetClient {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            runtime: None,
            outgoing_tx: None,
            game_messages_rx: None,
            events: Arc::new(EventQueue::new()),
            is_connected: false,
            codec: CodecType::from_id(1).unwrap(), // JSON by default
            msg_id_counter: 1,
        }
    }

    /// Called every frame by Godot - poll for network events
    fn process(&mut self, _delta: f64) {
        // Process connection events
        for event in self.events.drain() {
            match event {
                ClientEvent::Connected => {
                    self.is_connected = true;
                    self.base_mut().emit_signal("connected", &[]);
                }
                ClientEvent::Disconnected { reason } => {
                    self.is_connected = false;
                    self.base_mut().emit_signal("disconnected", &[reason.to_variant()]);
                }
                ClientEvent::Error { error } => {
                    self.base_mut().emit_signal("error_occurred", &[error.to_variant()]);
                }
            }
        }

        // Collect messages first, then process them
        let mut messages = Vec::new();

        // Poll for game messages from the client
        if let Some(rx) = &self.game_messages_rx {
            // Try to lock the receiver (non-blocking)
            if let Ok(mut rx_guard) = rx.try_lock() {
                // Drain all pending messages
                while let Ok(envelope) = rx_guard.try_recv() {
                    messages.push(envelope);
                }
            }
        }

        // Process messages and emit signals
        for envelope in messages {
            // Decode the payload using the codec
            match self.codec.decode::<serde_json::Value>(&envelope.payload) {
                Ok(json_value) => {
                    // Convert to JSON string for GDScript
                    let json_str = serde_json::to_string(&json_value)
                        .unwrap_or_else(|_| "{}".to_string());

                    self.base_mut().emit_signal("message_received", &[
                        json_str.to_variant()
                    ]);
                }
                Err(e) => {
                    eprintln!("Failed to decode message: {}", e);
                }
            }
        }
    }
}

#[godot_api]
impl NetClient {
    /// Signal emitted when connection established
    #[signal]
    fn connected();

    /// Signal emitted when disconnected
    #[signal]
    fn disconnected(reason: GString);

    /// Signal emitted when message received
    #[signal]
    fn message_received(message: GString);

    /// Signal emitted on error
    #[signal]
    fn error_occurred(error: GString);

    /// Connect to a WebSocket server
    ///
    /// # Arguments
    /// * `url` - WebSocket URL (e.g., "ws://localhost:8080")
    ///
    /// # Example
    /// ```gdscript
    /// client.connect_to_server("ws://localhost:8080")
    /// ```
    #[func]
    pub fn connect_to_server(&mut self, url: GString) {
        let url_str = url.to_string();
        eprintln!("NetClient: Connecting to {}", url_str);

        // Create runtime if needed
        if self.runtime.is_none() {
            self.runtime = Some(Arc::new(AsyncRuntime::new()));
        }

        let runtime = self.runtime.as_ref().unwrap().clone();

        // Create channels for transport layer
        let (transport_incoming_tx, transport_incoming_rx) = mpsc::channel(100);
        let (transport_outgoing_tx, transport_outgoing_rx) = mpsc::channel(100);

        // Create channel for receiving game messages
        let (game_messages_tx, game_messages_rx) = mpsc::channel(100);

        // Clone outgoing_tx for sending messages from Godot
        let outgoing_tx_clone = transport_outgoing_tx.clone();
        self.outgoing_tx = Some(outgoing_tx_clone);

        // Store game_messages_rx for polling in process()
        self.game_messages_rx = Some(Arc::new(tokio::sync::Mutex::new(game_messages_rx)));

        // Create Client with game messages channel
        let client = Client::with_game_messages(
            transport_incoming_rx,
            transport_outgoing_tx,
            Some(game_messages_tx),
        );

        // Mark as connected (will be updated by client events)
        self.is_connected = false;

        let events = Arc::clone(&self.events);
        let events_clone = Arc::clone(&events);

        // Spawn WebSocket transport task
        runtime.handle().spawn(async move {
            let transport = WebSocketClient::new(url_str.clone());
            if let Err(e) = transport.run(transport_incoming_tx, transport_outgoing_rx).await {
                eprintln!("Transport error: {}", e);
            }
        });

        // Spawn client connection and event loop
        runtime.handle().spawn(async move {
            let mut client = client;

            // Connect (HELLO handshake)
            match client.connect().await {
                Ok(_) => {
                    eprintln!("Client connected successfully");
                    let _ = events_clone.sender().send(ClientEvent::Connected);
                }
                Err(e) => {
                    eprintln!("Connection failed: {}", e);
                    let _ = events_clone.sender().send(ClientEvent::Error {
                        error: format!("Connection failed: {}", e),
                    });
                    return;
                }
            }

            // Run the client event loop
            client.run().await;
            eprintln!("Client disconnected");
            let _ = events_clone.sender().send(ClientEvent::Disconnected {
                reason: "Connection closed".to_string(),
            });
        });
    }

    /// Disconnect from server
    #[func]
    pub fn disconnect(&mut self) {
        if let Some(tx) = &self.outgoing_tx {
            // Send DISCONNECT message to server
            let disconnect = godot_netlink_protocol::messages::Disconnect {
                reason: godot_netlink_protocol::messages::DisconnectReason::ClientRequested,
                message: "Client disconnecting".to_string(),
            };

            // Serialize disconnect message
            let payload = self.codec.encode(&disconnect).unwrap_or_default();

            let disconnect_envelope = godot_netlink_protocol::Envelope::new_simple(
                godot_netlink_protocol::CURRENT_PROTOCOL_VERSION,
                self.codec.id(),
                0,
                godot_netlink_protocol::messages::routes::DISCONNECT,
                0,
                godot_netlink_protocol::EnvelopeFlags::RELIABLE,
                payload,
            );

            let tx_clone = tx.clone();
            if let Some(runtime) = &self.runtime {
                runtime.handle().spawn(async move {
                    let _ = tx_clone.send(disconnect_envelope).await;
                    // Small delay to ensure message is sent
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                });
            }
        }

        self.outgoing_tx = None;
        self.game_messages_rx = None;
        self.is_connected = false;
        eprintln!("NetClient: Disconnected");
    }

    /// Send a message to the server
    ///
    /// # Arguments
    /// * `message` - JSON message as string
    ///
    /// # Example
    /// ```gdscript
    /// var msg = {"type": "player_input", "x": 10, "y": 20}
    /// client.send_message(JSON.stringify(msg))
    /// ```
    #[func]
    pub fn send_message(&mut self, message: GString) {
        let msg_str = message.to_string();

        if let Some(tx) = &self.outgoing_tx {
            // Convert string to Bytes (JSON is already encoded as string)
            let payload = bytes::Bytes::from(msg_str.as_bytes().to_vec());

            // Get and increment message ID
            let msg_id = self.msg_id_counter;
            self.msg_id_counter += 1;

            // Create envelope
            let envelope = Envelope::new_simple(
                godot_netlink_protocol::CURRENT_PROTOCOL_VERSION,
                self.codec.id(),
                0, // schema_hash
                100, // Default route_id for game messages
                msg_id,
                godot_netlink_protocol::EnvelopeFlags::RELIABLE,
                payload,
            );

            // Send envelope
            let tx_clone = tx.clone();
            if let Some(runtime) = &self.runtime {
                runtime.handle().spawn(async move {
                    if let Err(e) = tx_clone.send(envelope).await {
                        eprintln!("Error sending envelope: {}", e);
                    }
                });
            }
        } else {
            eprintln!("Error: Not connected to server");
        }
    }

    /// Get current round-trip time (RTT) in milliseconds
    ///
    /// Returns -1.0 if not connected or no RTT measurement available
    #[func]
    pub fn get_rtt(&self) -> f64 {
        // TODO: Get actual RTT from client
        -1.0
    }

    /// Check if currently connected to server
    #[func]
    pub fn is_connected(&self) -> bool {
        self.is_connected
    }
}
