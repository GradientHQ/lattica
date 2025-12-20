#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

use std::fmt;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use blockstore::Blockstore;
use cid::CidGeneric;
use client::SendingState;
use futures_util::stream::{SelectAll, StreamExt};
use incoming_stream::IncomingMessage;
use libp2p_core::multiaddr::Multiaddr;
use libp2p_core::transport::PortUse;
use libp2p_core::upgrade::ReadyUpgrade;
use libp2p_core::Endpoint;
use libp2p_identity::PeerId;
use libp2p_swarm::{
    handler::ConnectionEvent, ConnectionClosed, ConnectionDenied, ConnectionHandler,
    ConnectionHandlerEvent, ConnectionId, FromSwarm, NetworkBehaviour, StreamProtocol,
    SubstreamProtocol, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use tracing::trace;

mod builder;
mod cid_prefix;
mod client;
mod incoming_stream;
mod message;
pub mod multihasher;
mod peer_selection;
mod proto;
mod server;
#[cfg(test)]
mod test_utils;
pub mod utils;
mod wantlist;

use crate::client::{ClientBehaviour, ClientConnectionHandler};
use crate::incoming_stream::IncomingStream;
use crate::multihasher::MultihasherTable;
use crate::proto::message::mod_Message::Wantlist as ProtoWantlist;
use crate::server::{ServerBehaviour, ServerConnectionHandler};

pub use crate::builder::BehaviourBuilder;
pub use crate::client::QueryId;
pub use crate::peer_selection::{GlobalStats, PeerMetrics, PeerSelectionConfig};

/// [`NetworkBehaviour`] for Bitswap protocol.
#[derive(Debug)]
pub struct Behaviour<const MAX_MULTIHASH_SIZE: usize, B>
where
    B: Blockstore + 'static,
{
    protocol: StreamProtocol,
    pub(crate) client: ClientBehaviour<MAX_MULTIHASH_SIZE, B>,
    server: ServerBehaviour<MAX_MULTIHASH_SIZE, B>,
    multihasher: Arc<MultihasherTable<MAX_MULTIHASH_SIZE>>,
}

/// Event produced by [`Behaviour`].
pub enum Event {
    /// Requested block has been successfuly retrieved
    GetQueryResponse {
        /// Id of the query, returned by [`Behaviour::get`]
        query_id: QueryId,
        /// The peer that provided the block. None if the block was found in local blockstore.
        peer_id: Option<PeerId>,
        /// Data of the requested block
        data: Vec<u8>,
    },
    /// Error occurred while fetching block
    GetQueryError {
        /// Id of the query, returned by [`Behaviour::get`]
        query_id: QueryId,
        /// Error that occurred when getting the data
        error: Error,
    },
}

struct DataFmt<'a>(&'a [u8]);

impl fmt::Debug for DataFmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("[... {} bytes]", self.0.len()))
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GetQueryResponse { query_id, peer_id, data } => f
                .debug_struct("GetQueryResponse")
                .field("query_id", query_id)
                .field("peer_id", peer_id)
                .field("data", &DataFmt(data))
                .finish(),
            Self::GetQueryError { query_id, error } => f
                .debug_struct("GetQueryError")
                .field("query_id", query_id)
                .field("error", error)
                .finish(),
        }
    }
}

/// Representation of all the errors that can occur when interacting with this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Encountered CID with multihash longer than max set when creating the [`Behaviour`]
    #[error("Invalid multihash size")]
    InvalidMultihashSize,

    /// Invalid protocol prefix provided when building `Behaviour`, see
    /// [`BehaviourBuilder::protocol_prefix`]
    #[error("Invalid protocol prefix: {0}")]
    InvalidProtocolPrefix(String),

    /// Error received when interacting with blockstore
    #[error("Blockstore error: {0}")]
    Blockstore(#[from] blockstore::Error),
}

/// Alias for a [`Result`] with the error type [`beetswap::Error`].
///
/// [`beetswap::Error`]: crate::Error
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl<const MAX_MULTIHASH_SIZE: usize, B> Behaviour<MAX_MULTIHASH_SIZE, B>
where
    B: Blockstore + 'static,
{
    /// Creates a new [`Behaviour`] with the default configuration.
    pub fn new(blockstore: Arc<B>) -> Behaviour<MAX_MULTIHASH_SIZE, B> {
        BehaviourBuilder::new(blockstore).build()
    }

    /// Creates a new [`BehaviourBuilder`].
    pub fn builder(blockstore: Arc<B>) -> BehaviourBuilder<MAX_MULTIHASH_SIZE, B> {
        BehaviourBuilder::new(blockstore)
    }

    /// Start a query that returns the raw data of a [`Cid`].
    ///
    /// [`Cid`]: cid::CidGeneric
    pub fn get<const S: usize>(&mut self, cid: &CidGeneric<S>) -> QueryId {
        self.client.get(cid)
    }

    /// Cancel an ongoing query.
    pub fn cancel(&mut self, query_id: QueryId) {
        self.client.cancel(query_id)
    }

    /// 设置节点选择配置
    pub fn set_peer_selection_config(&mut self, config: PeerSelectionConfig) {
        self.client.set_peer_selection_config(config);
    }

    /// 获取节点选择配置
    pub fn get_peer_selection_config(&self) -> &PeerSelectionConfig {
        self.client.get_peer_selection_config()
    }

    /// 获取节点性能指标
    pub fn get_peer_metrics(&self, peer_id: &PeerId) -> Option<&PeerMetrics> {
        self.client.get_peer_metrics(peer_id)
    }

    /// 获取所有节点的评分排名
    pub fn get_peer_rankings(&self) -> Vec<(PeerId, f64)> {
        self.client.get_peer_rankings()
    }

    /// 获取全局传输统计
    pub fn get_global_stats(&self) -> &GlobalStats {
        self.client.get_global_stats()
    }

    /// 打印统计报告
    pub fn print_stats_report(&self) {
        self.client.print_stats_report();
    }
}

impl<const MAX_MULTIHASH_SIZE: usize, B> NetworkBehaviour for Behaviour<MAX_MULTIHASH_SIZE, B>
where
    B: Blockstore + 'static,
{
    type ConnectionHandler = ConnHandler<MAX_MULTIHASH_SIZE>;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(ConnHandler {
            peer,
            protocol: self.protocol.clone(),
            client_handler: self.client.new_connection_handler(peer, connection_id),
            server_handler: self.server.new_connection_handler(peer),
            incoming_streams: SelectAll::new(),
            multihasher: self.multihasher.clone(),
        })
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(ConnHandler {
            peer,
            protocol: self.protocol.clone(),
            client_handler: self.client.new_connection_handler(peer, connection_id),
            server_handler: self.server.new_connection_handler(peer),
            incoming_streams: SelectAll::new(),
            multihasher: self.multihasher.clone(),
        })
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        #[allow(clippy::single_match)]
        match event {
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                connection_id,
                ..
            }) => {
                self.client.on_connection_closed(peer_id, connection_id);
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            ToBehaviourEvent::IncomingMessage(peer, mut msg) => {
                if let Some(client_msg) = msg.client.take() {
                    self.client.process_incoming_message(peer, client_msg);
                }

                if let Some(server_msg) = msg.server.take() {
                    self.server.process_incoming_message(peer, server_msg);
                }
            }
            ToBehaviourEvent::NewBlocksAvailable(blocks) => {
                trace!("received new blocks: {}", blocks.len());
                self.server.new_blocks_available(blocks);
            }
            ToBehaviourEvent::SendingStateChanged(peer_id, state) => {
                self.client.sending_state_changed(peer_id, state);
            }
            ToBehaviourEvent::ClientClosingConnection(peer_id, connection_id) => {
                self.client.on_connection_closed(peer_id, connection_id);
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let ready @ Poll::Ready(_) = self.client.poll(cx) {
            return ready;
        }

        let new_blocks = self.client.get_new_blocks();
        if !new_blocks.is_empty() {
            self.server.new_blocks_available(new_blocks);
        }

        // call server last so that it can process new blocks from client and blockstore
        // together
        if let ready @ Poll::Ready(_) = self.server.poll(cx) {
            return ready;
        }

        Poll::Pending
    }
}

#[derive(Debug)]
#[doc(hidden)]
pub enum ToBehaviourEvent<const S: usize> {
    IncomingMessage(PeerId, IncomingMessage<S>),
    NewBlocksAvailable(Vec<(CidGeneric<S>, Vec<u8>)>),
    SendingStateChanged(PeerId, SendingState),
    ClientClosingConnection(PeerId, ConnectionId),
}

#[derive(Debug)]
#[doc(hidden)]
pub enum ToHandlerEvent {
    SendWantlist(ProtoWantlist),
    QueueOutgoingMessages(Vec<(Vec<u8>, Vec<u8>)>),
}

#[doc(hidden)]
pub enum StreamRequester {
    Client,
    Server,
}

#[derive(Debug)]
#[doc(hidden)]
pub struct ConnHandler<const MAX_MULTIHASH_SIZE: usize> {
    peer: PeerId,
    protocol: StreamProtocol,
    client_handler: ClientConnectionHandler<MAX_MULTIHASH_SIZE>,
    server_handler: ServerConnectionHandler<MAX_MULTIHASH_SIZE>,
    incoming_streams: SelectAll<IncomingStream<MAX_MULTIHASH_SIZE>>,
    multihasher: Arc<MultihasherTable<MAX_MULTIHASH_SIZE>>,
}

pub(crate) type ConnHandlerEvent<const S: usize> =
    ConnectionHandlerEvent<ReadyUpgrade<StreamProtocol>, StreamRequester, ToBehaviourEvent<S>>;

impl<const MAX_MULTIHASH_SIZE: usize> ConnectionHandler for ConnHandler<MAX_MULTIHASH_SIZE> {
    type ToBehaviour = ToBehaviourEvent<MAX_MULTIHASH_SIZE>;
    type FromBehaviour = ToHandlerEvent;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundOpenInfo = StreamRequester;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol.clone()), ())
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            ToHandlerEvent::SendWantlist(wantlist) => {
                self.client_handler.send_wantlist(wantlist);
            }
            ToHandlerEvent::QueueOutgoingMessages(data) => {
                self.server_handler.queue_messages(data);
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            '_,
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedOutbound(outbound) => match outbound.info {
                StreamRequester::Client => self.client_handler.set_stream(outbound.protocol),
                StreamRequester::Server => self.server_handler.set_stream(outbound.protocol),
            },
            ConnectionEvent::DialUpgradeError(outbound) => match outbound.info {
                StreamRequester::Client => self.client_handler.stream_allocation_failed(),
                StreamRequester::Server => {
                    // TODO
                }
            },
            ConnectionEvent::FullyNegotiatedInbound(ev) => {
                let stream = IncomingStream::new(ev.protocol, self.multihasher.clone());
                self.incoming_streams.push(stream);
            }
            _ => (),
        }
    }

    fn connection_keep_alive(&self) -> bool {
        !self.client_handler.halted()
    }

    fn poll_close(&mut self, cx: &mut Context) -> Poll<Option<Self::ToBehaviour>> {
        if let Some(ev) = ready!(self.client_handler.poll_close(cx)) {
            return Poll::Ready(Some(ev));
        }

        Poll::Ready(None)
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ConnHandlerEvent<MAX_MULTIHASH_SIZE>> {
        if let Poll::Ready(Some(msg)) = self.incoming_streams.poll_next_unpin(cx) {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                ToBehaviourEvent::IncomingMessage(self.peer, msg),
            ));
        }

        if let Poll::Ready(ev) = self.client_handler.poll(cx) {
            return Poll::Ready(ev);
        }

        if let Poll::Ready(ev) = self.server_handler.poll(cx) {
            return Poll::Ready(ev);
        }

        Poll::Pending
    }
}
