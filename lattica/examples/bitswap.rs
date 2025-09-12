use lattica::{network, common, rpc};
use std::time::{Duration, Instant};
use libp2p::gossipsub::PublishError::Duplicate;
use libp2p::kad::{Quorum, Record, RecordKey};
use tokio::signal;
use tokio::time::sleep_until;
use std::env;
use std::error::Error;
use futures::StreamExt;
use libp2p::Multiaddr;
use anyhow::Result;
use blockstore::block::Block;
use lattica::common::BytesBlock;

// node1: cargo run --example bitswap
// node2: cargo run --example bitswap /ip4/127.0.0.1/tcp/x/p2p/xxxxxxxx
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
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

    let cid = lattica.put_block(&BytesBlock("hello".as_bytes().to_vec())).await?;
    println!("put block {:?}", cid);
    let block = lattica.get_block(&cid).await?;
    println!("get block {:?}", String::from_utf8(block.data().to_vec())?);
    let result = lattica.remove_block(&cid).await;
    if result.is_err() {
        println!("remove block error: {:?}", cid);
    }

    signal::ctrl_c().await.expect("failed to listen for event");
    Ok(())
}