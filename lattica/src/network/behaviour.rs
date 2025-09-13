use std::sync::Arc;
use std::time::Duration;
use super::{*};
use crate::rpc;
use tokio::sync::{oneshot};
use libp2p::{autonat, dcutr, gossipsub, identify, kad, mdns, ping, relay, rendezvous, upnp, swarm::{NetworkBehaviour,behaviour::toggle::Toggle},
             kad::{Quorum, Record, PeerRecord, RecordKey, K_VALUE}, request_response, PeerId,
             gossipsub::{MessageAuthenticity}};
use anyhow::{anyhow, Result};
use blockstore::{SledBlockstore};
use fnv::{FnvHashMap};
use libp2p_stream as stream;
use crate::common::{BytesBlock, QueryId, P2P_CIRCUIT_TOPIC};
use cid::Cid;

pub enum QueryChannel {
    GetRecord(
        usize,
        Vec<PeerRecord>,
        oneshot::Sender<Result<Vec<PeerRecord>>>,
    ),
    PutRecord(oneshot::Sender<Result<()>>),
    Bootstrap(oneshot::Sender<Result<()>>),
    RendezvousRegister(oneshot::Sender<Result<()>>),
    RendezvousDiscover(oneshot::Sender<Result<Vec<PeerId>>>),
    Get(oneshot::Sender<Result<BytesBlock>>),
    StartProviding(oneshot::Sender<Result<()>>),
    GetProviders(oneshot::Sender<Result<Vec<PeerId>>>),
}

const MAX_MULTIHASH_LENGHT: usize = 64;
const RAW_CODEC: u64 = 0x55;
#[derive(NetworkBehaviour)]
pub struct LatticaBehaviour{
    pub kad: Toggle<kad::Behaviour<MultiStore>>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub request_response: request_response::Behaviour<rpc::RpcCodec>,
    pub stream: stream::Behaviour,
    pub rendezvous: Toggle<rendezvous::client::Behaviour>,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub upnp: Toggle<upnp::tokio::Behaviour>,
    pub autonat: Toggle<autonat::v2::client::Behaviour>,
    pub dcutr: Toggle<dcutr::Behaviour>,
    pub relay: Toggle<relay::client::Behaviour>,
    pub gossipsub: gossipsub::Behaviour,
    pub bitswap: Toggle<beetswap::Behaviour<64, SledBlockstore>>
}

impl LatticaBehaviour{
    pub fn new(config: &mut Config, relay_behavior: Option<relay::client::Behaviour>, storage: Arc<SledBlockstore>) -> Self {
        let peer_id = config.keypair.public().to_peer_id();

        let protocols = std::iter::once((
            rpc::RPC_REQUEST_RESPONSE_PROTOCOL,
            request_response::ProtocolSupport::Full
        ));

        let kad = if config.with_kad {
            let mut store = MultiStore::new(peer_id, config.dht_db_path.clone()).unwrap();
            store.warm_up().unwrap();

            let mut kad_cfg = kad::Config::default();
            kad_cfg.set_query_timeout(Duration::from_secs(1));
            Toggle::from(Some(kad::Behaviour::with_config(peer_id, store, kad_cfg)))
        } else {
            Toggle::from(None)
        };

        let rendezvous = if config.with_rendezvous {
            tracing::debug!("Enabling rendezvous client behavior");
            Toggle::from(Some(rendezvous::client::Behaviour::new(config.keypair.clone())))
        } else {
            tracing::debug!("Rendezvous client behavior disabled");
            Toggle::from(None)
        };

        let mdns = if config.with_mdns {
            match mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id) {
                Ok(behaviour) => Toggle::from(Some(behaviour)),
                Err(e )=> {
                    tracing::error!("mdns config error: {:?}", e);
                    Toggle::from(None)
                }
            }
        } else {
            Toggle::from(None)
        };

        let upnp = if config.with_upnp {
            tracing::debug!("Enabling upnp behavior");
            Toggle::from(Some(upnp::tokio::Behaviour::default()))
        } else {
            Toggle::from(None)
        };

        let autonat = if config.with_autonat {
            tracing::debug!("Enabling autonat client behavior");
            Toggle::from(Some(autonat::v2::client::Behaviour::default()))
        } else {
            tracing::debug!("autonat client behavior disabled");
            Toggle::from(None)
        };

        let dcutr = if config.with_dcutr {
            tracing::debug!("Enabling DCUtR behavior");
            Toggle::from(Some(dcutr::Behaviour::new(peer_id)))
        } else {
            tracing::debug!("DCUtR behavior disabled");
            Toggle::from(None)
        };

        let mut relay = Toggle::from(None);
        if config.with_relay {
            tracing::debug!("Enabling relay behavior");
            relay = Toggle::from(relay_behavior)
        }

        let gossipsub_config = gossipsub::Config::default();
        let mut gossipsub = gossipsub::Behaviour::new(MessageAuthenticity::Signed(config.keypair.clone()), gossipsub_config).unwrap();
        let topic = gossipsub::IdentTopic::new(P2P_CIRCUIT_TOPIC);
        gossipsub.subscribe(&topic).unwrap();

        let sd_behaviour = Toggle::from(Some(beetswap::Behaviour::<64, SledBlockstore>::new(storage)));

        Self{
            kad,
            ping: ping::Behaviour::new(ping::Config::new()),
            identify: identify::Behaviour::new(identify::Config::new(
                config.protocol_version.clone(),
                config.keypair.public(),
            )),
            request_response: request_response::Behaviour::new(protocols, request_response::Config::default()),
            stream: stream::Behaviour::new(),
            rendezvous,
            mdns,
            upnp,
            autonat,
            dcutr,
            relay,
            gossipsub,
            bitswap: sd_behaviour,
        }
    }

    pub fn connection_closed(&mut self, peer_id: PeerId) {
        if let Some(kad) = self.kad.as_mut() {
            kad.remove_peer(&peer_id);
        }
    }

    pub fn put_record(&mut self, record: Record, quorum: Quorum, tx: oneshot::Sender<Result<()>>, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(kad) = self.kad.as_mut() {
            match kad.put_record(record, quorum) {
                Ok(id) => {
                    queries.insert(id.into(), QueryChannel::PutRecord(tx));
                }
                Err(err) => {
                    tx.send(Err(anyhow!("{:?}", err))).ok();
                }
            }
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }

    pub fn get_record(&mut self, key: RecordKey, quorum: Quorum, tx: oneshot::Sender<Result<Vec<PeerRecord>>>, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(kad) = self.kad.as_mut() {
            let quorum = match quorum {
                Quorum::One => 1,
                Quorum::Majority => K_VALUE.get() / 2 + 1,
                Quorum::All => K_VALUE.get(),
                Quorum::N(n) => n.get(),
            };
            let id = kad.get_record(key);
            queries.insert(id.into(), QueryChannel::GetRecord(quorum, vec![], tx));
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }

    pub fn is_kad_enabled(&self) -> bool {
        self.kad.is_enabled()
    }

    pub fn is_relay_enabled(&self) -> bool {
        self.relay.is_enabled()
    }

    pub fn is_rendezvous_client_enabled(&self) -> bool {
        self.rendezvous.is_enabled()
    }

    pub fn register_to_rendezvous(&mut self, rendezvous_peer: PeerId, namespace: Option<String>, ttl: Option<u64>, tx: oneshot::Sender<Result<()>>, _queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(client) = self.rendezvous.as_mut() {
            let namespace = namespace.unwrap_or_else(|| "default".to_string());

            // Register with the rendezvous server
            match client.register(
                rendezvous::Namespace::new(namespace).expect("Valid namespace"),
                rendezvous_peer,
                ttl,
            ) {
                Ok(()) => {
                    tracing::debug!("Successfully initiated rendezvous registration to peer {}", rendezvous_peer);
                    tx.send(Ok(())).ok();
                }
                Err(e) => {
                    tracing::error!("Failed to initiate rendezvous registration: {:?}", e);
                    tx.send(Err(anyhow!("Failed to register: {:?}", e))).ok();
                }
            }
        } else {
            tx.send(Err(anyhow!("Rendezvous client is not enabled"))).ok();
        }
    }

    pub fn discover_from_rendezvous(&mut self, rendezvous_peer: PeerId, namespace: Option<String>, _limit: Option<u64>, tx: oneshot::Sender<Result<Vec<PeerId>>>, _queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(client) = self.rendezvous.as_mut() {
            let namespace = namespace.map(|ns| rendezvous::Namespace::new(ns).expect("Valid namespace"));

            // Discover addressbook from the rendezvous server
            client.discover(namespace, None, None, rendezvous_peer);

            tracing::debug!("Successfully initiated rendezvous discovery from peer {}", rendezvous_peer);
            tx.send(Ok(vec![])).ok();
        } else {
            tx.send(Err(anyhow!("Rendezvous client is not enabled"))).ok();
        }
    }

    pub fn get(&mut self, cid: Cid, queries: &mut FnvHashMap<QueryId, QueryChannel>, tx: oneshot::Sender<Result<BytesBlock>>){
        if let Some(bitswap) = self.bitswap.as_mut() {
            let id = bitswap.get(&cid);
            queries.insert(id.into(), QueryChannel::Get(tx));
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }

    pub fn start_providing(&mut self, record_key: RecordKey, tx: oneshot::Sender<Result<()>>, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(kad) = self.kad.as_mut() {
            match kad.start_providing(record_key) {
                Ok(id) => {
                    queries.insert(id.into(), QueryChannel::StartProviding(tx));
                }
                Err(err) => {
                    tx.send(Err(anyhow!("{:?}", err))).ok();
                }
            }
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }

    pub fn get_providers(&mut self, record_key: RecordKey, tx: oneshot::Sender<Result<Vec<PeerId>>>, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
        if let Some(kad) = self.kad.as_mut() {
            let id = kad.get_providers(record_key);
            queries.insert(id.into(), QueryChannel::GetProviders(tx));
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }

    pub fn stop_providing(&mut self, record_key: RecordKey, tx: oneshot::Sender<Result<()>>) {
        if let Some(kad) = self.kad.as_mut() {
            kad.stop_providing(&record_key);
            tx.send(Ok(())).ok();
        } else {
            tx.send(Err(anyhow!("Kad is not enabled"))).ok();
        }
    }
}