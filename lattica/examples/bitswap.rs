use lattica::{network};
use libp2p::kad::{RecordKey};
use tokio::signal;
use std::env;
use std::error::Error;
use std::time::Duration;
use blockstore::block::Block;
use cid::Cid;
use libp2p::Multiaddr;
use lattica::common::BytesBlock;

// node1: cargo run --example bitswap
// node2: cargo run --example bitswap /ip4/127.0.0.1/tcp/x/p2p/xxxxxxxx  xxxxxx(cid)
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    let args: Vec<String> = env::args().collect();
    let bootstrap_nodes: Vec<Multiaddr> = if args.len() > 1 {
        match args[1].parse::<Multiaddr>() {
            Ok(addr) => vec![addr],
            Err(e) => {
                eprintln!("Invalid bootstrap address '{}': {}", args[1], e);
                vec![]
            }
        }
    } else {
        vec![]
    };

    let cid_arg = if args.len() > 2 {
        args[2].clone()
    } else {
        "".to_string()
    };

    let lattica = network::Lattica::builder()
        .with_bootstrap_nodes(bootstrap_nodes.clone())
        .build().await?;

    if bootstrap_nodes.len() > 0 {
        // get providers
        let record = RecordKey::new(&cid_arg);
        let peers = lattica.get_providers(record).await?;
        tracing::info!("get providers, peers: {:?}", peers);

        // get block
        let cid = Cid::try_from(cid_arg)?;
        let data_block = lattica.get_block(&cid, Duration::from_secs(10)).await?;
        let data = data_block.data();
        tracing::info!("get block,data: {:?}", str::from_utf8(&data)?);

    } else {
        // put block
        let cid = lattica.put_block(&BytesBlock("hello".as_bytes().to_vec())).await?;
        tracing::info!("put record cid: {:?}", cid);

        // start providing
        let record = RecordKey::new(&cid.to_string().as_str());
        lattica.start_providing(record).await?;
    }

    signal::ctrl_c().await.expect("failed to listen for event");
    Ok(())
}