use bytes::Bytes;
use godot_netlink_client::transport::websocket::WebSocketClient;
use godot_netlink_protocol::{Envelope, EnvelopeFlags, Transport};
use godot_netlink_server::transport::websocket::WebSocketServer;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_multiple_clients_connect_simultaneously() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, _server_incoming_rx) = mpsc::channel(1000);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(1000);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    sleep(Duration::from_millis(200)).await;

    let num_clients = 10;
    let mut client_handles = Vec::new();

    for _i in 0..num_clients {
        let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
        let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

        let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let transport_handle = tokio::spawn(async move {
            let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
        });

        let client_loop_handle = tokio::spawn(async move {
            use godot_netlink_client::Client;
            let client = Client::new(client_incoming_rx, client_outgoing_tx);
            client.run().await;
        });

        client_handles.push((transport_handle, client_loop_handle));

        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(500)).await;

    for (transport, client_loop) in client_handles {
        transport.abort();
        client_loop.abort();
    }

    server_handle.abort();
}

#[tokio::test]
async fn test_clients_send_messages_concurrently() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(1000);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(1000);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    sleep(Duration::from_millis(200)).await;

    let num_clients = 5;
    let messages_per_client = 3;
    let mut client_handles = Vec::new();

    for client_id in 0..num_clients {
        let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
        let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

        let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let transport_handle = tokio::spawn(async move {
            let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
        });

        let send_handle = tokio::spawn(async move {
            use godot_netlink_client::Client;
            let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

            let client_handle = tokio::spawn(async move {
                client.run().await;
            });

            sleep(Duration::from_millis(200)).await;

            for msg_id in 0..messages_per_client {
                let envelope = Envelope::new_simple(
                    1,
                    1,
                    0,
                    100 + client_id as u16,
                    (client_id * messages_per_client + msg_id + 1) as u64,
                    EnvelopeFlags::RELIABLE,
                    Bytes::from(format!("client {} msg {}", client_id, msg_id)),
                );

                let _ = client_outgoing_tx.send(envelope).await;
                sleep(Duration::from_millis(50)).await;
            }

            client_handle.abort();
        });

        client_handles.push((transport_handle, send_handle));
    }

    let mut received_count = 0;
    let timeout = Duration::from_secs(5);
    let start = tokio::time::Instant::now();

    while received_count < num_clients * messages_per_client {
        match tokio::time::timeout(Duration::from_millis(100), server_incoming_rx.recv()).await {
            Ok(Some(_envelope)) => {
                received_count += 1;
            }
            Ok(None) => break,
            Err(_) => {
                if start.elapsed() > timeout {
                    break;
                }
            }
        }
    }

    assert_eq!(
        received_count,
        num_clients * messages_per_client,
        "Expected {} messages, received {}",
        num_clients * messages_per_client,
        received_count
    );

    for (transport, send) in client_handles {
        transport.abort();
        send.abort();
    }

    server_handle.abort();
}

#[tokio::test]
async fn test_clients_connect_and_disconnect() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, _server_incoming_rx) = mpsc::channel(1000);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(1000);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    sleep(Duration::from_millis(200)).await;

    for _ in 0..5 {
        let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
        let (_client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

        let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let transport_handle = tokio::spawn(async move {
            let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
        });

        let client_loop_handle = tokio::spawn(async move {
            use godot_netlink_client::Client;
            let client = Client::new(client_incoming_rx, _client_outgoing_tx);
            client.run().await;
        });

        sleep(Duration::from_millis(100)).await;

        transport_handle.abort();
        client_loop_handle.abort();

        sleep(Duration::from_millis(100)).await;
    }

    server_handle.abort();
}

#[tokio::test]
async fn test_rapid_connect_disconnect() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, _server_incoming_rx) = mpsc::channel(1000);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(1000);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    sleep(Duration::from_millis(200)).await;

    let mut handles = Vec::new();

    for _ in 0..20 {
        let (client_incoming_tx, _client_incoming_rx) = mpsc::channel(100);
        let (_client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

        let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let handle = tokio::spawn(async move {
            let transport_handle = tokio::spawn(async move {
                let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
            });

            sleep(Duration::from_millis(50)).await;
            transport_handle.abort();
        });

        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    sleep(Duration::from_millis(200)).await;

    server_handle.abort();
}
