//! Async runtime bridge between Tokio and Godot's game loop
//!
//! This module provides utilities for running async Rust code (tokio-based networking)
//! alongside Godot's single-threaded game loop.

use std::sync::{Arc, Mutex};
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

/// Shared tokio runtime for all networking operations
///
/// This runtime runs on a background thread and communicates with Godot
/// via mpsc channels. The Godot side polls these channels from _process().
pub struct AsyncRuntime {
    runtime: Arc<Runtime>,
    _handle: thread::JoinHandle<()>,
}

impl AsyncRuntime {
    /// Create a new async runtime on a background thread
    pub fn new() -> Self {
        let runtime = Arc::new(
            Runtime::new().expect("Failed to create tokio runtime")
        );

        let runtime_clone = Arc::clone(&runtime);
        let handle = thread::spawn(move || {
            // Keep runtime alive until dropped
            runtime_clone.block_on(async {
                // Park the thread indefinitely
                std::future::pending::<()>().await;
            });
        });

        Self {
            runtime,
            _handle: handle,
        }
    }

    /// Get a handle to spawn tasks
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}

impl Default for AsyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Message queue for communicating between async tasks and Godot's game loop
///
/// This allows Godot to poll for events from the background networking tasks
/// without blocking the game loop.
pub struct EventQueue<T> {
    rx: Arc<Mutex<mpsc::UnboundedReceiver<T>>>,
    tx: mpsc::UnboundedSender<T>,
}

impl<T> EventQueue<T> {
    /// Create a new event queue
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            rx: Arc::new(Mutex::new(rx)),
            tx,
        }
    }

    /// Get the sender side (for async tasks)
    pub fn sender(&self) -> mpsc::UnboundedSender<T> {
        self.tx.clone()
    }

    /// Try to receive a message (non-blocking, for Godot's _process)
    pub fn try_recv(&self) -> Option<T> {
        self.rx.lock().unwrap().try_recv().ok()
    }

    /// Drain all pending messages
    pub fn drain(&self) -> Vec<T> {
        let mut messages = Vec::new();
        while let Some(msg) = self.try_recv() {
            messages.push(msg);
        }
        messages
    }
}

impl<T> Default for EventQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for EventQueue<T> {
    fn clone(&self) -> Self {
        Self {
            rx: Arc::clone(&self.rx),
            tx: self.tx.clone(),
        }
    }
}
