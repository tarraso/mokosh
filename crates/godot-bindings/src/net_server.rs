//! NetServer GDExtension class
//!
//! Godot-friendly wrapper around the Mokosh Server

use godot::prelude::*;
use godot::classes::{Node, INode};
use mokosh_server::Server;
use mokosh_protocol::{SessionEnvelope, SessionId};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::runtime::{AsyncRuntime, EventQueue};

/// Events emitted by the server
#[derive(Debug, Clone)]
pub enum ServerEvent {
    ClientConnected { session_id: u64 },
    ClientDisconnected { session_id: u64, reason: String },
    MessageReceived { session_id: u64, message: String },
    Error { error: String },
}

/// NetServer - Godot GDExtension class for server networking
///
/// Usage in GDScript:
/// ```gdscript
/// var server = NetServer.new()
/// add_child(server)  # Add as child node
/// server.client_connected.connect(_on_client_connected)
/// server.message_received.connect(_on_message)
/// server.start_server(8080)
/// ```
#[derive(GodotClass)]
#[class(base=Node)]
pub struct NetServer {
    base: Base<Node>,

    /// Shared async runtime
    runtime: Option<Arc<AsyncRuntime>>,

    /// Event queue for receiving events from async tasks
    events: Arc<EventQueue<ServerEvent>>,

    /// Outgoing message queue (Godot → Network)
    outgoing_tx: Option<mpsc::Sender<SessionEnvelope>>,

    /// Server running state
    is_running: bool,
}

#[godot_api]
impl INode for NetServer {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            runtime: None,
            events: Arc::new(EventQueue::new()),
            outgoing_tx: None,
            is_running: false,
        }
    }

    /// Called every frame by Godot - poll for network events
    fn process(&mut self, _delta: f64) {
        // Drain all pending events and emit signals
        for event in self.events.drain() {
            match event {
                ServerEvent::ClientConnected { session_id } => {
                    self.base_mut().emit_signal("client_connected", &[
                        (session_id as i64).to_variant()
                    ]);
                }
                ServerEvent::ClientDisconnected { session_id, reason } => {
                    self.base_mut().emit_signal("client_disconnected", &[
                        (session_id as i64).to_variant(),
                        reason.to_variant()
                    ]);
                }
                ServerEvent::MessageReceived { session_id, message } => {
                    self.base_mut().emit_signal("message_received", &[
                        (session_id as i64).to_variant(),
                        message.to_variant()
                    ]);
                }
                ServerEvent::Error { error } => {
                    self.base_mut().emit_signal("error_occurred", &[
                        error.to_variant()
                    ]);
                }
            }
        }
    }
}

#[godot_api]
impl NetServer {
    /// Signal emitted when a client connects
    #[signal]
    fn client_connected(session_id: i64);

    /// Signal emitted when a client disconnects
    #[signal]
    fn client_disconnected(session_id: i64, reason: GString);

    /// Signal emitted when message received from a client
    #[signal]
    fn message_received(session_id: i64, message: GString);

    /// Signal emitted on error
    #[signal]
    fn error_occurred(error: GString);

    /// Start the WebSocket server on the specified port
    ///
    /// # Arguments
    /// * `port` - Port to listen on (e.g., 8080)
    ///
    /// # Example
    /// ```gdscript
    /// server.start_server(8080)
    /// ```
    #[func]
    pub fn start_server(&mut self, port: i64) {
        if self.is_running {
            godot_print!("Error: Server already running");
            return;
        }

        // Create runtime if needed
        if self.runtime.is_none() {
            self.runtime = Some(Arc::new(AsyncRuntime::new()));
        }

        let runtime = self.runtime.as_ref().unwrap().clone();
        let events = Arc::clone(&self.events);
        let event_sender = events.sender();

        self.is_running = true;

        // Spawn server task
        runtime.handle().spawn(async move {
            // TODO: Implement WebSocket server
            // For now, send a placeholder error
            let _ = event_sender.send(ServerEvent::Error {
                error: format!("Server not yet implemented on port {}", port),
            });
        });
    }

    /// Stop the server
    #[func]
    pub fn stop_server(&mut self) {
        if !self.is_running {
            return;
        }

        self.outgoing_tx = None;
        self.is_running = false;

        godot_print!("Server stopped");
    }

    /// Send a message to a specific client
    ///
    /// # Arguments
    /// * `session_id` - Client session ID
    /// * `message` - JSON message as string
    ///
    /// # Example
    /// ```gdscript
    /// var msg = {"type": "world_state", "players": [...]}
    /// server.send_to_client(session_id, JSON.stringify(msg))
    /// ```
    #[func]
    pub fn send_to_client(&mut self, session_id: i64, message: GString) {
        if let Some(_tx) = &self.outgoing_tx {
            // TODO: Convert message to SessionEnvelope and send
            godot_print!("Would send to session {}: {}", session_id, message);
        } else {
            godot_print!("Error: Server not running");
        }
    }

    /// Broadcast a message to all connected clients
    ///
    /// # Arguments
    /// * `message` - JSON message as string
    #[func]
    pub fn broadcast_message(&mut self, message: GString) {
        if let Some(_tx) = &self.outgoing_tx {
            // TODO: Broadcast to all clients
            godot_print!("Would broadcast: {}", message);
        } else {
            godot_print!("Error: Server not running");
        }
    }

    /// Get the number of connected clients
    #[func]
    pub fn get_client_count(&self) -> i64 {
        // TODO: Get actual client count
        0
    }

    /// Get list of active client session IDs
    ///
    /// Returns an Array of session IDs
    #[func]
    pub fn get_active_clients(&self) -> Array<i64> {
        // TODO: Get actual active clients
        Array::new()
    }

    /// Check if server is running
    #[func]
    pub fn is_running(&self) -> bool {
        self.is_running
    }
}
