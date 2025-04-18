use eyre::eyre::Context;
use futures_util::SinkExt;
use std::sync::Arc;
use tokio::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{mpsc, Mutex},
    task::JoinHandle,
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

use crate::payload::FlashblocksPayloadV1;

/// Starts the websocket server that listens for incoming subscriber connections
pub fn start_ws(
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
    addr: impl ToSocketAddrs + Send + 'static,
) -> JoinHandle<eyre::Result<()>> {
    tokio::spawn(ws_server(subscribers, addr))
}

async fn ws_server(
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
    addr: impl ToSocketAddrs + Send + 'static,
) -> eyre::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .context("Failed to bind to addr")?;
    let addr = listener.local_addr().context("Failed to bind")?;
    let subscribers = subscribers.clone();

    tracing::info!("Starting WebSocket server on {}", addr);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => match accept_async(stream).await {
                Ok(ws_stream) => {
                    let mut subs = subscribers.lock().await;
                    subs.push(ws_stream);
                }
                Err(e) => tracing::error!("Error accepting websocket connection: {}", e),
            },
            Err(err) => {
                tracing::error!(err = %err.to_string(), "Error accepting a connection");
            }
        }
    }
}

/// Background task that handles publishing messages to WebSocket subscribers
pub fn publish_task(
    rx: mpsc::UnboundedReceiver<FlashblocksPayloadV1>,
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
) {
    tokio::spawn(publish_task_inner(rx, subscribers));
}

async fn publish_task_inner(
    mut rx: mpsc::UnboundedReceiver<FlashblocksPayloadV1>,
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
) -> eyre::Result<()> {
    while let Some(message) = rx.recv().await {
        let mut subscribers = subscribers.lock().await;

        // Remove disconnected subscribers and send message to connected ones
        let mut retained_subscribers = Vec::with_capacity(subscribers.len());
        for mut ws_stream in subscribers.drain(..) {
            let msg = serde_json::to_string(&message).context("Serializing payload")?;

            match ws_stream.send(Message::Text(msg.into())).await {
                Ok(()) => {
                    retained_subscribers.push(ws_stream);
                }
                Err(err) => {
                    tracing::warn!(err = %err.to_string(), "Error sending message - dropping connection");
                }
            }
        }
        *subscribers = retained_subscribers;
    }

    Ok(())
}
