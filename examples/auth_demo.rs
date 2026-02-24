//! Authentication demo
//!
//! Demonstrates the authentication flow with MockAuthProvider.
//!
//! This example shows:
//! - Server configuration with auth_required = true
//! - MockAuthProvider accepting "mock" method
//! - Client authenticate() API
//! - AUTH_REQUEST/AUTH_RESPONSE message exchange

use mokosh_protocol::auth::{AuthProvider, MockAuthProvider};
use mokosh_server::ServerConfig;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("=== Mokosh Authentication Demo ===\n");

    // Create MockAuthProvider
    let auth_provider = Arc::new(MockAuthProvider);
    println!("Created MockAuthProvider");
    println!("  Supported methods: {:?}\n", auth_provider.supported_methods());

    // Test authentication directly
    println!("Testing authentication with 'mock' method:");
    match auth_provider.authenticate("mock", b"test-credentials").await {
        Ok(result) => {
            println!("  Result: {:?}", result);
        }
        Err(e) => {
            println!("  Error: {}", e);
        }
    }

    println!("\nTesting authentication with unsupported method:");
    match auth_provider.authenticate("passcode", b"secret123").await {
        Ok(result) => {
            println!("  Result: {:?}", result);
        }
        Err(e) => {
            println!("  Error: {}", e);
        }
    }

    // Show server configuration
    println!("\n=== Server Configuration ===");
    let config = ServerConfig {
        auth_required: true,
        ..Default::default()
    };
    println!("  auth_required: {}", config.auth_required);
    println!("  hello_timeout: {:?}", config.hello_timeout);
    println!("  keepalive_interval: {:?}", config.keepalive_interval);

    println!("\n=== Usage Example ===");
    println!("Server setup:");
    println!("  let auth_provider = Arc::new(MockAuthProvider);");
    println!("  let mut config = ServerConfig::default();");
    println!("  config.auth_required = true;");
    println!("  let server = Server::with_auth(");
    println!("      incoming_rx, outgoing_tx,");
    println!("      CodecType::from_id(1).unwrap(),");
    println!("      CodecType::from_id(1).unwrap(),");
    println!("      config, None, Some(auth_provider),");
    println!("  );");

    println!("\nClient authentication:");
    println!("  client.authenticate(\"mock\", b\"my-credentials\").await?;");

    println!("\n=== Authentication Flow ===");
    println!("1. Client connects (HELLO → HELLO_OK)");
    println!("2. HELLO_OK includes auth_required=true");
    println!("3. Client calls authenticate(method, credentials)");
    println!("4. Client sends AUTH_REQUEST");
    println!("5. Server validates via AuthProvider");
    println!("6. Server sends AUTH_RESPONSE");
    println!("7. Client transitions to Authorized state");
    println!("8. Client can now send game messages");

    println!("\nDemo complete!");
}
