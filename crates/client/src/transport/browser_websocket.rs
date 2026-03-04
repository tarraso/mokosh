//! Browser WebSocket transport for WASM target
//!
//! This module provides WebSocket connectivity for browser environments using
//! the browser's native WebSocket API through wasm-bindgen.

use super::Transport;
use async_trait::async_trait;
use bytes::Bytes;
use js_sys::{ArrayBuffer, Uint8Array};
use mokosh_protocol::Envelope;
use std::cell::RefCell;
use std::rc::Rc;
use crate::compat::mpsc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, CloseEvent, ErrorEvent, MessageEvent, WebSocket};

/// Browser WebSocket client for WASM environments
///
/// Uses the browser's native WebSocket API to communicate with the server.
/// This transport is only available when compiled to `wasm32-unknown-unknown`.
pub struct BrowserWebSocketClient {
    url: String,
}

impl BrowserWebSocketClient {
    /// Creates a new browser WebSocket client for the given URL
    ///
    /// # Arguments
    /// * `url` - WebSocket URL (e.g., "ws://localhost:8080" or "wss://example.com")
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait(?Send)]
impl Transport for BrowserWebSocketClient {
    type Error = BrowserWebSocketError;

    async fn run(
        self,
        mut incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        tracing::info!(url = %self.url, "Connecting to WebSocket server (browser)");

        // Create WebSocket connection
        let ws = WebSocket::new(&self.url)
            .map_err(|e| BrowserWebSocketError::ConnectionError(format!("{:?}", e)))?;

        // Set binary type to ArrayBuffer (required for binary data)
        ws.set_binary_type(BinaryType::Arraybuffer);

        // Create channels for event handlers
        let (event_tx, mut event_rx) = mpsc::channel::<WsEvent>(32);

        // Setup message handler
        let mut event_tx_msg = event_tx.clone();
        let onmessage_callback = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(abuf) = e.data().dyn_into::<ArrayBuffer>() {
                let array = Uint8Array::new(&abuf);
                let data = array.to_vec();

                // Try to parse envelope
                match Envelope::from_bytes(Bytes::from(data)) {
                    Ok(envelope) => {
                        let _ = event_tx_msg.try_send(WsEvent::Message(envelope));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to parse envelope from browser WebSocket");
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
        onmessage_callback.forget(); // Keep callback alive

        // Setup error handler
        let mut event_tx_err = event_tx.clone();
        let onerror_callback = Closure::wrap(Box::new(move |e: ErrorEvent| {
            let msg = format!("WebSocket error: {:?}", e);
            tracing::error!("{}", msg);
            let _ = event_tx_err.try_send(WsEvent::Error(msg));
        }) as Box<dyn FnMut(ErrorEvent)>);
        ws.set_onerror(Some(onerror_callback.as_ref().unchecked_ref()));
        onerror_callback.forget();

        // Setup close handler
        let mut event_tx_close = event_tx.clone();
        let onclose_callback = Closure::wrap(Box::new(move |e: CloseEvent| {
            tracing::info!(code = e.code(), reason = %e.reason(), "WebSocket closed");
            let _ = event_tx_close.try_send(WsEvent::Close);
        }) as Box<dyn FnMut(CloseEvent)>);
        ws.set_onclose(Some(onclose_callback.as_ref().unchecked_ref()));
        onclose_callback.forget();

        // Setup open handler
        let mut event_tx_open = event_tx.clone();
        let onopen_callback = Closure::wrap(Box::new(move |_| {
            tracing::info!("Browser WebSocket connection established");
            let _ = event_tx_open.try_send(WsEvent::Open);
        }) as Box<dyn FnMut(JsValue)>);
        ws.set_onopen(Some(onopen_callback.as_ref().unchecked_ref()));
        onopen_callback.forget();

        // Keep WebSocket in Rc<RefCell> to share between async blocks
        let ws = Rc::new(RefCell::new(Some(ws)));

        // Wait for WebSocket to open
        use futures::StreamExt;
        tracing::info!("Waiting for WebSocket to open...");

        loop {
            match event_rx.next().await {
                Some(WsEvent::Open) => {
                    tracing::info!("WebSocket opened, ready to send/receive");
                    break;
                }
                Some(WsEvent::Error(msg)) => {
                    tracing::error!("WebSocket error before open: {}", msg);
                    return Err(BrowserWebSocketError::ConnectionError(msg));
                }
                Some(WsEvent::Close) => {
                    tracing::error!("WebSocket closed before open");
                    return Err(BrowserWebSocketError::ConnectionError("Closed before open".to_string()));
                }
                None => {
                    tracing::error!("Event channel closed before WebSocket open");
                    return Err(BrowserWebSocketError::ConnectionError("Channel closed".to_string()));
                }
                _ => {
                    // Ignore other events before open
                }
            }
        }

        // Main event loop (only after WebSocket is open)
        loop {
            futures::select! {
                // Handle WebSocket events
                event = event_rx.next() => {
                    match event {
                        Some(WsEvent::Open) => {
                            // Already open, ignore
                        }
                        Some(WsEvent::Message(envelope)) => {
                            // Forward to event loop
                            if let Err(e) = incoming_tx.send(envelope).await {
                                tracing::error!(error = %e, "Failed to send envelope to event loop");
                                break;
                            }
                        }
                        Some(WsEvent::Error(msg)) => {
                            tracing::error!("WebSocket error event: {}", msg);
                            // Continue - browser will trigger close event
                        }
                        Some(WsEvent::Close) => {
                            tracing::info!("WebSocket closed by server");
                            break;
                        }
                        None => {
                            tracing::info!("WebSocket event channel closed");
                            break;
                        }
                    }
                }

                // Handle outgoing messages
                envelope = outgoing_rx.next() => {
                    match envelope {
                        Some(envelope) => {
                            if let Some(ws_ref) = ws.borrow().as_ref() {
                                let bytes = envelope.to_bytes();
                                let array = Uint8Array::from(bytes.as_ref());

                                if let Err(e) = ws_ref.send_with_array_buffer(&array.buffer()) {
                                    tracing::error!(error = ?e, "Failed to send message");
                                    break;
                                }
                            } else {
                                tracing::error!("WebSocket is None, cannot send message");
                                break;
                            }
                        }
                        None => {
                            tracing::info!("Browser WebSocket client shutting down");
                            break;
                        }
                    }
                }
            }
        }

        // Close WebSocket if still open
        if let Some(ws_instance) = ws.borrow_mut().take() {
            let _ = ws_instance.close();
        }

        Ok(())
    }
}

/// WebSocket events from browser
enum WsEvent {
    Open,
    Message(Envelope),
    Error(String),
    Close,
}

/// Browser WebSocket errors
#[derive(Debug, thiserror::Error)]
pub enum BrowserWebSocketError {
    #[error("Failed to connect: {0}")]
    ConnectionError(String),

    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    #[error("Channel error: {0}")]
    ChannelError(String),
}
