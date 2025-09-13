//! This example shows an improved rendezvous client implementation based on
//! the standard libp2p pattern, with automatic discovery, periodic rediscovery,
//! and auto-connection to discovered addressbook.
use lattica::network;
use std::error::Error;
use libp2p::PeerId;
use std::time::Duration;
use tokio::time::{interval, sleep};
use anyhow::{Result, anyhow};

const RENDEZVOUS_NAMESPACE: &str = "network-demo";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    // The well-known rendezvous server PeerID (from the server example)
    let rendezvous_peer_id: PeerId = "xx".parse().map_err(|e| anyhow!("Failed to parse peer ID: {}", e))?;
    let rendezvous_addr: libp2p::Multiaddr = "/ip4/127.0.0.1/tcp/x".parse().map_err(|e| anyhow!("Failed to parse rendezvous address: {}", e))?;

    tracing::info!("Starting improved rendezvous client...");
    tracing::info!("Will connect to rendezvous server: {}", rendezvous_peer_id);
    tracing::info!("Using namespace: {}", RENDEZVOUS_NAMESPACE);

    // Create Lattica instance with rendezvous client enabled
    // NOTE: Rendezvous protocol requires external addresses to register addressbook
    // In production, use your actual external IP; for testing, we use a mock address
    let mut lattica = network::Lattica::builder()
        .with_listen_addrs(vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()]) // Local bind address
        .with_external_addr("/ip4/8.8.8.8/tcp/1234".parse().map_err(|e| anyhow!("Failed to parse external address: {}", e))?) // Mock external address for testing
        .with_rendezvous(true) // Enable rendezvous client
        .with_rendezvous_servers(vec![(rendezvous_peer_id, rendezvous_addr.clone())])
        .with_kad(false) // Disable Kademlia for this example
        .build().await.map_err(|e| anyhow::anyhow!("Failed to build Lattica: {}", e))?;

    let our_peer_id = lattica.peer_id();
    tracing::info!("Our PeerID: {}", our_peer_id);

    // Connect to the rendezvous server first
    tracing::info!("Connecting to rendezvous server...");

    // Dial the full address (including peer ID)
    let full_addr = format!("{}/p2p/{}", rendezvous_addr, rendezvous_peer_id);
    let dial_addr: libp2p::Multiaddr = full_addr.parse().map_err(|e| anyhow!("Failed to parse dial address: {}", e))?;

    if let Err(e) = lattica.dial(dial_addr).await {
        tracing::error!("Failed to connect to rendezvous server: {}", e);
        return Err(e);
    }
    tracing::info!("Connected to rendezvous server");

    // Wait a moment for connection to establish
    sleep(Duration::from_secs(2)).await;

    // Register ourselves to the rendezvous server
    tracing::info!("Registering to rendezvous server in namespace '{}'...", RENDEZVOUS_NAMESPACE);
    if let Err(e) = lattica.register_to_rendezvous(
        rendezvous_peer_id,
        Some(RENDEZVOUS_NAMESPACE.to_string()),
        None // Use default TTL
    ).await {
        tracing::error!("Failed to register: {}", e);
        return Err(e);
    }
    tracing::info!("Registration request sent");

    // Wait a moment for registration to complete
    sleep(Duration::from_secs(3)).await;

    // Initial discovery
    tracing::info!("Starting initial peer discovery...");
    if let Err(e) = lattica.discover_from_rendezvous(
        rendezvous_peer_id,
        Some(RENDEZVOUS_NAMESPACE.to_string()),
        Some(10) // Limit to 10 addressbook
    ).await {
        tracing::error!("Failed to discover addressbook: {}", e);
    }

    // Set up periodic discovery timer
    let mut discover_interval = interval(Duration::from_secs(30));
    let mut re_register_interval = interval(Duration::from_secs(240)); // Re-register every 4 minutes

    tracing::info!("Starting periodic discovery and re-registration...");
    tracing::info!("Discovery interval: 30 seconds");
    tracing::info!("Re-registration interval: 4 minutes");

    // Main event loop
    loop {
        tokio::select! {
            // Periodic discovery
            _ = discover_interval.tick() => {
                tracing::info!("Periodic peer discovery...");
                if let Err(e) = lattica.discover_from_rendezvous(
                    rendezvous_peer_id,
                    Some(RENDEZVOUS_NAMESPACE.to_string()),
                    Some(10)
                ).await {
                    tracing::warn!("Periodic discovery failed: {}", e);
                }
            }

            // Periodic re-registration
            _ = re_register_interval.tick() => {
                tracing::info!("Periodic re-registration...");
                if let Err(e) = lattica.register_to_rendezvous(
                    rendezvous_peer_id,
                    Some(RENDEZVOUS_NAMESPACE.to_string()),
                    None // Use default TTL
                ).await {
                    tracing::warn!("Periodic re-registration failed: {}", e);
                }
            }
        }
    }
}