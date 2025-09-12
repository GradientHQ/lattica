use lattica::network::Lattica;
use lattica::common::extract_peer_id;
use libp2p::{Multiaddr, PeerId};
use std::time::Duration;
use tokio::time::sleep;
use std::env;

async fn print_node_info(node_name: &str, node: &Lattica) {
    println!("{}:", node_name);
    let peers = node.get_all_peers().await;
    println!("  - peers: {}", peers.len());
    for peer_id in &peers {
        if let Some(peer_info) = node.get_peer_info(peer_id).await {
            println!("  - Peer {}: RTT = {:?}", peer_id, peer_info.rtt());
            let addresses = node.get_peer_addresses(peer_id).await;
            println!("    - address: {:?}", addresses);
            println!("    - peer_info: {:?}", peer_info.clone());
        }
    }
}

// cargo run --example ping
// cargo run --example ping -- /ip4/127.0.0.1/tcp/x/p2p/x
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let mut server_peer_id = PeerId::random();
    let mut bootstrap_nodes = vec![];
    if args.len() > 1 {
        let addr = args[1].parse::<Multiaddr>()?;
        bootstrap_nodes.push(addr.clone());

        if let Some(peer_id) = extract_peer_id(&addr) {
            server_peer_id = peer_id;
        }
    }

    if bootstrap_nodes.is_empty() {
        let mut node1 = Lattica::builder()
            // .with_listen_addrs(vec!["/ip4/127.0.0.1/tcp/0".parse().map_err(|e| anyhow::anyhow!("{}", e))?])
            .with_listen_addrs(vec!["/ip4/0.0.0.0/tcp/0".parse()?])
            // .with_listen_addrs(vec!["/ip4/0.0.0.0/udp/0/quic-v1".parse()?])
            // .with_listen_addr("/ip4/127.0.0.1/tcp/0".parse().map_err(|e| anyhow::anyhow!("{}", e))?)
            .with_mdns(true)
            .with_kad(true)
            .build()
            .await.map_err(|e| anyhow::anyhow!("{}", e))?;

        println!("node1 Peer ID: {}", node1.peer_id());

        loop {
            print_node_info("node1", &node1).await;
            sleep(Duration::from_secs(10)).await;
        }

    }else{
        // node2
        let mut node2 = Lattica::builder()
            // .with_listen_addrs(vec!["/ip4/127.0.0.1/tcp/0".parse().map_err(|e| anyhow::anyhow!("{}", e))?])
            .with_listen_addrs(vec!["/ip4/0.0.0.0/tcp/0".parse()?])
            // .with_listen_addrs(vec!["/ip4/0.0.0.0/udp/0/quic-v1".parse()?])
            // .with_listen_addr("/ip4/127.0.0.1/tcp/0".parse().map_err(|e| anyhow::anyhow!("{}", e))?)
            .with_mdns(true)
            .with_kad(true)
            .build()
            .await.map_err(|e| anyhow::anyhow!("{}", e))?;

        println!("node2 Peer ID: {}", node2.peer_id());

        let node1_addrs = bootstrap_nodes[0].clone();
        if node1_addrs.is_empty() {
            println!("node1 no address");
            return Ok(());
        }
        println!("node1 address: {:?}", node1_addrs);
        node2.dial(node1_addrs).await?;
        println!("\nbegin Ping test...");

        for i in 1..=5 {
            println!("Ping test {}: node2 ping node1", i);

            let before_rtt = node2.get_peer_rtt(&server_peer_id).await;
            println!("  - Ping before RTT: {:?}", before_rtt);

            sleep(Duration::from_secs(5)).await;

            let after_rtt = node2.get_peer_rtt(&server_peer_id).await;
            println!("  - Ping after RTT: {:?}", after_rtt);

            if let (Some(before), Some(after)) = (before_rtt, after_rtt) {
                let diff = if after > before {
                    after - before
                } else {
                    before - after
                };
                println!("  - RTT change: {:?}", diff);
            }

            sleep(Duration::from_secs(1)).await;
        }

        println!("\nPing done!");

        print_node_info("node2", &node2).await;
    }
    Ok(())
}