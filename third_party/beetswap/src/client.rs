use std::collections::{hash_map, VecDeque};
use std::mem::take;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::Duration;
use std::{fmt, mem};

use asynchronous_codec::FramedWrite;
use blockstore::Blockstore;
use cid::CidGeneric;
use fnv::{FnvHashMap, FnvHashSet};
use futures_timer::Delay;
use futures_util::future::{AbortHandle, Abortable, FutureExt};
use futures_util::sink::SinkExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use libp2p_core::upgrade::ReadyUpgrade;
use libp2p_identity::PeerId;
use libp2p_swarm::{
    ConnectionHandlerEvent, ConnectionId, NotifyHandler, StreamProtocol, SubstreamProtocol, ToSwarm,
};
use smallvec::SmallVec;
use tracing::{debug, info};
use web_time::Instant;

use crate::incoming_stream::ClientMessage;
use crate::message::Codec;
use crate::peer_selection::{GlobalStats, PeerDetail, PeerMetrics, PeerSelectionConfig, PeerSelector};
use crate::proto::message::mod_Message::{BlockPresenceType, Wantlist as ProtoWantlist};
use crate::proto::message::Message;
use crate::utils::{box_future, convert_cid, stream_protocol, BoxFuture};
use crate::wantlist::{Wantlist, WantlistState};
use crate::{ConnHandlerEvent, StreamRequester};
use crate::{Error, Event, Result, ToBehaviourEvent, ToHandlerEvent};

const SEND_FULL_INTERVAL: Duration = Duration::from_secs(30);
const RECEIVE_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const START_SENDING_TIMEOUT: Duration = Duration::from_secs(5);

/// ID of an ongoing query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryId(u64);

#[derive(Debug)]
pub struct ClientConfig {
    pub set_send_dont_have: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            set_send_dont_have: true,
        }
    }
}

enum TaskResult<const S: usize> {
    Get(
        QueryId,
        CidGeneric<S>,
        Result<Option<Vec<u8>>, blockstore::Error>,
    ),
    Set(Result<Vec<(CidGeneric<S>, Vec<u8>)>, blockstore::Error>),
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct ClientBehaviour<const S: usize, B>
where
    B: Blockstore,
{
    store: Arc<B>,
    protocol: StreamProtocol,
    queue: VecDeque<ToSwarm<Event, ToHandlerEvent>>,
    wantlist: Wantlist<S>,
    peers: FnvHashMap<PeerId, PeerState<S>>,
    cid_to_queries: FnvHashMap<CidGeneric<S>, SmallVec<[QueryId; 1]>>,
    tasks: FuturesUnordered<BoxFuture<'static, TaskResult<S>>>,
    query_abort_handle: FnvHashMap<QueryId, AbortHandle>,
    next_query_id: u64,
    send_full_timer: Delay,
    new_blocks: Vec<(CidGeneric<S>, Vec<u8>)>,
    /// Peer selection config
    peer_selection_config: PeerSelectionConfig,
    /// Global stats
    global_stats: GlobalStats,
    /// Track the first Have response time for each CID (for wait window)
    cid_first_have_time: FnvHashMap<CidGeneric<S>, Instant>,
}

#[derive(Debug)]
struct PeerState<const S: usize> {
    /// Keeps track of established connections.
    ///
    /// A connection is removed from this list if one of the following happens:
    ///
    /// * Connection closure is triggered.
    /// * `ClientConnectionHandler` did not receive the `SendWantlist` request. In other
    ///   words the `RECEIVE_REQUEST_TIMEOUT` is triggered.
    /// * `ClientConnectionHandler` failed to allocate a communication channel with the
    ///   other peer. In other words the `START_SENDING_TIMEOUT` is triggered.
    /// * Communication channel with the peer was closed unexpectedly. This can happen for example when
    ///   the TCP conection is closed.
    established_connections: FnvHashSet<ConnectionId>,
    sending_state: SendingState,
    wantlist: WantlistState<S>,
    send_full: bool,
    /// Peer performance metrics
    metrics: PeerMetrics,
}

/// Sending state of the `ClientConnectionHandler`.
///
/// This exists in two different places:
///
/// * `PeerState` in `ClientBehaviour`
/// * `ClientConnectionHandler`
///
/// The changes are synchronized via events. See the following on
/// why this designed was chosen:
///
/// * https://github.com/eigerco/lumina/issues/257
/// * https://github.com/eigerco/beetswap/pull/36
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SendingState {
    /// All `ClientConnectionHandler` are ready to send new messages.
    ///
    /// NOTE: Each peer can have multiple `ClientConnectionHandler`.
    Ready,
    /// `ClientBehaviour` requested to send a message via `ClientConnectionHandler` with `ConnectionId`.
    Requested(Instant, ConnectionId),
    /// `ClientConnectionHandler` with `ConnectionId` received the request.
    RequestReceived(Instant, ConnectionId),
    /// `ClientConnectionHandler` with `ConnectionId` started sending the message.
    Sending(Instant, ConnectionId),
    /// `ClientConnectionHandler` with `ConnectionId` failed to send the message.
    Failed(ConnectionId),
}

impl<const S: usize, B> ClientBehaviour<S, B>
where
    B: Blockstore + 'static,
{
    pub(crate) fn new(config: ClientConfig, store: Arc<B>, protocol_prefix: Option<&str>) -> Self {
        let protocol = stream_protocol(protocol_prefix, "/ipfs/bitswap/1.2.0")
            .expect("prefix checked by beetswap::BehaviourBuilder::protocol_prefix");
        let set_send_dont_have = config.set_send_dont_have;

        ClientBehaviour {
            store,
            protocol,
            queue: VecDeque::new(),
            wantlist: Wantlist::new(set_send_dont_have),
            peers: FnvHashMap::default(),
            cid_to_queries: FnvHashMap::default(),
            tasks: FuturesUnordered::new(),
            query_abort_handle: FnvHashMap::default(),
            next_query_id: 0,
            send_full_timer: Delay::new(SEND_FULL_INTERVAL),
            new_blocks: Vec::new(),
            peer_selection_config: PeerSelectionConfig::default(),
            global_stats: GlobalStats::default(),
            cid_first_have_time: FnvHashMap::default(),
        }
    }

    pub(crate) fn new_connection_handler(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> ClientConnectionHandler<S> {
        let peer = self.peers.entry(peer_id).or_insert_with(|| PeerState {
            established_connections: FnvHashSet::default(),
            sending_state: SendingState::Ready,
            wantlist: WantlistState::new(),
            send_full: true,
            metrics: PeerMetrics::default(),
        });

        peer.established_connections.insert(connection_id);

        ClientConnectionHandler {
            peer_id,
            connection_id,
            queue: VecDeque::new(),
            protocol: self.protocol.clone(),
            msg: None,
            sink_state: SinkState::None,
            sending_state: SendingState::Ready,
            closing: false,
            halted: false,
            start_sending_timeout: None,
        }
    }

    pub(crate) fn on_connection_closed(&mut self, peer: PeerId, connection_id: ConnectionId) {
        if let hash_map::Entry::Occupied(mut entry) = self.peers.entry(peer) {
            entry
                .get_mut()
                .established_connections
                .remove(&connection_id);

            if entry.get().established_connections.is_empty() {
                entry.remove();
            }
        }
    }

    fn next_query_id(&mut self) -> QueryId {
        let id = QueryId(self.next_query_id);
        self.next_query_id += 1;
        id
    }

    /// Schedule a `Blockstore::get` for the specified cid
    fn schedule_store_get(&mut self, query_id: QueryId, cid: CidGeneric<S>) {
        let store = self.store.clone();
        let (handle, reg) = AbortHandle::new_pair();

        // Try to asynchronously get the CID from the store..
        self.tasks.push(box_future(async move {
            match Abortable::new(store.get(&cid), reg).await {
                // ..And continue the procedure in `poll`. Missing CID will be handled there.
                Ok(res) => TaskResult::Get(query_id, cid, res),
                Err(_) => TaskResult::Cancelled,
            }
        }));

        self.query_abort_handle.insert(query_id, handle);
    }

    /// Schedule a `Blockstore::put_many_keyed` for the specified blocks
    fn schedule_store_put_many(&mut self, blocks: Vec<(CidGeneric<S>, Vec<u8>)>) {
        let store = self.store.clone();

        self.tasks.push(box_future(async move {
            let res = store
                .put_many_keyed(blocks.clone().into_iter())
                .await
                .map(|_| blocks);
            TaskResult::Set(res)
        }));
    }

    pub(crate) fn get<const CS: usize>(&mut self, cid: &CidGeneric<CS>) -> QueryId {
        let query_id = self.next_query_id();

        match convert_cid(cid) {
            // Schedule an asynchronous get from the blockstore. The result will be provided
            // from `poll` and if CID is missing `poll` will query the network.
            Some(cid) => self.schedule_store_get(query_id, cid),
            // If CID conversion fails, an event with the error will be given to
            // the requestor on the next `poll`.
            None => {
                self.queue
                    .push_back(ToSwarm::GenerateEvent(Event::GetQueryError {
                        query_id,
                        error: Error::InvalidMultihashSize,
                    }));
            }
        }

        query_id
    }

    pub(crate) fn cancel(&mut self, query_id: QueryId) {
        if let Some(abort_handle) = self.query_abort_handle.remove(&query_id) {
            abort_handle.abort();
        }

        for (cid, queries) in self.cid_to_queries.iter_mut() {
            if let Some(pos) = queries.iter().position(|id| *id == query_id) {
                queries.swap_remove(pos);

                // If CID doesn't have any other queries requesting it, remove it completely.
                // Cancel message will be send to the servers from `poll`.
                if queries.is_empty() {
                    // Cancelling message will be generated from `poll` method
                    let cid = cid.to_owned();
                    self.cid_to_queries.remove(&cid);
                    self.wantlist.remove(&cid);
                    self.cid_first_have_time.remove(&cid);
                }

                break;
            }
        }
    }

    pub(crate) fn process_incoming_message(&mut self, peer: PeerId, msg: ClientMessage<S>) {
        let Some(peer_state) = self.peers.get_mut(&peer) else {
            return;
        };

        let mut new_blocks = Vec::new();

        // Update presence and track first Have time for wait window
        for (cid, block_presence) in msg.block_presences {
            match block_presence {
                BlockPresenceType::Have => {
                    peer_state.wantlist.got_have(&cid);
                    // Record first Have time for this CID (for wait window mechanism)
                    self.cid_first_have_time.entry(cid).or_insert_with(Instant::now);
                }
                BlockPresenceType::DontHave => peer_state.wantlist.got_dont_have(&cid),
            }
        }

        // TODO: If someone sends a huge message, the executor will block! We need to
        // truncate the data, maybe even in the `message::Codec` level
        for (cid, block) in msg.blocks {
            let block_size = block.len();
            let is_first_response = self.wantlist.cids.contains(&cid);

            // Get elapsed time from peer's request sent time, fallback to historical RTT
            let elapsed_ms = peer_state.wantlist.get_request_sent_time(&cid)
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(100);

            // Record peer metrics
            peer_state.metrics.record_success(block_size, elapsed_ms);

            // Update global stats only for first response
            if is_first_response {
                self.global_stats.successful_requests += 1;
                self.global_stats.total_bytes_received += block_size as u64;
            }

            let speed_mbps = if elapsed_ms > 0 {
                (block_size as f64 / elapsed_ms as f64) * 1000.0 / (1024.0 * 1024.0)
            } else {
                0.0
            };

            tracing::info!(
                "{} CID {} from {} | {:.2} MB | {} ms | {:.2} MB/s | score: {:.2}",
                if is_first_response { "recv" } else { "dup" },
                cid,
                peer,
                block_size as f64 / (1024.0 * 1024.0),
                elapsed_ms,
                speed_mbps,
                peer_state.metrics.calculate_score()
            );
            
            if !self.wantlist.remove(&cid) {
                debug_assert!(!self.cid_to_queries.contains_key(&cid));
                continue;
            }

            // Clean up wait window tracking
            self.cid_first_have_time.remove(&cid);

            peer_state.wantlist.got_block(&cid);
            new_blocks.push((cid, block.clone()));

            // Inform the upper layer for the result
            if let Some(queries) = self.cid_to_queries.remove(&cid) {
                for query_id in queries {
                    self.queue
                        .push_back(ToSwarm::GenerateEvent(Event::GetQueryResponse {
                            query_id,
                            peer_id: Some(peer),
                            data: block.clone(),
                        }));
                }
            }
        }

        // Store them in blockstore
        if !new_blocks.is_empty() {
            self.schedule_store_put_many(new_blocks);
        }
    }

    pub(crate) fn sending_state_changed(&mut self, peer_id: PeerId, state: SendingState) {
        if let Some(peer) = self.peers.get_mut(&peer_id) {
            peer.sending_state = state;
        }
    }

    fn update_handlers(&mut self) -> bool {
        let mut handler_updated = false;
        let mut peers_without_connection = SmallVec::<[PeerId; 8]>::new();

        // Smart peer selection: identify candidates and count already-sent WantBlock requests
        let mut cid_to_candidates: FnvHashMap<CidGeneric<S>, Vec<PeerId>> = FnvHashMap::default();
        let mut cid_already_sent_count: FnvHashMap<CidGeneric<S>, usize> = FnvHashMap::default();
        
        for cid in &self.wantlist.cids {
            let already_sent: usize = self.peers.iter()
                .filter(|(_, state)| state.wantlist.has_sent_want_block(cid))
                .count();
            cid_already_sent_count.insert(*cid, already_sent);
            
            let candidates: Vec<PeerId> = self.peers.iter()
                .filter(|(_, state)| state.wantlist.has_received_have(cid))
                .map(|(peer_id, _)| *peer_id)
                .collect();
            
            if !candidates.is_empty() {
                cid_to_candidates.insert(*cid, candidates);
            }
        }

        // Select optimal peers for each CID (with wait window mechanism)
        let mut selected_peers_for_cid: FnvHashMap<CidGeneric<S>, Vec<PeerId>> = FnvHashMap::default();
        let total_peers = self.peers.len();
        let wait_window = Duration::from_millis(self.peer_selection_config.have_wait_window_ms);
        let min_candidate_ratio = self.peer_selection_config.min_candidate_ratio;
        
        for (cid, candidate_peers) in &cid_to_candidates {
            let already_sent = *cid_already_sent_count.get(cid).unwrap_or(&0);
            let top_n = self.peer_selection_config.top_n;
            
            if already_sent >= top_n {
                debug!("CID {} - already sent {} WantBlock, skipping (top_n={})", cid, already_sent, top_n);
                continue;
            }
            
            // Wait window check: ensure we have enough candidates before selecting
            let should_select = if let Some(first_have_time) = self.cid_first_have_time.get(cid) {
                let elapsed = first_have_time.elapsed();
                let min_candidates = ((total_peers as f64) * min_candidate_ratio).ceil() as usize;
                
                // Start selection if:
                // 1. Wait window has elapsed, OR
                // 2. We have enough candidates (>= min_candidate_ratio), OR
                // 3. We have enough candidates to fill remaining slots
                let window_elapsed = elapsed >= wait_window;
                let enough_candidates = candidate_peers.len() >= min_candidates;
                let can_fill_slots = candidate_peers.len() >= (top_n - already_sent);
                
                if !window_elapsed && !enough_candidates && !can_fill_slots {
                    // Use trace level to reduce log spam (this is called frequently during polling)
                    tracing::trace!(
                        "CID {} - waiting for more candidates: elapsed={:?}, candidates={}, min={}, window={:?}",
                        cid, elapsed, candidate_peers.len(), min_candidates, wait_window
                    );
                    false
                } else {
                    tracing::info!(
                        "CID {} - start selection: elapsed={:?}, candidates={}, candidate_peers={:?}, min={}, window={:?}",
                        cid, elapsed, candidate_peers.len(), candidate_peers.iter().map(|p| p.to_string()).collect::<Vec<String>>(), min_candidates, wait_window
                    );
                    true
                }
            } else {
                // No first Have time recorded yet (shouldn't happen, but be safe)
                true
            };
            
            if !should_select {
                continue;
            }
            
            let need_to_select = top_n - already_sent;
            
            let candidates_with_metrics: FnvHashMap<PeerId, &PeerMetrics> = candidate_peers
                .iter()
                .filter_map(|peer_id| {
                    self.peers.get(peer_id).map(|state| (*peer_id, &state.metrics))
                })
                .collect();

            if candidates_with_metrics.is_empty() {
                continue;
            }

            let mut temp_config = self.peer_selection_config.clone();
            temp_config.top_n = need_to_select;
            let selected = PeerSelector::select_top_peers(&candidates_with_metrics, &temp_config);

            debug!(
                "CID {} - sent: {}, candidates: {}, selected: {} (top_n={}, waited: {:?})",
                cid, already_sent, candidates_with_metrics.len(), selected.len(), top_n,
                self.cid_first_have_time.get(cid).map(|t| t.elapsed())
            );

            selected_peers_for_cid.insert(*cid, selected);
        }

        // Generate wantlist for each peer
        for (peer, state) in self.peers.iter_mut() {
            // Clear out bad connections
            match state.sending_state {
                SendingState::Ready => {
                    // Allowed to send
                }
                SendingState::Requested(instant, connection_id) => {
                    if instant.elapsed() < RECEIVE_REQUEST_TIMEOUT {
                        // Sending in progress
                        continue;
                    }
                    // Bad connection - request timeout
                    state.established_connections.remove(&connection_id);
                    state.send_full = true;
                    state.sending_state = SendingState::Ready;
                    state.metrics.record_failure();
                    tracing::debug!("Peer {} request timeout, recorded failure", peer);
                }
                SendingState::RequestReceived(..) => {
                    // Stream allocation in progress
                    continue;
                }
                SendingState::Sending(..) => {
                    // Sending in progress
                    continue;
                }
                SendingState::Failed(connection_id) => {
                    // Bad connection - sending failed
                    state.established_connections.remove(&connection_id);
                    state.send_full = true;
                    state.sending_state = SendingState::Ready;
                    state.metrics.record_failure();
                    tracing::debug!("Peer {} sending failed, recorded failure", peer);
                }
            };

            let Some(connection_id) = state.established_connections.iter().next().copied() else {
                peers_without_connection.push(*peer);
                continue;
            };

            // Determine which CIDs this peer should receive WantBlock for
            let should_send_want_blocks: FnvHashSet<CidGeneric<S>> = selected_peers_for_cid
                .iter()
                .filter(|(_, selected_peers)| selected_peers.contains(peer))
                .map(|(cid, _)| *cid)
                .collect();

            let wantlist = if state.send_full {
                state.wantlist.generate_proto_full_with_filter(&self.wantlist, Some(&should_send_want_blocks))
            } else {
                state.wantlist.generate_proto_update_with_filter(&self.wantlist, Some(&should_send_want_blocks))
            };

            // Allow empty entries to be sent when send_full flag is set
            if state.send_full {
                state.send_full = false;
            } else if wantlist.entries.is_empty() {
                continue;
            }

            // Log WantBlock sending
            if !should_send_want_blocks.is_empty() {
                for cid in &should_send_want_blocks {
                    tracing::info!(
                        "send WantBlock to {} for CID {} | score: {:.2}",
                        peer,
                        cid,
                        state.metrics.calculate_score()
                    );
                }
            }

            self.queue.push_back(ToSwarm::NotifyHandler {
                peer_id: peer.to_owned(),
                handler: NotifyHandler::One(connection_id),
                event: ToHandlerEvent::SendWantlist(wantlist),
            });

            state.sending_state = SendingState::Requested(Instant::now(), connection_id);
            handler_updated = true;
        }

        // Remove dead peers
        for peer in peers_without_connection {
            self.peers.remove(&peer);
        }

        handler_updated
    }

    /// Set peer selection config
    pub(crate) fn set_peer_selection_config(&mut self, config: PeerSelectionConfig) {
        tracing::info!(
            "Peer selection config updated: top_n={}, enabled={}, min_peers={}, randomness={}",
            config.top_n, config.enabled, config.min_peers, config.enable_randomness
        );
        self.peer_selection_config = config;
    }

    /// Get peer selection config
    pub(crate) fn get_peer_selection_config(&self) -> &PeerSelectionConfig {
        &self.peer_selection_config
    }

    /// Get peer metrics
    pub(crate) fn get_peer_metrics(&self, peer_id: &PeerId) -> Option<&PeerMetrics> {
        self.peers.get(peer_id).map(|state| &state.metrics)
    }

    /// Get all peer rankings with detailed metrics
    pub(crate) fn get_peer_rankings(&self) -> Vec<PeerDetail> {
        let mut details: Vec<PeerDetail> = self.peers.iter()
            .map(|(peer_id, state)| {
                let metrics = &state.metrics;
                PeerDetail {
                    peer_id: peer_id.to_string(),
                    score: metrics.calculate_score(),
                    blocks_received: metrics.blocks_received,
                    failures: metrics.failures,
                    success_rate: metrics.success_rate(),
                    avg_speed: metrics.avg_speed(),
                }
            })
            .collect();
        details.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        details
    }

    /// Get global stats
    pub(crate) fn get_global_stats(&self) -> &GlobalStats {
        &self.global_stats
    }

    /// This is polled by `Behaviour`, which is polled by `Swarm`.
    pub(crate) fn poll(&mut self, cx: &mut Context) -> Poll<ToSwarm<Event, ToHandlerEvent>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Poll::Ready(ev);
            }

            if self.send_full_timer.poll_unpin(cx).is_ready() {
                for state in self.peers.values_mut() {
                    state.send_full = true;
                }

                // Reset timer and loop again to get it registered
                self.send_full_timer.reset(SEND_FULL_INTERVAL);
                continue;
            }

            if let Poll::Ready(Some(task_result)) = self.tasks.poll_next_unpin(cx) {
                match task_result {
                    // Blockstore already has the data so return them to the user
                    TaskResult::Get(query_id, _, Ok(Some(data))) => {
                        return Poll::Ready(ToSwarm::GenerateEvent(Event::GetQueryResponse {
                            query_id,
                            peer_id: None,
                            data: data.clone(),
                        }));
                    }

                    // If blockstore doesn't have the data, add CID in the wantlist.
                    //
                    // Connection handlers will be informed via `update_handlers` about the new items in wantlist.
                    TaskResult::Get(query_id, cid, Ok(None)) => {
                        self.wantlist.insert(cid);
                        self.cid_to_queries.entry(cid).or_default().push(query_id);
                    }

                    // Blockstore error
                    TaskResult::Get(query_id, _, Err(e)) => {
                        return Poll::Ready(ToSwarm::GenerateEvent(Event::GetQueryError {
                            query_id,
                            error: e.into(),
                        }));
                    }

                    TaskResult::Set(Ok(blocks)) => {
                        self.new_blocks.extend(blocks);
                    }

                    // TODO: log it
                    TaskResult::Set(Err(_e)) => {}

                    // Nothing to do
                    TaskResult::Cancelled => {}
                }

                // If we didn't return an event, we need to retry the whole loop
                continue;
            }

            if self.update_handlers() {
                // New events generated, loop again to send them.
                continue;
            }

            return Poll::Pending;
        }
    }

    pub(crate) fn get_new_blocks(&mut self) -> Vec<(CidGeneric<S>, Vec<u8>)> {
        take(&mut self.new_blocks)
    }
}

pub(crate) struct ClientConnectionHandler<const S: usize> {
    peer_id: PeerId,
    connection_id: ConnectionId,
    queue: VecDeque<ToBehaviourEvent<S>>,
    protocol: StreamProtocol,
    msg: Option<Message>,
    sink_state: SinkState,
    sending_state: SendingState,
    closing: bool,
    halted: bool,
    start_sending_timeout: Option<Delay>,
}

enum SinkState {
    None,
    Requested,
    Ready(FramedWrite<libp2p_swarm::Stream, Codec>),
}

impl<const S: usize> ClientConnectionHandler<S> {
    pub(crate) fn halted(&self) -> bool {
        self.halted
    }

    pub(crate) fn set_stream(&mut self, stream: libp2p_swarm::Stream) {
        if self.halted {
            return;
        }

        // Convert `AsyncWrite` stream to `Sink`
        self.sink_state = SinkState::Ready(FramedWrite::new(stream, Codec));
    }

    pub(crate) fn stream_allocation_failed(&mut self) {
        if self.halted {
            return;
        }

        debug_assert!(matches!(self.sink_state, SinkState::Requested));
        // Reset state to force a new allocation in `poll`.
        self.sink_state = SinkState::None;
    }

    /// Initiate sending of a wantlist to the peer.
    pub(crate) fn send_wantlist(&mut self, wantlist: ProtoWantlist) {
        if self.halted {
            return;
        }

        debug_assert!(self.msg.is_none());
        debug_assert!(matches!(self.sending_state, SendingState::Ready));

        self.msg = Some(Message {
            wantlist: Some(wantlist),
            ..Message::default()
        });

        self.change_sending_state(SendingState::RequestReceived(
            Instant::now(),
            self.connection_id,
        ));

        // Before reaching the `Sending` state, a stream allocation must happen.
        // This can take time or require multiple retries. We specify how much time we
        // are willing to wait until `Sending` is reached.
        self.start_sending_timeout = Some(Delay::new(START_SENDING_TIMEOUT));
    }

    /// Changes sending state if needed and informs `ClientBehaviour` if there is a change.
    fn change_sending_state(&mut self, state: SendingState) {
        if self.sending_state != state {
            self.sending_state = state;
            self.queue
                .push_back(ToBehaviourEvent::SendingStateChanged(self.peer_id, state));
        }
    }

    fn open_new_substream(
        &mut self,
    ) -> Poll<
        ConnectionHandlerEvent<ReadyUpgrade<StreamProtocol>, StreamRequester, ToBehaviourEvent<S>>,
    > {
        self.sink_state = SinkState::Requested;

        Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
            protocol: SubstreamProtocol::new(
                ReadyUpgrade::new(self.protocol.clone()),
                StreamRequester::Client,
            ),
        })
    }

    fn close_sink_on_error(&mut self, location: &str) {
        debug!("sink operation failed, closing: {location}");
        self.sink_state = SinkState::None;
    }

    /// This is polled when the `ConnectionHandler` task initiates the closing of the connection.
    ///
    /// This method needs to return all the remaining events that are going to be send to
    /// the behaviour. It is polled in a stream-like fashion and stops when `Poll::Ready(None)`
    /// is returned.
    ///
    /// After reaching this point, `poll` method will never be called again.
    pub(crate) fn poll_close(&mut self, cx: &mut Context) -> Poll<Option<ToBehaviourEvent<S>>> {
        if !self.closing {
            self.closing = true;
            self.msg.take();

            if let SinkState::Ready(mut sink) = mem::replace(&mut self.sink_state, SinkState::None)
            {
                // Close the sink but don't await for it.
                let _ = sink.poll_close_unpin(cx);
            }

            // If sending is in progress, then we don't know how much data the other end received
            // so we consider this as "failed".
            if matches!(
                self.sending_state,
                SendingState::RequestReceived(..) | SendingState::Sending(..)
            ) {
                self.change_sending_state(SendingState::Failed(self.connection_id));
            }

            self.queue
                .push_back(ToBehaviourEvent::ClientClosingConnection(
                    self.peer_id,
                    self.connection_id,
                ));
        }

        Poll::Ready(self.queue.pop_front())
    }

    /// Each connection has its own dedicated task, which polls this method.
    pub(crate) fn poll(&mut self, cx: &mut Context) -> Poll<ConnHandlerEvent<S>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(ev));
            }

            if self.halted {
                return Poll::Pending;
            }

            if let Some(delay) = &mut self.start_sending_timeout {
                // If we have never reached the `Sending` state within the specified
                // time, we abort and halt this connection.
                if delay.poll_unpin(cx).is_ready() {
                    self.start_sending_timeout.take();
                    self.msg.take();
                    self.close_sink_on_error("start_sending_timeout");
                    self.change_sending_state(SendingState::Failed(self.connection_id));
                    self.halted = true;
                    continue;
                }
            }

            match (&mut self.msg, &mut self.sink_state) {
                (None, SinkState::None) => return Poll::Pending,
                (Some(_), SinkState::None) => return self.open_new_substream(),
                (_, SinkState::Requested) => return Poll::Pending,
                (None, SinkState::Ready(sink)) => {
                    // When `poll_flush` returns `Ok`, it means the sending just finished.
                    // When `poll_flush` returns `Err`, it means the sending just failed.
                    if ready!(sink.poll_flush_unpin(cx)).is_err() {
                        self.close_sink_on_error("poll_flush_unpin");
                        self.change_sending_state(SendingState::Failed(self.connection_id));
                        continue;
                    }

                    // Sending finished and we have nothing else to send, so we close the stream.
                    let _ = sink.poll_close_unpin(cx);
                    self.sink_state = SinkState::None;
                    self.change_sending_state(SendingState::Ready);
                }
                (msg @ Some(_), SinkState::Ready(sink)) => {
                    if ready!(sink.poll_ready_unpin(cx)).is_err() {
                        self.close_sink_on_error("poll_ready_unpin");
                        continue;
                    }

                    let msg = msg.take().expect("msg is always Some here");

                    if sink.start_send_unpin(&msg).is_err() {
                        self.msg = Some(msg);
                        self.close_sink_on_error("start_send_unpin");
                        continue;
                    }

                    // Stop the timer because sending started
                    self.start_sending_timeout = None;

                    self.change_sending_state(SendingState::Sending(
                        Instant::now(),
                        self.connection_id,
                    ));

                    // Loop again, so `poll_flush` will be called and register a waker.
                }
            }
        }
    }
}

impl<const S: usize> fmt::Debug for ClientConnectionHandler<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientConnectionHandler")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cid_prefix::CidPrefix;
    use crate::proto::message::mod_Message::mod_Wantlist::{Entry, WantType};
    use crate::proto::message::mod_Message::{Block, BlockPresence, Wantlist};
    use crate::test_utils::{cid_of_data, poll_fn_once};
    use crate::Behaviour;
    use asynchronous_codec::FramedRead;
    use blockstore::InMemoryBlockstore;
    use futures_util::future::{self, Either};
    use libp2p_stream::IncomingStreams;
    use libp2p_swarm::Swarm;
    use libp2p_swarm_test::SwarmExt;
    use std::pin::pin;
    use tokio::time::sleep;

    #[tokio::test]
    async fn get_unknown_cid_responds_with_have() {
        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let (mut server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let cid1 = cid_of_data(b"x1");
        let _query_id1 = client.behaviour_mut().get(&cid1);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }],
                    full: false,
                }),
                ..Default::default()
            }]
        );

        send_message_to_client(
            &mut server_control,
            &mut client,
            Message {
                wantlist: None,
                payload: vec![],
                blockPresences: vec![BlockPresence {
                    cid: cid1.to_bytes(),
                    type_pb: BlockPresenceType::Have,
                }],
                pendingBytes: 0,
            },
        )
        .await;

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Block,
                        sendDontHave: true,
                    }],
                    full: false,
                }),
                payload: vec![],
                blockPresences: vec![],
                pendingBytes: 0
            }]
        );
    }

    #[tokio::test]
    async fn get_unknown_cid_responds_with_dont_have() {
        let server1 = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let server2 = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let (mut server1_control, mut server1_incoming_streams) =
            connect_to_server(&mut client, server1).await;
        let (_server2_control, mut server2_incoming_streams) =
            connect_to_server(&mut client, server2).await;

        // Initial full list sent to server1
        let msgs = collect_incoming_messages(&mut server1_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        // Initial full list sent to server2
        let msgs = collect_incoming_messages(&mut server2_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let cid1 = cid_of_data(b"x1");
        let _query_id1 = client.behaviour_mut().get(&cid1);

        let expected_msgs = vec![Message {
            wantlist: Some(Wantlist {
                entries: vec![Entry {
                    block: cid1.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Have,
                    sendDontHave: true,
                }],
                full: false,
            }),
            payload: vec![],
            blockPresences: vec![],
            pendingBytes: 0,
        }];

        let msgs = collect_incoming_messages(&mut server1_incoming_streams, &mut client).await;
        assert_eq!(&msgs, &expected_msgs);

        let msgs = collect_incoming_messages(&mut server2_incoming_streams, &mut client).await;
        assert_eq!(&msgs, &expected_msgs);

        send_message_to_client(
            &mut server1_control,
            &mut client,
            Message {
                wantlist: None,
                payload: vec![],
                blockPresences: vec![BlockPresence {
                    cid: cid1.to_bytes(),
                    type_pb: BlockPresenceType::DontHave,
                }],
                pendingBytes: 0,
            },
        )
        .await;

        // Mark that full wantlist must be send
        for peer_state in client.behaviour_mut().client.peers.values_mut() {
            peer_state.send_full = true;
        }

        // `client` sends a full wantlist to `server1` but without the `cid1` because server
        // already replied with DontHave.
        let msgs = collect_incoming_messages(&mut server1_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let msgs = collect_incoming_messages(&mut server2_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }],
                    full: true,
                }),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn get_unknown_cid_responds_with_block() {
        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let (mut server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let data1 = b"x1";
        let cid1 = cid_of_data(data1);
        let query_id1 = client.behaviour_mut().get(&cid1);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }],
                    full: false,
                }),
                payload: vec![],
                blockPresences: vec![],
                pendingBytes: 0
            }]
        );

        let ev = send_message_to_client_and_wait_beheviour_event(
            &mut server_control,
            &mut client,
            Message {
                wantlist: None,
                payload: vec![Block {
                    prefix: CidPrefix::from_cid(&cid1).to_bytes(),
                    data: data1.to_vec(),
                }],
                blockPresences: vec![],
                pendingBytes: 0,
            },
        )
        .await;

        let (query_id, data) = unwrap_get_query_reponse(ev);
        assert_eq!(query_id, query_id1);
        assert_eq!(data, data1);

        // Poll once more for the store to be updated. This does not produce an event.
        poll_fn_once(|cx| client.poll_next_unpin(cx)).await;
        assert_eq!(
            client
                .behaviour()
                .client
                .store
                .get(&cid1)
                .await
                .unwrap()
                .unwrap(),
            data1
        );
    }

    #[tokio::test]
    async fn update_wantlist() {
        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let (_server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let cid1 = cid_of_data(b"x1");
        let cid2 = cid_of_data(b"x2");
        let cid3 = cid_of_data(b"x3");

        let _query_id1 = client.behaviour_mut().get(&cid1);
        let _query_id2 = client.behaviour_mut().get(&cid2);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![
                        Entry {
                            block: cid2.to_bytes(),
                            priority: 1,
                            cancel: false,
                            wantType: WantType::Have,
                            sendDontHave: true,
                        },
                        Entry {
                            block: cid1.to_bytes(),
                            priority: 1,
                            cancel: false,
                            wantType: WantType::Have,
                            sendDontHave: true,
                        }
                    ],
                    full: false,
                }),
                ..Default::default()
            }]
        );

        let _query_id3 = client.behaviour_mut().get(&cid3);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid3.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }],
                    full: false,
                }),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn request_then_cancel() {
        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let (_server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let cid1 = cid_of_data(b"x1");
        let cid2 = cid_of_data(b"x2");

        let query_id1 = client.behaviour_mut().get(&cid1);
        let query_id2 = client.behaviour_mut().get(&cid2);

        // This cancel will not generate any messages because request was not send yet
        client.behaviour_mut().cancel(query_id2);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },],
                    full: false,
                }),
                ..Default::default()
            }]
        );

        // This cancel should produce a message for cancelling the request
        client.behaviour_mut().cancel(query_id1);

        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid1.to_bytes(),
                        cancel: true,
                        ..Default::default()
                    },],
                    full: false,
                }),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn request_before_connect() {
        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(|_| {
            Behaviour::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
        });

        let cid1 = cid_of_data(b"x1");
        let cid2 = cid_of_data(b"x2");
        let cid3 = cid_of_data(b"x3");

        let _query_id1 = client.behaviour_mut().get(&cid1);
        let query_id2 = client.behaviour_mut().get(&cid2);
        let _query_id3 = client.behaviour_mut().get(&cid3);

        // Cancel request of `cid2`.
        client.behaviour_mut().cancel(query_id2);

        let (_server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server should contain `cid1` and `cid3`.
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![
                        Entry {
                            block: cid3.to_bytes(),
                            priority: 1,
                            cancel: false,
                            wantType: WantType::Have,
                            sendDontHave: true,
                        },
                        Entry {
                            block: cid1.to_bytes(),
                            priority: 1,
                            cancel: false,
                            wantType: WantType::Have,
                            sendDontHave: true,
                        }
                    ],
                    full: true,
                }),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn get_known_cid() {
        let data1 = b"x1";
        let cid1 = cid_of_data(data1);
        let cid2 = cid_of_data(b"x2");

        let blockstore = Arc::new(InMemoryBlockstore::<64>::new());
        blockstore.put_keyed(&cid1, data1).await.unwrap();

        let server = Swarm::new_ephemeral(|_| libp2p_stream::Behaviour::new());
        let mut client = Swarm::new_ephemeral(move |_| Behaviour::<64, _>::new(blockstore));

        let (_server_control, mut server_incoming_streams) =
            connect_to_server(&mut client, server).await;

        // Initial full list sent to server
        let msgs = collect_incoming_messages(&mut server_incoming_streams, &mut client).await;
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![],
                    full: true,
                }),
                ..Default::default()
            }]
        );

        let query_id1 = client.behaviour_mut().get(&cid1);
        let _query_id2 = client.behaviour_mut().get(&cid2);

        let (msgs, ev) = collect_incoming_messages_and_behaviour_event(
            &mut server_incoming_streams,
            &mut client,
        )
        .await;

        // `cid1` is known, so client replies without sending a request.
        let (query_id, data) = unwrap_get_query_reponse(ev);
        assert_eq!(query_id, query_id1);
        assert_eq!(data, data1);

        // `cid2` is not know, so client sends a request.
        assert_eq!(
            msgs,
            vec![Message {
                wantlist: Some(Wantlist {
                    entries: vec![Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },],
                    full: false,
                }),
                ..Default::default()
            }]
        );
    }

    async fn connect_to_server(
        client: &mut Swarm<Behaviour<64, InMemoryBlockstore<64>>>,
        mut server: Swarm<libp2p_stream::Behaviour>,
    ) -> (libp2p_stream::Control, libp2p_stream::IncomingStreams) {
        let mut server_control = server.behaviour().new_control();
        let server_incoming_streams = server_control
            .accept(StreamProtocol::new("/ipfs/bitswap/1.2.0"))
            .unwrap();

        server.listen().with_memory_addr_external().await;
        client.connect(&mut server).await;

        // Server can be controled by `server_control` but it still needs
        // to be driven by the executor.
        tokio::spawn(server.loop_on_next());

        (server_control, server_incoming_streams)
    }

    async fn collect_incoming_messages(
        server_incoming_streams: &mut IncomingStreams,
        client: &mut Swarm<Behaviour<64, InMemoryBlockstore<64>>>,
    ) -> Vec<Message> {
        let server_fut = pin!(async {
            let (peer_id, stream) = server_incoming_streams.next().await.unwrap();
            let stream = FramedRead::new(stream, Codec);
            let msgs = stream.map(|res| res.unwrap()).collect::<Vec<_>>().await;
            (peer_id, msgs)
        });

        let client_peer_id = *client.local_peer_id();
        let client_fut = pin!(client.next_behaviour_event());

        match future::select(server_fut, client_fut).await {
            Either::Left(((peer_id, mut msgs), _)) => {
                assert_eq!(peer_id, client_peer_id);

                // Sort message for easier testing
                for msg in &mut msgs {
                    if let Some(wantlist) = &mut msg.wantlist {
                        wantlist
                            .entries
                            .sort_by(|entry1, entry2| entry1.block.cmp(&entry2.block));
                    }

                    msg.payload
                        .sort_by(|block1, block2| block1.data.cmp(&block2.data));
                    msg.blockPresences
                        .sort_by(|presence1, presence2| presence1.cid.cmp(&presence2.cid));
                }

                msgs
            }
            Either::Right((ev, _)) => panic!("Received behaviour event on client: {ev:?}"),
        }
    }

    async fn collect_incoming_messages_and_behaviour_event(
        server_incoming_streams: &mut IncomingStreams,
        client: &mut Swarm<Behaviour<64, InMemoryBlockstore<64>>>,
    ) -> (Vec<Message>, Event) {
        let mut server_fut = async {
            let (peer_id, stream) = server_incoming_streams.next().await.unwrap();
            let stream = FramedRead::new(stream, Codec);
            let msgs = stream.map(|res| res.unwrap()).collect::<Vec<_>>().await;
            (peer_id, msgs)
        }
        .boxed()
        .fuse();

        let mut msgs = None;
        let mut ev = None;

        // We need to keep polling `client` even after it generates an event
        // otherwise `server_fut` will not progress.
        while msgs.is_none() || ev.is_none() {
            tokio::select! {
                (peer_id, m) = &mut server_fut => {
                    assert_eq!(peer_id, *client.local_peer_id());
                    msgs = Some(m);
                }
                e = client.next_behaviour_event() => {
                    assert!(ev.is_none());
                    ev = Some(e);
                }
            }
        }

        (msgs.unwrap(), ev.unwrap())
    }

    async fn send_message_to_client(
        server_control: &mut libp2p_stream::Control,
        client: &mut Swarm<Behaviour<64, InMemoryBlockstore<64>>>,
        msg: Message,
    ) {
        let client_peer_id = *client.local_peer_id();

        let server_fut = pin!(async {
            let stream = server_control
                .open_stream(client_peer_id, StreamProtocol::new("/ipfs/bitswap/1.2.0"))
                .await
                .unwrap();
            let mut stream = FramedWrite::new(stream, Codec);
            stream.send(&msg).await.unwrap();
            // Wait a bit for the client to process it
            sleep(Duration::from_millis(10)).await;
        });

        let client_fut = pin!(client.next_behaviour_event());

        match future::select(server_fut, client_fut).await {
            Either::Left((_, _)) => {}
            Either::Right((ev, _)) => panic!("Received behaviour event on client: {ev:?}"),
        }
    }

    async fn send_message_to_client_and_wait_beheviour_event(
        server_control: &mut libp2p_stream::Control,
        client: &mut Swarm<Behaviour<64, InMemoryBlockstore<64>>>,
        msg: Message,
    ) -> Event {
        let client_peer_id = *client.local_peer_id();

        let server_fut = pin!(async {
            let stream = server_control
                .open_stream(client_peer_id, StreamProtocol::new("/ipfs/bitswap/1.2.0"))
                .await
                .unwrap();
            let mut stream = FramedWrite::new(stream, Codec);
            stream.send(&msg).await.unwrap();
        });

        let client_fut = pin!(client.next_behaviour_event());

        future::join(server_fut, client_fut).await.1
    }

    fn unwrap_get_query_reponse(ev: Event) -> (QueryId, Vec<u8>) {
        match ev {
            Event::GetQueryResponse { query_id, peer_id: _, data } => (query_id, data),
            ev => panic!("Expected Event::GetQueryResponse, got {ev:?}"),
        }
    }
}
