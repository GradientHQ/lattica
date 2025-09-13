use std::{error::Error, str::FromStr};
use std::net::Ipv4Addr;
use clap::{Parser};
use libp2p::{core::multiaddr::{Multiaddr, Protocol}, PeerId};
use tokio::signal;
use lattica::network;
use tokio::time::{sleep, Duration};

#[derive(Debug, Parser)]
#[command(name = "libp2p DCUtR client")]
struct Opts {
    /// The mode (client-listen, client-dial).
    #[arg(long)]
    mode: Mode,

    #[arg(long, default_value_t = 0)]
    port: u16,

    /// The listening address
    #[arg(long, value_delimiter = ',')]
    relay_addresses: Vec<Multiaddr>,

    /// Peer ID of the remote peer to hole punch to.
    #[arg(long)]
    remote_peer_id: Option<PeerId>,
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode {
    Dial,
    Listen,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "dial" => Ok(Mode::Dial),
            "listen" => Ok(Mode::Listen),
            _ => Err("Expected either 'dial' or 'listen'".to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let opts = Opts::parse();

    let listen_addr_tcp = Multiaddr::empty()
        .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(Protocol::Tcp(opts.port));

    let listen_addr_quic = Multiaddr::empty()
        .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(Protocol::Udp(opts.port))
        .with(Protocol::QuicV1);

    let mut lattica =  network::Lattica::builder()
        .with_autonat(false)
        .with_dcutr(true)
        .with_listen_addrs(vec![listen_addr_tcp, listen_addr_quic])
        .with_relay_servers(opts.relay_addresses.clone())
        .build().await?;

    match opts.mode {
        Mode::Dial => {
            sleep(Duration::from_secs(1)).await;
            for addr in opts.relay_addresses.clone() {
                lattica.dial(addr
                    .with(Protocol::P2pCircuit)
                    .with(Protocol::P2p(opts.remote_peer_id.unwrap()))).await.unwrap();
            }
        }
        _ => {}
    }

    signal::ctrl_c().await.expect("failed to listen for event");
    Ok(())
}