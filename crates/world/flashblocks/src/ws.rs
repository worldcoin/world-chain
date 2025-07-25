// TODO: Remove this after P2P Handle is used as sink of messages in the tests
use futures_util::SinkExt;
use rollup_boost::FlashblocksPayloadV1;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::Mutex,
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

pub fn new_subscribers() -> Arc<Mutex<Vec<WebSocketStream<TcpStream>>>> {
    Arc::new(Mutex::new(Vec::default()))
}

pub async fn ws_server(
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
    addr: impl ToSocketAddrs + Send + 'static,
) {
    let listener = TcpListener::bind(addr)
        .await
        .expect("Failed to bind WebSocket server");
    let addr = listener.local_addr().expect("Failed to get local address");
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
pub async fn publish_task(
    mut rx: broadcast::Receiver<FlashblocksPayloadV1>,
    subscribers: Arc<Mutex<Vec<WebSocketStream<TcpStream>>>>,
) {
    while let Ok(message) = rx.recv().await {
        let mut subscribers = subscribers.lock().await;

        // Remove disconnected subscribers and send message to connected ones
        let mut retained_subscribers = Vec::with_capacity(subscribers.len());
        for mut ws_stream in subscribers.drain(..) {
            let msg = serde_json::to_string(&message).expect("Failed to serialize message");

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
}
