use lattica::{network, common};
use std::time::{Duration, Instant};
use libp2p::kad::{Quorum, Record, RecordKey};
use tokio::signal;
use std::env;
use libp2p::Multiaddr;

// node1: cargo run --example dht
// node2: cargo run --example dht /ip4/127.0.0.1/tcp/x/p2p/xxxxxxxx
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    
    let args: Vec<String> = env::args().collect();
    let bootstrap_nodes = if args.len() > 1 {
        args[1..].into_iter().filter_map(|addr| {
            match addr.parse::<Multiaddr>() {
                Ok(addr) => Some(addr),
                Err(e) => {
                    tracing::warn!("Invalid bootstrap address '{}': {}", addr, e);
                    None
                }
            }
        }).collect()
    } else {
        vec![]
    };

    let mut lattica = network::Lattica::builder()
        .with_bootstrap_nodes(bootstrap_nodes.clone())
        .build().await?;

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // client node
    if !bootstrap_nodes.is_empty() {
        let current_time = common::time::get_current_timestamp();
        let expiration_time = current_time + 600.0;
        tracing::info!("1. store simple");
        lattica.store_simple("greeting", "Hello, World!".as_bytes().to_vec(), expiration_time).await?;
        if let Some(value) = lattica.get_with_subkey("greeting").await? {
            match value {
                common::types::DhtValue::Simple { value, expiration } => {
                    let content = String::from_utf8_lossy(&value);
                    tracing::info!("get simple value: '{}', exp: {}", content, expiration);
                }
                _ => {}
            }
        }

        tracing::info!("2. store with sub key");
        lattica.store_subkey("party_vote", "alice", "yes".as_bytes().to_vec(), expiration_time).await?;
        lattica.store_subkey("party_vote", "bob", "yes".as_bytes().to_vec(), expiration_time).await?;
        lattica.store_subkey("party_vote", "carol", "no".as_bytes().to_vec(), expiration_time).await?;
        lattica.store_subkey("party_vote", "david", "maybe".as_bytes().to_vec(), expiration_time).await?;
        tokio::time::sleep(Duration::from_millis(1000)).await;

        if let Some(value) = lattica.get_with_subkey("party_vote").await? {
            match value {
                common::types::DhtValue::WithSubkeys { subkeys } => {
                    tracing::info!("get with subkeys:");
                    for (subkey, vote_bytes, expiration) in subkeys {
                        let vote = String::from_utf8_lossy(&vote_bytes);
                        tracing::info!("  {}: {} (expiration: {})", subkey, vote, expiration);
                    }
                }
                _ => {}
            }
        } else {
            tracing::info!("get with no subkeys");
        }

        tracing::info!("3. store complex data");
        let user_status = serde_json::json!({
            "online": true,
            "last_seen": current_time,
            "mood": "happy"
        });
        let user_profile = serde_json::json!({
            "name": "Alice",
            "age": 25,
            "city": "Beijing"
        });
        lattica.store_subkey("user_status", "user_123", user_status.to_string().as_bytes().to_vec(), expiration_time).await?;
        lattica.store_subkey("user_status", "user_456", user_profile.to_string().as_bytes().to_vec(), expiration_time).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(value) = lattica.get_with_subkey("user_status").await? {
            match value {
                common::types::DhtValue::WithSubkeys { subkeys } => {
                    tracing::info!("user status:");
                    for (user_id, data_bytes, expiration) in subkeys {
                        let data_str = String::from_utf8_lossy(&data_bytes);
                        if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(&data_str) {
                            tracing::info!("  {}: {} (expiration: {})", user_id, json_data, expiration);
                        } else {
                            tracing::info!("  {}: {} (expiration: {})", user_id, data_str, expiration);
                        }
                    }
                }
                _ => {}
            }
        }

        tracing::info!("4. key expired");
        let short_expiration = current_time + 2.0;
        lattica.store_simple("temp_data", "expired after 2 seconds".as_bytes().to_vec(), short_expiration).await?;

        tracing::info!("store，expired after 2 seconds...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        match lattica.get_with_subkey("temp_data").await? {
            Some(_) => tracing::info!("expired failed"),
            None => tracing::info!("expired success"),
        }
    }

    signal::ctrl_c().await.expect("failed to listen for event");
    Ok(())
}