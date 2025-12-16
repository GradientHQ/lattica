use std::future::Future;
use std::sync::Arc;

use beetswap::{Error, Event, QueryId};
use blockstore::InMemoryBlockstore;
use cid::CidGeneric;
use fnv::FnvHashMap;
use futures_util::future::FutureExt;
use futures_util::stream::StreamExt;
use libp2p::swarm::{DialError, SwarmEvent};
use libp2p::{noise, tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder};
use multihash_codetable::{Code, MultihashDigest};
use tokio::select;
use tokio::sync::{mpsc, oneshot};

const RAW_CODEC: u64 = 0x55;
const CID_SIZE: usize = 64;
type Cid = CidGeneric<CID_SIZE>;

pub fn cid(bytes: &[u8]) -> Cid {
    let hash = Code::Sha2_256.digest(bytes);
    Cid::new_v1(RAW_CODEC, hash)
}

type BitswapSwarm = Swarm<beetswap::Behaviour<CID_SIZE, InMemoryBlockstore<CID_SIZE>>>;

pub struct TestBitswapNode {
    cmd: mpsc::UnboundedSender<NodeCommand>,
    pub addr: Multiaddr,
    pub peer_id: PeerId,
}

impl TestBitswapNode {
    fn cmd(&mut self, cmd: NodeCommand) {
        self.cmd.send(cmd).unwrap()
    }

    pub fn connect(
        &mut self,
        node: &TestBitswapNode,
    ) -> impl Future<Output = Result<(), DialError>> {
        let (notify_connected, response) = oneshot::channel();

        self.cmd(NodeCommand::Connect {
            addr: node.addr.clone(),
            peer_id: node.peer_id,
            notify_connected,
        });

        // unwrap oneshot closed channel error
        response.map(|f| f.unwrap()).boxed()
    }

    pub fn request_cid(&mut self, cid: Cid) -> impl Future<Output = Result<Vec<u8>, Error>> {
        let (respond_to, response) = oneshot::channel();
        self.cmd(NodeCommand::WaitForCid { cid, respond_to });

        // unwrap oneshot closed channel error
        response.map(|f| f.unwrap()).boxed()
    }
}

pub enum NodeCommand {
    Connect {
        addr: Multiaddr,
        peer_id: PeerId,
        notify_connected: oneshot::Sender<Result<(), DialError>>,
    },
    WaitForCid {
        cid: Cid,
        respond_to: oneshot::Sender<Result<Vec<u8>, Error>>,
    },
}

struct TestBitswapWorker {
    swarm: BitswapSwarm,
    cmd: mpsc::UnboundedReceiver<NodeCommand>,
    queried_cids: FnvHashMap<QueryId, oneshot::Sender<Result<Vec<u8>, Error>>>,
    dials_requested: FnvHashMap<PeerId, oneshot::Sender<Result<(), DialError>>>,
}

impl TestBitswapWorker {
    async fn run(&mut self) {
        loop {
            select! {
                ev = self.swarm.select_next_some() => {
                    self.on_event(ev);
                },
                cmd = self.cmd.recv() => if let Some(cmd) = cmd {
                    self.on_cmd(cmd).await;
                } else {
                    break;
                }
            }
        }
    }

    fn on_event(&mut self, ev: SwarmEvent<Event>) {
        match ev {
            SwarmEvent::Behaviour(bev) => match bev {
                Event::GetQueryResponse { query_id, data } => {
                    let tx = self.queried_cids.remove(&query_id).unwrap();
                    tx.send(Ok(data)).unwrap();
                }
                Event::GetQueryError { query_id, error } => {
                    let tx = self.queried_cids.remove(&query_id).unwrap();
                    tx.send(Err(error)).unwrap();
                }
            },
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                if let Some(tx) = self.dials_requested.remove(&peer_id) {
                    // receiver might have been dropped already if caller doesn't care about
                    // waiting for connecion to be established
                    let _ = tx.send(Ok(()));
                }
            }
            _ => (),
        }
    }

    async fn on_cmd(&mut self, cmd: NodeCommand) {
        match cmd {
            NodeCommand::Connect {
                addr,
                peer_id,
                notify_connected,
            } => {
                if let e @ Err(_) = self.swarm.dial(addr.clone()) {
                    notify_connected.send(e).unwrap();
                } else {
                    self.dials_requested.insert(peer_id, notify_connected);
                }
            }
            NodeCommand::WaitForCid { cid, respond_to } => {
                let query_id = self.swarm.behaviour_mut().get(&cid);
                self.queried_cids.insert(query_id, respond_to);
            }
        }
    }
}

pub async fn spawn_node(store: Option<InMemoryBlockstore<CID_SIZE>>) -> TestBitswapNode {
    let blockstore = Arc::new(store.unwrap_or_default());

    let mut swarm = SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_key| beetswap::Behaviour::<CID_SIZE, _>::new(blockstore))
        .unwrap()
        .build();

    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();

    let mut events = Vec::new();
    let addr = loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => break address,
            ev => events.push(ev),
        }
    };
    let peer_id = swarm.local_peer_id().to_owned();

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let mut worker = TestBitswapWorker {
        swarm,
        cmd: cmd_rx,
        queried_cids: FnvHashMap::default(),
        dials_requested: FnvHashMap::default(),
    };

    let _handle = tokio::spawn(async move { worker.run().await });

    TestBitswapNode {
        cmd: cmd_tx,
        addr,
        peer_id,
    }
}
