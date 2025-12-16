//! Example bitswap node implementation allowing with basic interaction over cli
//!
//! It shows off example way to setup beetswap behaviour with a libp2p swarm and then either
//! connecting to another node and/or listening for incoming connections. In both cases both
//! the server and the client parts of the bitswap are running.
//!
//! Example invocations:
//!
//! Listen on port `9898` and serve single block with contents "12345"
//! ```sh
//! cargo run --example=node -- -l 9898 --preload-blockstore-string 12345
//! ```
//!
//! Connect to `10.0.0.101` on port `9898` and ask for CID `bafkreic...` (it's a CID of "12345" string above). You can specify multiple CIDs at once.
//! ```sh
//! cargo run --example=node  -- -p /ip4/10.0.0.101/tcp/9898 bafkreiczsrdrvoybcevpzqmblh3my5fu6ui3tgag3jm3hsxvvhaxhswpyu
//! ```
//!
//! Listen on port `9898` and requests provided CID from them until it gets correct response with
//! data
//! ```sh
//! cargo run --example=node  -- -l 9898 bafkreiczsrdrvoybcevpzqmblh3my5fu6ui3tgag3jm3hsxvvhaxhswpyu
//! ```
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use blockstore::{
    block::{Block, CidError},
    Blockstore, InMemoryBlockstore,
};
use cid::Cid;
use clap::Parser;
use libp2p::{
    futures::StreamExt, identify, noise, swarm::NetworkBehaviour, swarm::SwarmEvent, tcp, yamux,
    Multiaddr, SwarmBuilder,
};
use multihash_codetable::{Code, MultihashDigest};
use tracing::{debug, info};

const MAX_MULTIHASH_LENGHT: usize = 64;
const RAW_CODEC: u64 = 0x55;

#[derive(Debug, Parser)]
struct Args {
    /// Peers to connect to
    #[arg(short, long = "peer")]
    pub(crate) peers: Vec<Multiaddr>,

    /// CIDs to request
    pub(crate) cids: Vec<Cid>,

    /// Listen on provided port
    #[arg(short, long = "listen")]
    pub(crate) listen_port: Option<u16>,

    /// Load provided string into blockstore on start
    #[arg(long)]
    pub(crate) preload_blockstore_string: Vec<String>,
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    identify: identify::Behaviour,
    bitswap: beetswap::Behaviour<MAX_MULTIHASH_LENGHT, InMemoryBlockstore<MAX_MULTIHASH_LENGHT>>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let _guard = init_tracing();

    let blockstore = Arc::new(InMemoryBlockstore::new());
    for preload_string in args.preload_blockstore_string {
        let block = StringBlock(preload_string);
        let cid = block.cid()?;
        info!("inserted {cid} with content '{}'", block.0);
        blockstore.put_keyed(&cid, block.data()).await?;
    }

    let mut swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| Behaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                "/ipfs/id/1.0.0".to_string(),
                key.public(),
            )),
            bitswap: beetswap::Behaviour::new(blockstore),
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    if let Some(port) = args.listen_port {
        swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{port}").parse()?)?;
    }

    for peer in args.peers {
        swarm.dial(peer)?;
    }

    let mut queries = HashMap::new();
    for cid in args.cids {
        let query_id = swarm.behaviour_mut().bitswap.get(&cid);
        queries.insert(query_id, cid);
        info!("requested cid {cid}: {query_id:?}");
    }

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => debug!("Listening on {address:?}"),
            // Prints peer id identify info is being sent to.
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify)) => match identify {
                identify::Event::Sent { peer_id, .. } => {
                    info!("Sent identify info to {peer_id:?}");
                }
                identify::Event::Received { info, .. } => {
                    info!("Received {info:?}")
                }
                _ => (),
            },
            SwarmEvent::Behaviour(BehaviourEvent::Bitswap(bitswap)) => match bitswap {
                beetswap::Event::GetQueryResponse { query_id, data } => {
                    let cid = queries.get(&query_id).expect("unknown cid received");
                    info!("received response for {cid:?}: {data:?}");
                }
                beetswap::Event::GetQueryError { query_id, error } => {
                    let cid = queries.get(&query_id).expect("unknown cid received");
                    info!("received error for {cid:?}: {error}");
                }
            },
            _ => (),
        }
    }
}

struct StringBlock(pub String);

impl Block<64> for StringBlock {
    fn cid(&self) -> Result<Cid, CidError> {
        let hash = Code::Sha2_256.digest(self.0.as_ref());
        Ok(Cid::new_v1(RAW_CODEC, hash))
    }

    fn data(&self) -> &[u8] {
        self.0.as_ref()
    }
}

fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());

    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::fmt()
        .event_format(
            tracing_subscriber::fmt::format()
                .with_file(true)
                .with_line_number(true),
        )
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .init();

    guard
}
