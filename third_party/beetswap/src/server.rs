use std::collections::hash_map::Entry;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use asynchronous_codec::FramedWrite;
use blockstore::Blockstore;
use cid::CidGeneric;
use fnv::{FnvHashMap, FnvHashSet};
use futures_util::sink::SinkExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use libp2p_core::upgrade::ReadyUpgrade;
use libp2p_identity::PeerId;
use libp2p_swarm::{
    ConnectionHandlerEvent, ConnectionId, NotifyHandler, StreamProtocol, SubstreamProtocol, ToSwarm,
};
use smallvec::SmallVec;
use tracing::{debug, trace};

use crate::cid_prefix::CidPrefix;
use crate::incoming_stream::ServerMessage;
use crate::message::Codec;
use crate::proto::message::{
    mod_Message::Block as ProtoBlock, mod_Message::BlockPresence as ProtoBlockPresence,
    mod_Message::BlockPresenceType, mod_Message::Wantlist as ProtoWantlist, 
    mod_Message::mod_Wantlist::WantType, Message,
};
use crate::utils::{box_future, stream_protocol, BoxFuture};
use crate::{Event, Result, StreamRequester, ToBehaviourEvent, ToHandlerEvent};

type Sink = FramedWrite<libp2p_swarm::Stream, Codec>;
type BlockWithCid<const S: usize> = (CidGeneric<S>, Vec<u8>);

const MAX_WANTLIST_ENTRIES_PER_PEER: usize = 1024;

#[derive(Debug)]
pub(crate) struct ServerBehaviour<const S: usize, B>
where
    B: Blockstore,
{
    /// protocol, used to create connection handler
    protocol: StreamProtocol,
    /// blockstore to fetch blocks from
    store: Arc<B>,
    /// list of CIDs each connected peer is waiting for
    peers_wantlists: FnvHashMap<PeerId, PeerWantlist<S>>,
    /// list of peers that wait for particular CID (WantBlock requests)
    peers_waiting_for_cid: FnvHashMap<CidGeneric<S>, SmallVec<[Arc<PeerId>; 1]>>,
    /// tracks established connections for each peer
    peer_connections: FnvHashMap<PeerId, FnvHashSet<ConnectionId>>,
    /// list of peers that need BlockPresence response for CIDs (WantHave requests)
    peers_wanting_presence: FnvHashMap<CidGeneric<S>, SmallVec<[Arc<PeerId>; 1]>>,
    /// list of blocks received from blockstore or network, that connected peers may be waiting for
    outgoing_queue: VecDeque<BlockWithCid<S>>,
    /// list of events to be sent back to swarm when poll is called
    outgoing_event_queue: VecDeque<ToSwarm<Event, ToHandlerEvent>>,
    /// list of long running tasks, currently used to interact with the store
    tasks: FuturesUnordered<BoxFuture<'static, TaskResult<S>>>,
}

enum TaskResult<const S: usize> {
    Get(Arc<PeerId>, Vec<GetCidResult<S>>),
    GetPresence(Arc<PeerId>, Vec<GetCidResult<S>>),
}

struct GetCidResult<const S: usize> {
    cid: CidGeneric<S>,
    data: Result<Option<Vec<u8>>, blockstore::Error>,
}

#[derive(Debug, Default)]
struct PeerWantlist<const S: usize>(FnvHashSet<CidGeneric<S>>);

impl<const S: usize> PeerWantlist<S> {
    /// Updates peers wantlist according to received message. 
    /// Returns tuple: (want_blocks, want_haves, removals)
    fn process_wantlist(
        &mut self,
        wantlist: ProtoWantlist,
    ) -> (Vec<CidGeneric<S>>, Vec<CidGeneric<S>>, Vec<CidGeneric<S>>) {
        if wantlist.full {
            let mut want_blocks = Vec::new();
            let mut want_haves = Vec::new();
            
            for e in wantlist.entries {
                if e.cancel {
                    continue;
                }
                if let Ok(cid) = CidGeneric::try_from(e.block) {
                    if e.wantType == WantType::Block {
                        want_blocks.push(cid);
                    } else {
                        want_haves.push(cid);
                    }
                }
            }
            
            let wanted_cids: FnvHashSet<_> = want_blocks.iter().copied().collect();
            let (additions, removals) = self.wantlist_replace(wanted_cids);
            return (additions, want_haves, removals);
        }

        let (cancels, additions): (Vec<_>, Vec<_>) = wantlist.entries.into_iter()
            .filter_map(|e| CidGeneric::<S>::try_from(e.block).map(|cid| (e.cancel, cid, e.wantType)).ok())
            .partition(|(cancel, _, _)| *cancel);

        let mut removed = Vec::with_capacity(cancels.len());
        for (_, cid, _) in cancels {
            if self.0.remove(&cid) {
                removed.push(cid);
            }
        }

        let mut want_blocks = Vec::new();
        let mut want_haves = Vec::new();
        
        for (_, cid, want_type) in additions {
            if want_type == WantType::Block {
                if self.0.len() >= MAX_WANTLIST_ENTRIES_PER_PEER {
                    break;
                }
                if self.0.insert(cid) {
                    want_blocks.push(cid)
                }
            } else {
                want_haves.push(cid);
            }
        }

        (want_blocks, want_haves, removed)
    }

    fn wantlist_replace(
        &mut self,
        cids: FnvHashSet<CidGeneric<S>>,
    ) -> (Vec<CidGeneric<S>>, Vec<CidGeneric<S>>) {
        let additions = cids.difference(&self.0).copied().collect();
        let removals = self.0.difference(&cids).copied().collect();

        self.0 = cids;

        (additions, removals)
    }
}

impl<const S: usize, B> ServerBehaviour<S, B>
where
    B: Blockstore + 'static,
{
    pub(crate) fn new(store: Arc<B>, protocol_prefix: Option<&str>) -> Self {
        let protocol = stream_protocol(protocol_prefix, "/ipfs/bitswap/1.2.0")
            .expect("prefix checked by beetswap::BehaviourBuilder::protocol_prefix");

        ServerBehaviour {
            protocol,
            store,
            peers_wantlists: Default::default(),
            peers_waiting_for_cid: Default::default(),
            peer_connections: Default::default(),
            peers_wanting_presence: Default::default(),
            tasks: Default::default(),
            outgoing_queue: Default::default(),
            outgoing_event_queue: Default::default(),
        }
    }

    fn schedule_store_get(&mut self, peer: Arc<PeerId>, cids: Vec<CidGeneric<S>>) {
        let store = self.store.clone();

        self.tasks.push(box_future(async move {
            let result = get_multiple_cids_from_store(store, cids).await;
            TaskResult::Get(peer, result)
        }));
    }

    fn schedule_store_check_presence(&mut self, peer: Arc<PeerId>, cids: Vec<CidGeneric<S>>) {
        let store = self.store.clone();

        self.tasks.push(box_future(async move {
            let result = get_multiple_cids_from_store(store, cids).await;
            TaskResult::GetPresence(peer, result)
        }));
    }

    fn cancel_request(&mut self, peer: Arc<PeerId>, cid: CidGeneric<S>) {
        // remove peer from the waitlist for cid, in case we happen to get it later
        if let Entry::Occupied(mut entry) = self.peers_waiting_for_cid.entry(cid) {
            if entry.get().as_ref() == [peer.clone()] {
                entry.remove();
            } else {
                let peers = entry.get_mut();
                if let Some(index) = peers.iter().position(|p| *p == peer) {
                    peers.swap_remove(index);
                }
            }
        }

        if let Some(peer_state) = self.peers_wantlists.get_mut(&peer) {
            peer_state.0.remove(&cid);
        }
    }

    pub(crate) fn process_incoming_message(&mut self, peer: PeerId, msg: ServerMessage) {
        let Some(wantlist) = self.peers_wantlists.get_mut(&peer) else {
            return; // entry should have been created in `new_connection_handler`
        };

        let (want_blocks, want_haves, removals) = wantlist.process_wantlist(msg.wantlist);

        debug!(
            "updating local wantlist for {peer}: WantBlock={}, WantHave={}, removed={}",
            want_blocks.len(),
            want_haves.len(),
            removals.len()
        );

        let peer = Arc::new(peer);
        
        // Process WantBlock requests
        for cid in &want_blocks {
            let peers_for_cid = self.peers_waiting_for_cid.entry(*cid).or_default();
            if !peers_for_cid.iter().any(|p| **p == *peer) {
                peers_for_cid.push(peer.clone());
            }
        }
        if !want_blocks.is_empty() {
            self.schedule_store_get(peer.clone(), want_blocks);
        }
        
        // Process WantHave requests
        for cid in &want_haves {
            self.peers_wanting_presence.entry(*cid).or_default().push(peer.clone());
        }
        if !want_haves.is_empty() {
            self.schedule_store_check_presence(peer.clone(), want_haves);
        }

        for cid in removals {
            self.cancel_request(peer.clone(), cid);
        }
    }

    pub(crate) fn new_blocks_available(&mut self, blocks: Vec<BlockWithCid<S>>) {
        for (cid, data) in blocks {
            if self.peers_waiting_for_cid.contains_key(&cid) {
                debug!("New block available: CID={}, size={:.2}MB", cid, data.len() as f64 / (1024.0 * 1024.0));
                self.outgoing_queue.push_back((cid, data));
            }
        }
    }

    pub(crate) fn new_connection_handler(&mut self, peer: PeerId, connection_id: ConnectionId) -> ServerConnectionHandler<S> {
        self.peers_wantlists.entry(peer).or_default();
        self.peer_connections.entry(peer).or_default().insert(connection_id);

        ServerConnectionHandler {
            protocol: self.protocol.clone(),
            sink: Default::default(),
            pending_outgoing_messages: None,
            pending_outgoing_presences: None,
        }
    }

    pub(crate) fn on_connection_closed(&mut self, peer: PeerId, connection_id: ConnectionId) {
        if let Entry::Occupied(mut entry) = self.peer_connections.entry(peer) {
            entry.get_mut().remove(&connection_id);

            if entry.get().is_empty() {
                entry.remove();

                if let Some(wantlist) = self.peers_wantlists.remove(&peer) {
                    for cid in wantlist.0 {
                        if let Entry::Occupied(mut cid_entry) = self.peers_waiting_for_cid.entry(cid) {
                            let peers = cid_entry.get_mut();
                            peers.retain(|p| **p != peer);
                            if peers.is_empty() {
                                cid_entry.remove();
                            }
                        }
                    }
                }
            }
        }
    }

    fn update_handlers(&mut self) -> bool {
        if self.outgoing_queue.is_empty() {
            return false;
        }

        let mut blocks_ready_for_peer =
            FnvHashMap::<Arc<PeerId>, Vec<(Vec<u8>, Vec<u8>)>>::default();

        while let Some((cid, data)) = self.outgoing_queue.pop_front() {
            let Some(peers_waiting) = self.peers_waiting_for_cid.remove(&cid) else {
                continue;
            };

            trace!("Sending block: CID={}, size={:.2}MB, peers={}", 
                  cid, data.len() as f64 / (1024.0 * 1024.0), peers_waiting.len());

            for peer in peers_waiting {
                if self.peer_connections.contains_key(&peer) {
                    blocks_ready_for_peer
                        .entry(peer)
                        .or_default()
                        .push((CidPrefix::from_cid(&cid).to_bytes(), data.clone()))
                }
            }
        }

        if blocks_ready_for_peer.is_empty() {
            return false;
        }

        trace!(
            "sending response to {} peer(s)",
            blocks_ready_for_peer.len()
        );
        for (peer, blocks) in blocks_ready_for_peer {
            self.outgoing_event_queue.push_back(ToSwarm::NotifyHandler {
                peer_id: *peer,
                handler: NotifyHandler::Any,
                event: ToHandlerEvent::QueueOutgoingMessages(blocks),
            })
        }

        true
    }

    fn process_store_get_results(&mut self, peer: Arc<PeerId>, results: Vec<GetCidResult<S>>) {
        for result in results {
            let cid = result.cid;
            match result.data {
                Ok(None) => {
                    // requested CID isn't present locally. If we happen to get it, we'll
                    // forward it to the peer later
                    debug!("Cid {cid} not in blockstore for {peer}");
                }
                Ok(Some(data)) => {
                    trace!("Block ready: CID={}, size={:.2}MB, to={}", cid, data.len() as f64 / (1024.0 * 1024.0), peer);
                    self.outgoing_queue.push_back((cid, data));
                }
                Err(e) => {
                    debug!("Fetching {cid} from blockstore failed: {e}");
                }
            }
        }
    }

    fn process_store_presence_results(&mut self, peer: Arc<PeerId>, results: Vec<GetCidResult<S>>) {
        let mut presences = Vec::new();
        
        for result in results {
            let cid = result.cid;
            let presence_type = match result.data {
                Ok(None) => BlockPresenceType::DontHave,
                Ok(Some(_)) => BlockPresenceType::Have,
                Err(_) => BlockPresenceType::DontHave,
            };
            presences.push(ProtoBlockPresence {
                cid: cid.to_bytes(),
                type_pb: presence_type,
            });
            self.peers_wanting_presence.remove(&cid);
        }
        
        if !presences.is_empty() {
            self.outgoing_event_queue.push_back(ToSwarm::NotifyHandler {
                peer_id: *peer,
                handler: NotifyHandler::Any,
                event: ToHandlerEvent::QueueOutgoingPresences(presences),
            });
        }
    }

    pub(crate) fn poll(&mut self, cx: &mut Context) -> Poll<ToSwarm<Event, ToHandlerEvent>> {
        loop {
            if let Some(ev) = self.outgoing_event_queue.pop_front() {
                return Poll::Ready(ev);
            }

            if let Poll::Ready(Some(task_result)) = self.tasks.poll_next_unpin(cx) {
                match task_result {
                    TaskResult::Get(peer, results) => self.process_store_get_results(peer, results),
                    TaskResult::GetPresence(peer, results) => self.process_store_presence_results(peer, results),
                }
                continue;
            }

            if self.update_handlers() {
                continue;
            }

            return Poll::Pending;
        }
    }
}

pub(crate) struct ServerConnectionHandler<const S: usize> {
    protocol: StreamProtocol,
    sink: SinkState,
    pending_outgoing_messages: Option<Vec<ProtoBlock>>,
    pending_outgoing_presences: Option<Vec<ProtoBlockPresence>>,
}

impl<const S: usize> fmt::Debug for ServerConnectionHandler<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerConnectionHandler")
    }
}

#[derive(Default)]
enum SinkState {
    #[default]
    None,
    Requested,
    Ready(Sink),
}

impl<const S: usize> ServerConnectionHandler<S> {
    pub(crate) fn set_stream(&mut self, stream: libp2p_swarm::Stream) {
        // Convert `AsyncWrite` stream to `Sink`
        self.sink = SinkState::Ready(FramedWrite::new(stream, Codec));
    }

    pub(crate) fn queue_messages(&mut self, messages: Vec<(Vec<u8>, Vec<u8>)>) {
        let block_list = messages
            .into_iter()
            .map(|(prefix, data)| ProtoBlock { prefix, data })
            .collect::<Vec<_>>();

        self.pending_outgoing_messages
            .get_or_insert_with(|| Vec::with_capacity(block_list.len()))
            .extend(block_list);
    }

    pub(crate) fn queue_presences(&mut self, presences: Vec<ProtoBlockPresence>) {
        self.pending_outgoing_presences
            .get_or_insert_with(|| Vec::with_capacity(presences.len()))
            .extend(presences);
    }

    fn open_new_substream(
        &mut self,
    ) -> Poll<
        ConnectionHandlerEvent<ReadyUpgrade<StreamProtocol>, StreamRequester, ToBehaviourEvent<S>>,
    > {
        self.sink = SinkState::Requested;

        Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
            protocol: SubstreamProtocol::new(
                ReadyUpgrade::new(self.protocol.clone()),
                StreamRequester::Server,
            ),
        })
    }

    fn poll_outgoing(
        &mut self,
        cx: &mut Context,
    ) -> Poll<
        ConnectionHandlerEvent<ReadyUpgrade<StreamProtocol>, StreamRequester, ToBehaviourEvent<S>>,
    > {
        loop {
            let has_messages = self.pending_outgoing_messages.is_some();
            let has_presences = self.pending_outgoing_presences.is_some();
            
            match &mut self.sink {
                SinkState::Requested => return Poll::Pending,
                SinkState::None => {
                    if has_messages || has_presences {
                        return self.open_new_substream();
                    }
                    return Poll::Pending;
                }
                SinkState::Ready(sink) => {
                    if ready!(sink.poll_flush_unpin(cx)).is_err() {
                        self.close_sink_on_error("poll_flush_unpin");
                        continue;
                    }

                    if !has_messages && !has_presences {
                        return Poll::Pending;
                    }

                    let messages = self.pending_outgoing_messages.take().unwrap_or_default();
                    let presences = self.pending_outgoing_presences.take().unwrap_or_default();
                    
                    let message = Message {
                        payload: messages,
                        blockPresences: presences,
                        ..Message::default()
                    };

                    if sink.start_send_unpin(&message).is_err() {
                        self.close_sink_on_error("start_send_unpin");
                        continue;
                    }
                }
            }
        }
    }

    fn close_sink_on_error(&mut self, location: &str) {
        debug!("sink operation failed, closing: {location}");
        self.sink = SinkState::None;
    }

    pub(crate) fn poll(
        &mut self,
        cx: &mut Context,
    ) -> Poll<
        ConnectionHandlerEvent<ReadyUpgrade<StreamProtocol>, StreamRequester, ToBehaviourEvent<S>>,
    > {
        self.poll_outgoing(cx)
    }
}

async fn get_multiple_cids_from_store<const S: usize, B: Blockstore>(
    store: Arc<B>,
    cids: Vec<CidGeneric<S>>,
) -> Vec<GetCidResult<S>> {
    let mut results = Vec::with_capacity(cids.len());

    for cid in cids {
        let data = store.get(&cid).await;
        results.push(GetCidResult { cid, data });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::message::mod_Message::mod_Wantlist::{Entry, WantType};
    use crate::test_utils::{cid_of_data, poll_fn_once};
    use blockstore::InMemoryBlockstore;
    use cid::Cid;
    use multihash::Multihash;
    use std::future::poll_fn;

    #[test]
    fn wantlist_replace() {
        let initial_cids =
            (0..500_i32).map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()));
        let replacing_cids = (600..1200_i32)
            .map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()));

        let mut wantlist = PeerWantlist::<64>::default();
        let (initial_events, _) = wantlist.wantlist_replace(initial_cids.clone().collect());
        assert_eq!(initial_cids.len(), initial_events.len());
        for cid in initial_cids.clone() {
            assert!(initial_events.contains(&cid));
        }

        let (added, removed) = wantlist.wantlist_replace(replacing_cids.clone().collect());
        assert_eq!(added.len(), replacing_cids.len());
        assert_eq!(removed.len(), initial_cids.len());
        for cid in replacing_cids {
            assert!(added.contains(&cid));
        }
        for cid in initial_cids {
            assert!(removed.contains(&cid));
        }
    }

    #[test]
    fn wantlist_replace_overlaping() {
        let initial_cids = (0..600_i32)
            .map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()))
            .collect();
        let replacing_cids = (500..1000_i32)
            .map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()))
            .collect();

        let mut wantlist = PeerWantlist::<64>::default();
        wantlist.wantlist_replace(initial_cids);
        let (added, removed) = wantlist.wantlist_replace(replacing_cids);

        let removed_cids: Vec<_> = (0..500_i32)
            .map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()))
            .collect();
        let added_cids: Vec<_> = (600..1000_i32)
            .map(|v| Cid::new_v1(24, Multihash::wrap(42, &v.to_le_bytes()).unwrap()))
            .collect();
        assert_eq!(added.len(), added_cids.len());
        assert_eq!(removed.len(), removed_cids.len());
        for cid in added_cids {
            assert!(added.contains(&cid));
        }
        for cid in removed_cids {
            assert!(removed.contains(&cid));
        }
    }

    #[tokio::test]
    async fn client_requests_known_cid() {
        let data = "1";
        let cid = cid_of_data(data.as_bytes());
        let message = ServerMessage {
            wantlist: ProtoWantlist {
                full: true,
                entries: vec![Entry {
                    cancel: false,
                    priority: 0,
                    sendDontHave: false,
                    block: cid.into(),
                    wantType: WantType::Block,
                }],
            },
        };
        let peer = PeerId::random();
        let connection_id = libp2p_swarm::ConnectionId::new_unchecked(0);

        let mut server = new_server().await;
        let _client_connection = server.new_connection_handler(peer, connection_id);
        server.process_incoming_message(peer, message);

        let ev = poll_fn(|cx| server.poll(cx)).await;

        let ToSwarm::NotifyHandler { peer_id, event, .. } = ev else {
            panic!("Unexpected event {ev:?}");
        };
        assert_eq!(peer_id, peer);
        let ToHandlerEvent::QueueOutgoingMessages(msgs) = event else {
            panic!("Invalid handler message type ");
        };
        assert_eq!(
            msgs,
            vec![(
                CidPrefix::from_cid(&cid).to_bytes(),
                data.as_bytes().to_vec()
            )]
        );
    }

    #[tokio::test]
    async fn client_requests_unknown_cid() {
        let data = "unknown";
        let cid = cid_of_data(data.as_bytes());
        let message = ServerMessage {
            wantlist: ProtoWantlist {
                full: true,
                entries: vec![Entry {
                    cancel: false,
                    priority: 0,
                    sendDontHave: false,
                    block: cid.into(),
                    wantType: WantType::Block,
                }],
            },
        };
        let peer = PeerId::random();
        let connection_id = libp2p_swarm::ConnectionId::new_unchecked(0);

        let mut server = new_server().await;
        let _client_connection = server.new_connection_handler(peer, connection_id);
        server.process_incoming_message(peer, message);

        // no data yet
        assert!(poll_fn_once(|cx| server.poll(cx)).await.is_none());

        server.new_blocks_available(vec![(cid, data.into())]);

        let ev = poll_fn(|cx| server.poll(cx)).await;

        let ToSwarm::NotifyHandler { peer_id, event, .. } = ev else {
            panic!("Unexpected event {ev:?}");
        };
        assert_eq!(peer_id, peer);
        let ToHandlerEvent::QueueOutgoingMessages(msgs) = event else {
            panic!("Invalid handler message type ");
        };
        assert_eq!(
            msgs,
            vec![(
                CidPrefix::from_cid(&cid).to_bytes(),
                data.as_bytes().to_vec()
            )]
        );
    }

    async fn new_server() -> ServerBehaviour<64, InMemoryBlockstore<64>> {
        let store = Arc::new(InMemoryBlockstore::<64>::new());
        for i in 0..16 {
            let data = format!("{i}").into_bytes();
            let cid = cid_of_data(&data);
            store.put_keyed(&cid, &data).await.unwrap();
        }
        ServerBehaviour::<64, _>::new(store, None)
    }
}
