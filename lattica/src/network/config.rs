use std::env::temp_dir;
use std::time::Duration;
use libp2p::identity::Keypair;
use libp2p::{Multiaddr, PeerId};
use crate::common::{CompressionAlgorithm, CompressionLevel};

#[derive(Clone)]
pub struct Config {
    pub keypair: Keypair,
    pub protocol_version: String,
    pub idle_timeout: Duration,
    pub bootstrap_nodes: Vec<Multiaddr>,
    pub listen_addrs: Vec<Multiaddr>,
    pub external_addrs: Vec<Multiaddr>,
    pub with_kad: bool,
    pub with_rendezvous: bool,
    pub rendezvous_servers: Vec<(PeerId, Multiaddr)>,
    pub with_mdns: bool,
    pub with_upnp: bool,
    pub with_autonat: bool,
    pub with_dcutr: bool,
    pub with_relay: bool,
    pub relay_servers: Vec<Multiaddr>,
    pub storage_path: String,
    pub compression_algorithm: CompressionAlgorithm,
    pub compression_level: CompressionLevel,
    pub dht_db_path: String
}

impl Default for Config {
    fn default() -> Self {
        let key = Keypair::generate_ed25519();

        let mut storage_path = temp_dir();
        storage_path.push(format!("db_{}", key.public().to_peer_id()));

        let mut dht_db_path = temp_dir();
        dht_db_path.push(format!("dht_{}", key.public().to_peer_id()));
        
        Self {
            listen_addrs: vec!["/ip4/0.0.0.0/tcp/0".parse().unwrap(),"/ip4/0.0.0.0/udp/0/quic-v1".parse().unwrap()],
            keypair: key,
            protocol_version: "/lattica/1.0.0".to_string(),
            idle_timeout: Duration::from_secs(60),
            bootstrap_nodes: vec![],
            external_addrs: vec![],
            with_kad: true,
            with_rendezvous: false,
            rendezvous_servers: vec![],
            with_mdns: true,
            with_upnp: true,
            with_autonat: false,
            with_dcutr: false,
            with_relay: false,
            relay_servers: vec![],
            storage_path: storage_path.to_str().unwrap().to_string(),
            compression_algorithm: CompressionAlgorithm::None,
            compression_level: CompressionLevel::Default,
            dht_db_path: dht_db_path.to_str().unwrap().to_string(),
        }
    }
}