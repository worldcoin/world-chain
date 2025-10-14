use async_trait::async_trait;
use flashblocks_p2p::events::{
    NetworkPeersContext, PeerEventsDispatcherBuilder, PeerEventsListener,
};
use reth_eth_wire::EthNetworkPrimitives;
use reth_network::config::rng_secret_key;
use reth_network::events::PeerEvent;
use reth_network::types::PeerKind;
use reth_network::{NetworkConfigBuilder, NetworkManager, Peers, PeersInfo};
use reth_network_peers::PeerId;
use reth_storage_api::noop::NoopProvider;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

async fn create_test_network() -> NetworkManager<EthNetworkPrimitives> {
    let secret_key = rng_secret_key();

    let config = NetworkConfigBuilder::new(secret_key)
        .listener_port(0) // OS assigns random port
        .disable_discovery() // No peer discovery needed for tests
        .build(NoopProvider::default());

    NetworkManager::new(config)
        .await
        .expect("failed to create network manager")
}

struct EventCapturingListener {
    peer_added: Arc<Mutex<Vec<PeerId>>>,
    session_established: Arc<AtomicBool>,
    session_closed: Arc<AtomicBool>,
    peer_removed: Arc<Mutex<Vec<PeerId>>>,
}

impl EventCapturingListener {
    fn new() -> (
        Self,
        Arc<Mutex<Vec<PeerId>>>,
        Arc<AtomicBool>,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<PeerId>>>,
    ) {
        let peer_added = Arc::new(Mutex::new(Vec::new()));
        let session_established = Arc::new(AtomicBool::new(false));
        let session_closed = Arc::new(AtomicBool::new(false));
        let peer_removed = Arc::new(Mutex::new(Vec::new()));

        (
            Self {
                peer_added: peer_added.clone(),
                session_established: session_established.clone(),
                session_closed: session_closed.clone(),
                peer_removed: peer_removed.clone(),
            },
            peer_added,
            session_established,
            session_closed,
            peer_removed,
        )
    }
}

#[async_trait]
impl<N> PeerEventsListener<N> for EventCapturingListener
where
    N: NetworkPeersContext + Send + Sync,
{
    async fn on_peer_event(&self, _network: &N, event: &PeerEvent) {
        match event {
            PeerEvent::PeerAdded(peer_id) => {
                self.peer_added.lock().unwrap().push(*peer_id);
            }
            PeerEvent::SessionEstablished { .. } => {
                self.session_established.store(true, Ordering::SeqCst);
            }
            PeerEvent::SessionClosed { .. } => {
                self.session_closed.store(true, Ordering::SeqCst);
            }
            PeerEvent::PeerRemoved(peer_id) => {
                self.peer_removed.lock().unwrap().push(*peer_id);
            }
        }
    }
}

/// This test validates the full peer event flow:
/// 1. PeerAdded - when a peer is added to the network
/// 2. SessionEstablished - when connection/handshake completes
/// 3. SessionClosed - when peer disconnects gracefully
/// 4. PeerRemoved - when peer is explicitly removed
///
/// # Test Flow
/// 1. Create two isolated network instances
/// 2. Set up dispatcher with multiple listeners for all event types
/// 3. Add peer (triggers PeerAdded)
/// 4. Wait for connection (triggers SessionEstablished)
/// 5. Shutdown peer (triggers SessionClosed) -> TCP FIN
/// 6. Remove peer (triggers PeerRemoved)
/// 7. Verify all listeners received their respective events
/// 8. Test graceful dispatcher shutdown
#[tokio::test]
async fn test_dispatcher_handles_all_peer_event_types() -> eyre::Result<()> {
    println!("[DISPATCHER TEST] Creating two isolated network instances");
    let network_0 = create_test_network().await;
    let network_1 = create_test_network().await;

    let network_handle_0 = network_0.handle().clone();
    let network_handle_1 = network_1.handle().clone();

    tokio::spawn(network_0);
    let network_1_task = tokio::spawn(network_1);
    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("[DISPATCHER TEST] Setting up dispatcher with listeners for all event types");
    let (listener1, added1, established1, closed1, removed1) = EventCapturingListener::new();
    let (listener2, added2, established2, closed2, removed2) = EventCapturingListener::new();

    let dispatcher = PeerEventsDispatcherBuilder::new()
        .with_network(network_handle_0.clone())
        .add_listener(Box::new(listener1))
        .add_listener(Box::new(listener2))
        .build();

    let dispatcher_handle = dispatcher.start();
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("[DISPATCHER TEST] Phase 1: Adding peer (expect PeerAdded event)");
    let node_1_record = network_handle_1.local_node_record();
    let peer_id = node_1_record.id;
    network_handle_0.add_peer(peer_id, node_1_record.tcp_addr());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let added1_peers = added1.lock().unwrap().clone();
    let added2_peers = added2.lock().unwrap().clone();

    println!(
        "[DISPATCHER TEST] Phase 1 Results: Listener1 captured {} PeerAdded, Listener2 captured {}",
        added1_peers.len(),
        added2_peers.len()
    );

    assert!(
        !added1_peers.is_empty(),
        "Listener 1 should capture PeerAdded event"
    );
    assert!(
        !added2_peers.is_empty(),
        "Listener 2 should capture PeerAdded event"
    );
    assert!(
        added1_peers.contains(&peer_id),
        "Listener 1 should capture correct peer ID"
    );
    assert!(
        added2_peers.contains(&peer_id),
        "Listener 2 should capture correct peer ID"
    );

    println!("[DISPATCHER TEST] Phase 2: Waiting for connection (expect SessionEstablished event)");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let num_peers = network_handle_0.num_connected_peers();
    let established1_triggered = established1.load(Ordering::SeqCst);
    let established2_triggered = established2.load(Ordering::SeqCst);

    println!(
        "[DISPATCHER TEST] Phase 2 Results: {} peer(s) connected, Listener1={}, Listener2={}",
        num_peers, established1_triggered, established2_triggered
    );

    assert!(
        established1_triggered,
        "Listener 1 should receive SessionEstablished event"
    );
    assert!(
        established2_triggered,
        "Listener 2 should receive SessionEstablished event"
    );
    assert_eq!(num_peers, 1, "Should have exactly 1 connected peer");

    println!("[DISPATCHER TEST] Phase 3: Shutting down peer (expect SessionClosed event)");
    network_handle_1.shutdown().await?;
    network_1_task.abort();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let closed1_triggered = closed1.load(Ordering::SeqCst);
    let closed2_triggered = closed2.load(Ordering::SeqCst);

    println!(
        "[DISPATCHER TEST] Phase 3 Results: Listener1={}, Listener2={}",
        closed1_triggered, closed2_triggered
    );

    assert!(
        closed1_triggered,
        "Listener 1 should receive SessionClosed event"
    );
    assert!(
        closed2_triggered,
        "Listener 2 should receive SessionClosed event"
    );

    println!("[DISPATCHER TEST] Phase 4: Removing peer (expect PeerRemoved event)");
    network_handle_0.remove_peer(peer_id, PeerKind::Basic);

    tokio::time::sleep(Duration::from_millis(500)).await;

    let removed1_peers = removed1.lock().unwrap().clone();
    let removed2_peers = removed2.lock().unwrap().clone();

    println!(
        "[DISPATCHER TEST] Phase 4 Results: Listener1 captured {} PeerRemoved, Listener2 captured {}",
        removed1_peers.len(),
        removed2_peers.len()
    );

    assert!(
        !removed1_peers.is_empty(),
        "Listener 1 should capture PeerRemoved event"
    );
    assert!(
        !removed2_peers.is_empty(),
        "Listener 2 should capture PeerRemoved event"
    );
    assert!(
        removed1_peers.contains(&peer_id),
        "Listener 1 should capture correct peer ID in PeerRemoved"
    );
    assert!(
        removed2_peers.contains(&peer_id),
        "Listener 2 should capture correct peer ID in PeerRemoved"
    );

    dispatcher_handle.shutdown().await;

    Ok(())
}
