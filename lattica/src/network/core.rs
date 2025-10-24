use crate::common;
use crate::rpc;
use super::{*};
use std::{sync::Arc, error::Error, fs};
use anyhow::{Result, anyhow};
use chrono::Utc;
use libp2p::{noise, tcp, yamux, Swarm, SwarmBuilder, identity,swarm::{SwarmEvent},
             kad::{RecordKey,Mode, PeerRecord, Quorum, Record}, Multiaddr, multiaddr::{Protocol}, PeerId, futures::{future, StreamExt},
             request_response::{OutboundRequestId}, Stream};
use futures::{AsyncReadExt, AsyncWriteExt};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use bincode::config::standard;
use futures::future::Either;
use tokio::task::JoinHandle;
use fnv::{FnvHashMap};
use uuid::Uuid;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use std::time::{Duration, Instant};
use blockstore::{Blockstore, SledBlockstore};
use blockstore::block::Block;
use crate::common::{compress_data, should_compress, BytesBlock, CompressionAlgorithm, CompressionLevel, QueryId};
use cid::Cid;
use futures::io::{WriteHalf};

pub enum Command{
    PutRecord(Record, Quorum, oneshot::Sender<Result<()>>),
    GetRecord(RecordKey, Quorum, oneshot::Sender<Result<Vec<PeerRecord>>>),
    RpcConnect(PeerId, oneshot::Sender<Result<()>>),
    Dial(Multiaddr, oneshot::Sender<Result<()>>),
    RpcSend(PeerId, rpc::RpcRequest, oneshot::Sender<Result<rpc::RpcResponse>>),
    RpcGetOrCreateStreamHandle(PeerId, oneshot::Sender<Result<Arc<StreamHandle>>>),
    RpcRegister(Box<dyn rpc::RpcService>, oneshot::Sender<Result<()>>),
    RendezvousRegister(PeerId, Option<String>, Option<u64>, oneshot::Sender<Result<()>>),
    RendezvousDiscover(PeerId, Option<String>, Option<u64>, oneshot::Sender<Result<Vec<PeerId>>>),
    GetVisibleMAddrs(oneshot::Sender<Result<Vec<Multiaddr>>>),
    Get(Cid, oneshot::Sender<Result<BytesBlock>>),
    StartProviding(RecordKey, oneshot::Sender<Result<()>>),
    GetProviders(RecordKey, oneshot::Sender<Result<Vec<PeerId>>>),
    StopProviding(RecordKey, oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct Lattica {
    _swarm_handle: Arc<JoinHandle<()>>,
    cmd: mpsc::Sender<Command>,
    config: Arc<Config>,
    address_book: Arc<RwLock<AddressBook>>,
    storage: Arc<SledBlockstore>,
}

pub struct LatticaBuilder {
    config: Config,
}

impl LatticaBuilder {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }

    pub fn with_bootstrap_nodes(mut self, nodes: Vec<Multiaddr>) -> Self {
        self.config.bootstrap_nodes = nodes;
        self
    }

    pub fn with_kad(mut self, kad: bool) -> Self {
        self.config.with_kad = kad;
        self
    }

    pub fn with_listen_addrs(mut self, addrs: Vec<Multiaddr>) -> Self {
        if !addrs.is_empty() {
            self.config.listen_addrs = addrs;
        }
        self
    }

    pub fn with_listen_addr(mut self, addr: Multiaddr) -> Self {
        self.config.listen_addrs.push(addr);
        self
    }

    pub fn with_external_addrs(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.config.external_addrs = addrs;
        self
    }

    pub fn with_external_addr(mut self, addr: Multiaddr) -> Self {
        self.config.external_addrs.push(addr);
        self
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.config.idle_timeout = timeout;
        self
    }

    pub fn with_key_path(mut self, key_path: Option<String>) -> Self {
        if let Some(key_path) = key_path {
            self.config.key_path = key_path;
        }
        self
    }

    pub fn with_rendezvous(mut self, enabled: bool) -> Self {
        self.config.with_rendezvous = enabled;
        self
    }

    pub fn with_rendezvous_servers(mut self, servers: Vec<(PeerId, Multiaddr)>) -> Self {
        self.config.rendezvous_servers = servers;
        self
    }

    pub fn with_mdns(mut self, enable: bool) -> Self {
        self.config.with_mdns = enable;
        self
    }

    pub fn with_upnp(mut self, enable: bool) -> Self {
        self.config.with_upnp = enable;
        self
    }

    pub fn with_autonat(mut self, enable: bool) -> Self {
        self.config.with_autonat = enable;
        self
    }

    pub fn with_dcutr(mut self, enable: bool) -> Self {
        self.config.with_dcutr = enable;
        self
    }

    pub fn with_relay_servers(mut self, servers: Vec<Multiaddr>) -> Self {
        if !servers.is_empty() {
            self.config.relay_servers = servers;
            self.config.with_relay = true
        }

        self
    }

    pub fn with_storage_path(mut self, storage_path: Option<String>) -> Self {
        if let Some(storage_path) = storage_path {
            self.config.storage_path = storage_path;
        }
        self
    }

    pub fn with_compression(mut self, algorithm: CompressionAlgorithm, level: CompressionLevel) -> Self {
        self.config.compression_algorithm = algorithm;
        self.config.compression_level = level;
        self
    }

    pub fn with_dht_db_path(mut self, db_path: Option<String>) -> Self {
        if let Some(db_path) = db_path {
            self.config.dht_db_path = db_path;
        }
        self
    }

    pub fn with_protocol_version(mut self, protocol: Option<String>) -> Self {
        if let Some(protocol) = protocol {
            self.config.protocol_version = protocol;
        }
        self
    }

    pub fn from_config(config: Config) -> Self {
        Self { config }
    }

    pub async fn build(mut self) -> Result<Lattica, Box<dyn Error>> {
        // load keypair
        let keypair_path = format!("{}/p2p.key", self.config.key_path.clone());
        if Path::new(&keypair_path).exists() {
            match fs::read(&keypair_path) {
                Ok(keypair_bytes) => {
                    match identity::Keypair::from_protobuf_encoding(&keypair_bytes) {
                        Ok(keypair) => {
                            self.config.keypair = keypair;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        } else {
            match self.config.keypair.to_protobuf_encoding() {
                Ok(keypair_bytes) => {
                    if let Some(parent) = Path::new(&keypair_path).parent() {
                        if !parent.exists() {
                            let _ = fs::create_dir_all(parent);
                        }
                    }
                    fs::write(&keypair_path, keypair_bytes)?;
                }
                _ => {}
            }
        }

        if self.config.protocol_version == "".to_string() {
            self.config.protocol_version = format!("/{}", self.config.keypair.public().to_peer_id().to_string());
        }

        let db = sled::open(self.config.clone().storage_path)?;
        let storage = SledBlockstore::new(db).await?;
        let storage_arc = Arc::new(storage);

        let mut swarm = SwarmBuilder::with_existing_identity(self.config.keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_quic()
            .with_dns()?
            .with_websocket(noise::Config::new, yamux::Config::default).await?
            .with_relay_client(noise::Config::new, yamux::Config::default)?
            .with_behaviour(|_keypair, relay_behaviour| {
                let relay_client = if self.config.with_relay {
                    Some(relay_behaviour)
                } else {
                    None
                };

                LatticaBehaviour::new(&mut self.config, relay_client, storage_arc.clone())
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(self.config.idle_timeout))
            .build();

        if !self.config.listen_addrs.is_empty() {
            for addr in self.config.listen_addrs.iter() {
                swarm.listen_on(addr.clone())?;
            }
        }

        // Add external addresses for rendezvous functionality
        if !self.config.external_addrs.is_empty() {
            for addr in self.config.external_addrs.iter() {
                swarm.add_external_address(addr.clone());
                tracing::info!("Added external address: {}", addr);
            }
        }

        let bootstrap_nodes = self.config.bootstrap_nodes.clone();
        if !bootstrap_nodes.is_empty() {
            if swarm.behaviour().is_kad_enabled(){
                if let Some(kad) = swarm.behaviour_mut().kad.as_mut() {
                    for addr in bootstrap_nodes.clone() {
                        if let Some(Protocol::P2p(peer_id)) = addr.iter().last() {
                            kad.add_address(&peer_id, addr.clone());
                        }
                    }
                    kad.bootstrap()?;
                }
            }
        }

        let relay_servers = self.config.relay_servers.clone();
        if swarm.behaviour().is_relay_enabled() && !relay_servers.is_empty() {
            for addr in relay_servers {
                let result = swarm.listen_on(addr.clone().with(Protocol::P2pCircuit));
                match result {
                    Err(err) => {
                        tracing::error!("Failed to listen on circuit {}: {}", addr.clone(), err);
                    }
                    _ => {}
                }
            }
        }

        let (cmd_tx, cmd_rx) = mpsc::channel(100);

        let address_book = AddressBook::new();
        let address_book_arc = Arc::new(RwLock::new(address_book));
        let address_book_clone = address_book_arc.clone();

        let _swarm_handle = tokio::spawn(
            swarm_poll(
                cmd_rx,
                swarm,
                self.config.clone(),
                address_book_clone,
            )
        );

        let lattica = Lattica{
            _swarm_handle: Arc::new(_swarm_handle),
            cmd: cmd_tx,
            config: Arc::new(self.config),
            address_book: address_book_arc,
            storage: storage_arc,
        };

        Ok(lattica)
    }
}

pub struct StreamHandle {
    writer: Arc<Mutex<WriteHalf<Stream>>>,

    // request_id -> response channel (for stream_iter)
    pending_stream_calls: Arc<RwLock<HashMap<String, mpsc::Sender<Vec<u8>>>>>,
    // request_id -> accumulated data (for call_stream)
    pending_unary_calls: Arc<RwLock<HashMap<String, (Vec<u8>, oneshot::Sender<Vec<u8>>)>>>,
    _read_task: Arc<JoinHandle<()>>,
}

impl StreamHandle {
    fn new(stream: Stream, peer_id: PeerId) -> Self {
        let (read_half, write_half) = stream.split();
        let reader = Arc::new(Mutex::new(read_half));
        let writer = Arc::new(Mutex::new(write_half));

        let pending_stream_calls: Arc<RwLock<HashMap<String, mpsc::Sender<Vec<u8>>>>>
            = Arc::new(RwLock::new(HashMap::new()));
        let pending_unary_calls: Arc<RwLock<HashMap<String, (Vec<u8>, oneshot::Sender<Vec<u8>>)>>>
            = Arc::new(RwLock::new(HashMap::new()));

        // run background task: response by request id
        let reader_clone = reader.clone();
        let pending_stream_clone = pending_stream_calls.clone();
        let pending_unary_clone = pending_unary_calls.clone();

        let read_task = tokio::spawn(async move {
            let mut buf = Vec::with_capacity(8 * 1024);

            loop {
                // read head frame
                let mut r = reader_clone.lock().await;

                let mut len_buf = [0u8; 4];
                if r.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;

                buf.resize(len, 0);
                if r.read_exact(&mut buf).await.is_err() {
                    break;
                }


                drop(r);

                // handle frame
                if let Ok((frame, _)) = bincode::decode_from_slice::<rpc::StreamFrame, _>(&buf, standard()) {
                    match frame {
                        rpc::StreamFrame::Data(msg) => {
                            if let Some(tx) = pending_stream_clone.read().await.get(&msg.id) {
                                if tx.try_send(msg.data.to_vec()).is_err() {
                                    pending_stream_clone.write().await.remove(&msg.id);
                                }
                            } else if let Some((acc, _tx)) = pending_unary_clone.write().await.get_mut(&msg.id) {
                                acc.extend_from_slice(&msg.data);
                            }
                        }
                        rpc::StreamFrame::Close(id) => {
                            if let Some(tx) = pending_stream_clone.write().await.remove(&id) {
                                drop(tx);
                            }
                            if let Some((data, tx)) = pending_unary_clone.write().await.remove(&id) {
                                let _ = tx.send(data);
                            }
                        }
                        rpc::StreamFrame::Error(err) => {
                            pending_stream_clone.write().await.remove(&err.id);
                            pending_unary_clone.write().await.remove(&err.id);
                        }
                        _ => {}
                    }
                } else {
                    tracing::error!("Failed to decode frame for peer {}", peer_id);
                    break;
                }
            }

            // connection close, clean
            tracing::info!("Cleaning up pending calls for peer {}", peer_id);
            pending_stream_clone.write().await.clear();
            pending_unary_clone.write().await.clear();
        });

        Self {
            writer,
            pending_stream_calls,
            pending_unary_calls,
            _read_task: Arc::new(read_task),
        }
    }

    async fn is_alive(&self) -> bool {
        let mut w = self.writer.lock().await;
        w.flush().await.is_ok()
    }

    async fn send_request(&self, request_id: String, method: String, data: Vec<u8>) -> Result<()> {
        let mut w = self.writer.lock().await;

        // send initial request
        let initial_request = rpc::StreamRequest {
            id: request_id.clone(),
            method,
            data: Arc::new([]),
        };
        let request_frame = rpc::StreamFrame::Request(initial_request);
        let request_data = bincode::encode_to_vec(&request_frame, standard())?;
        let len = request_data.len() as u32;
        // write len frame
        w.write_all(&len.to_be_bytes()).await?;
        w.write_all(&request_data).await?;
        w.flush().await?;

        // write data frame
        let chunk_size = 16 * 1024 * 1024; // 16MB peer frame
        let total_chunks = if data.is_empty() { 0 } else { (data.len() + chunk_size - 1) / chunk_size };
        let mut frame_buffer = Vec::with_capacity(64 * 1024 + 1024);

        if total_chunks == 0 {
            let message = rpc::StreamMessage {
                id: request_id.clone(),
                data: Cow::Borrowed(&[]),
                is_end: true,
            };
            frame_buffer.clear();
            bincode::encode_into_std_write(&rpc::StreamFrame::Data(message), &mut frame_buffer, standard())?;
            let frame_len = frame_buffer.len() as u32;
            w.write_all(&frame_len.to_be_bytes()).await?;
            w.write_all(&frame_buffer).await?;
            w.flush().await?;
        } else {
            for index in 0..total_chunks {
                let start = index * chunk_size;
                let end = std::cmp::min(start + chunk_size, data.len());
                let is_last = index == total_chunks - 1;

                let message = rpc::StreamMessage {
                    id: request_id.clone(),
                    data: Cow::Borrowed(&data[start..end]),
                    is_end: is_last,
                };

                frame_buffer.clear();
                bincode::encode_into_std_write(&rpc::StreamFrame::Data(message), &mut frame_buffer, standard())?;
                let frame_len = frame_buffer.len() as u32;
                w.write_all(&frame_len.to_be_bytes()).await?;
                w.write_all(&frame_buffer).await?;
                w.flush().await?;
            }
        }
        Ok(())
    }

    pub async fn cancel_request(&self, request_id: &str) -> Result<()> {
        // send cancel frame
        let cancel_frame = rpc::StreamFrame::Cancel(request_id.to_string());
        let encoded = bincode::encode_to_vec(&cancel_frame, standard())?;
        let len = (encoded.len() as u32).to_be_bytes();

        let mut writer = self.writer.lock().await;
        writer.write_all(&len).await?;
        writer.write_all(&encoded).await?;
        writer.flush().await?;
        drop(writer);

        // clean pending request
        self.pending_stream_calls.write().await.remove(request_id);
        Ok(())
    }

    async fn register_unary_call(&self, request_id: String, tx: oneshot::Sender<Vec<u8>>) {
        let mut pending = self.pending_unary_calls.write().await;
        pending.insert(request_id, (Vec::new(), tx));
    }

    async fn register_stream_call(&self, request_id: String, tx: mpsc::Sender<Vec<u8>>) {
        let mut pending = self.pending_stream_calls.write().await;
        pending.insert(request_id, tx);
    }
}

impl Lattica {
    pub fn builder() -> LatticaBuilder {
        LatticaBuilder::new()
    }

    pub fn peer_id(&self) -> PeerId {
        self.config.keypair.public().to_peer_id()
    }

    pub async fn get_record(&self, key:RecordKey,  quorum: Quorum) -> Result<Vec<PeerRecord>>{
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::GetRecord(key, quorum, tx))?;
        rx.await?
    }

    pub async fn put_record(&self, record: Record, quorum: Quorum) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::PutRecord(record, quorum, tx))?;
        rx.await?
    }

    pub async fn store_simple(&self, key: &str, value: Vec<u8>, expiration_time: f64) -> Result<()> {
        let record_key = RecordKey::new(&key.to_string());

        let value_with_time = common::ValueWithTime {
            value,
            expiration_time,
        };

        let serialized = bincode::encode_to_vec(&value_with_time, standard())?;
        let mut record = Record::new(record_key, serialized);
        record.publisher = Some(self.peer_id());
        record.expires = Some(Instant::now() + Duration::from_secs((expiration_time - common::get_current_timestamp()) as u64));

        self.put_record(record, Quorum::One).await
    }

    pub async fn store_subkey(&self, key: &str, subkey: &str, value: Vec<u8>, expiration_time: f64) -> Result<()> {
        let subkey_full = format!("{}#{}", key, subkey);
        let record_key = RecordKey::new(&subkey_full);

        let value_with_time = common::ValueWithTime {
            value,
            expiration_time,
        };

        let serialized = bincode::encode_to_vec(&value_with_time, standard())?;
        let mut record = Record::new(record_key, serialized);
        record.publisher = Some(self.peer_id());
        record.expires = Some(Instant::now() + Duration::from_secs((expiration_time - common::get_current_timestamp()) as u64));

        self.put_record(record, Quorum::One).await?;

        // update subkey
        self.update_subkey_index(key, subkey, expiration_time).await?;

        Ok(())
    }

    async fn update_subkey_index(&self, key: &str, subkey: &str, expiration_time: f64) -> Result<()> {
        let index_key = format!("{}#index", key);
        let record_key = RecordKey::new(&index_key);

        // try to find index
        let mut index = match self.get_record(record_key.clone(), Quorum::One).await {
            Ok(records) if !records.is_empty() => {
                if let Ok((existing_index, _)) = bincode::decode_from_slice::<common::types::SubkeyIndex, _>(&records[0].record.value, standard()) {
                    existing_index
                } else {
                    common::types::SubkeyIndex {
                        subkeys: vec![],
                        expiration: expiration_time,
                    }
                }
            }
            _ => common::types::SubkeyIndex {
                subkeys: vec![],
                expiration: expiration_time,
            }
        };

        // update index
        if !index.subkeys.contains(&subkey.to_string()) {
            index.subkeys.push(subkey.to_string());
        }
        index.expiration = index.expiration.max(expiration_time);

        // store index after updated
        let serialized = bincode::encode_to_vec(&index, standard())?;
        let mut record = Record::new(record_key, serialized);
        record.publisher = Some(self.peer_id());
        record.expires = Some(Instant::now() + Duration::from_secs((expiration_time - common::time::get_current_timestamp()) as u64));

        self.put_record(record, Quorum::One).await
    }

    pub async fn get_with_subkey(&self, key: &str) -> Result<Option<common::types::DhtValue>> {
        // try to get subkey index
        let index_key = format!("{}#index", key);
        let index_record_key = RecordKey::new(&index_key);

        match self.get_record(index_record_key, Quorum::One).await {
            Ok(records) if !records.is_empty() => {
                // subkey index exist, get all subkey
                if let Ok((index, _)) = bincode::decode_from_slice::<common::types::SubkeyIndex, _>(&records[0].record.value, standard()) {
                    let mut subkey_values = Vec::new();

                    for subkey in index.subkeys {
                        let subkey_full = format!("{}#{}", key, subkey);
                        let subkey_record_key = RecordKey::new(&subkey_full);

                        if let Ok(subkey_records) = self.get_record(subkey_record_key, Quorum::One).await {
                            if !subkey_records.is_empty() {
                                if let Ok((value_with_time, _)) =
                                    bincode::decode_from_slice::<common::types::ValueWithTime, _>(&subkey_records[0].record.value, standard()) {
                                    // check expiration
                                    if value_with_time.expiration_time > common::time::get_current_timestamp() {
                                        subkey_values.push((subkey, value_with_time.value, value_with_time.expiration_time));
                                    }
                                }
                            }
                        }
                    }

                    if !subkey_values.is_empty() {
                        return Ok(Some(common::types::DhtValue::WithSubkeys { subkeys: subkey_values }));
                    }
                }
            }
            _ => {}
        }

        // subkey index not exist, try to get simple value
        let record_key = RecordKey::new(&key.to_string());
        match self.get_record(record_key, Quorum::One).await {
            Ok(records) if !records.is_empty() => {
                if let Ok((value_with_time, _)) =
                    bincode::decode_from_slice::<common::types::ValueWithTime, _>(&records[0].record.value, standard()) {
                    // check expiration
                    if value_with_time.expiration_time > common::time::get_current_timestamp() {
                        return Ok(Some(common::types::DhtValue::Simple {
                            value: value_with_time.value,
                            expiration: value_with_time.expiration_time,
                        }));
                    }
                }
            }
            _ => {}
        }
        Ok(None)
    }

    pub async fn get_visible_maddrs(&self) -> Result<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::GetVisibleMAddrs(tx))?;
        rx.await?
    }

    pub async fn connect(&self, peer_id: PeerId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RpcConnect(peer_id, tx))?;
        rx.await?
    }

    pub async fn dial(&mut self, addr: Multiaddr) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::Dial(addr, tx))?;
        rx.await?
    }

    pub async fn call(&self, peer_id: PeerId, method: String, data: Vec<u8>) -> Result<rpc::RpcResponse> {
        let (tx, rx) = oneshot::channel();

        let compression_algo = if should_compress(data.len(), self.config.compression_algorithm) {
            Some(self.config.compression_algorithm)
        } else {
            None
        };

        let (final_data, final_compression) = if let Some(algo) = compression_algo {
            match compress_data(&data, algo, self.config.compression_level).await {
                Ok(compressed_data) => {
                    (compressed_data, Some(algo))
                },
                Err(e) => {
                    tracing::warn!("Failed to compress data: {}", e);
                    (data, None)
                }
            }
        } else {
            (data, None)
        };

        let request = rpc::RpcRequest {
            id: Uuid::new_v4().to_string(),
            method,
            data: final_data,
            compression: final_compression,
        };
        self.cmd.try_send(Command::RpcSend(peer_id, request, tx))?;
        rx.await?
    }

    pub async fn call_stream(&self, peer_id: PeerId, method: String, data: Vec<u8>) -> Result<Vec<u8>> {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RpcGetOrCreateStreamHandle(peer_id, tx))?;
        let handle = rx.await??;

        // send request
        let (response_tx, response_rx) = oneshot::channel();
        handle.register_unary_call(request_id.clone(), response_tx).await;
        handle.send_request(request_id, method, data).await?;

        // get response
        let result = response_rx.await
            .map_err(|_| anyhow!("Response channel closed"))?;

        Ok(result)
    }

    pub async fn call_stream_iter(&self, peer_id: PeerId, method: String, data: Vec<u8>) -> Result<(String, mpsc::Receiver<Vec<u8>>)> {
        let request_id = Uuid::new_v4().to_string();
        // get stream handle
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RpcGetOrCreateStreamHandle(peer_id, tx))?;
        let handle = rx.await??;

        // send request
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(512);
        handle.register_stream_call(request_id.clone(), out_tx).await;
        handle.send_request(request_id.clone(), method, data).await?;

        Ok((request_id, out_rx))
    }

    pub async fn cancel_stream_iter(&self, peer_id: PeerId, request_id: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RpcGetOrCreateStreamHandle(peer_id, tx))?;

        match rx.await {
            Ok(Ok(handle)) => {
                handle.cancel_request(&request_id).await?;
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow!("Failed to get stream handle: {:?}", e)),
        }
    }

    pub async fn register_service(&self, service: Box<dyn rpc::RpcService>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RpcRegister(service, tx))?;
        rx.await?
    }

    pub async fn register_to_rendezvous(&mut self, rendezvous_peer: PeerId, namespace: Option<String>, ttl: Option<u64>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RendezvousRegister(rendezvous_peer, namespace, ttl, tx))?;
        rx.await?
    }

    pub async fn discover_from_rendezvous(&mut self, rendezvous_peer: PeerId, namespace: Option<String>, limit: Option<u64>) -> Result<Vec<PeerId>> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::RendezvousDiscover(rendezvous_peer, namespace, limit, tx))?;
        rx.await?
    }

    pub fn get_rendezvous_servers(&self) -> &Vec<(PeerId, Multiaddr)> {
        &self.config.rendezvous_servers
    }

    pub async fn auto_register_to_rendezvous(&mut self, namespace: Option<String>, ttl: Option<u64>) -> Result<()> {
        let servers = self.config.rendezvous_servers.clone();
        for (peer_id, _addr) in servers {
            if let Err(e) = self.register_to_rendezvous(peer_id, namespace.clone(), ttl).await {
                tracing::warn!("Failed to register to rendezvous server {}: {}", peer_id, e);
            } else {
                tracing::info!("Successfully registered to rendezvous server {}", peer_id);
            }
        }
        Ok(())
    }

    pub async fn auto_discover_from_rendezvous(&mut self, namespace: Option<String>, limit: Option<u64>) -> Result<Vec<PeerId>> {
        let mut all_peers = Vec::new();
        let servers = self.config.rendezvous_servers.clone();
        
        for (peer_id, _addr) in servers {
            match self.discover_from_rendezvous(peer_id, namespace.clone(), limit).await {
                Ok(mut peers) => {
                    tracing::info!("Discovered {} addressbook from rendezvous server {}", peers.len(), peer_id);
                    all_peers.append(&mut peers);
                }
                Err(e) => {
                    tracing::warn!("Failed to discover from rendezvous server {}: {}", peer_id, e);
                }
            }
        }
        
        // Remove duplicates
        all_peers.sort();
        all_peers.dedup();
        
        Ok(all_peers)
    }

    pub async fn get_peer_info(&self, peer_id: &PeerId) -> Option<PeerInfo> {
        let address_book = self.address_book.read().await;
        address_book.info(peer_id)
    }

    pub async fn get_all_peers(&self) -> Vec<PeerId> {
        let address_book = self.address_book.read().await;
        address_book.peers()
    }

    pub async fn get_peer_addresses(&self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let address_book = self.address_book.read().await;
        address_book.get_peer_addresses(peer_id)
    }

    pub async fn get_peer_rtt(&self, peer_id: &PeerId) -> Option<Duration> {
        let address_book = self.address_book.read().await;
        address_book.info(peer_id)?.rtt()
    }

    pub async fn get_block(&self, cid: &Cid) -> Result<BytesBlock> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::Get(*cid, tx))?;
        rx.await?
    }

    pub async fn put_block(&self, block: &BytesBlock) -> Result<Cid> {
        let cid = block.cid()?;
        self.storage.put_keyed(&cid, block.data()).await?;
        Ok(cid)
    }

    pub async fn remove_block(&self, cid: &Cid) -> Result<()> {
        self.storage.remove(&cid).await?;
        Ok(())
    }

    pub async fn start_providing(&self, record_key: RecordKey) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::StartProviding(record_key, tx))?;
        rx.await?
    }

    pub async fn get_providers(&self, record_key: RecordKey) -> Result<Vec<PeerId>> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::GetProviders(record_key, tx))?;
        rx.await?
    }

    pub async fn stop_providing(&self, record_key: RecordKey) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd.try_send(Command::StopProviding(record_key, tx))?;
        rx.await?
    }
}


async fn swarm_poll(
    cmd_rx: mpsc::Receiver<Command>,
    mut swarm: Swarm<LatticaBehaviour>,
    config: Config,
    address_book: Arc<RwLock<AddressBook>>,
) {
    let services = Arc::new(RwLock::new(HashMap::<String, Box<dyn rpc::RpcService>>::new()));
    let mut pending_requests: HashMap<OutboundRequestId, oneshot::Sender<Result<rpc::RpcResponse>>> = HashMap::new();
    let mut stream_control = swarm.behaviour().stream.new_control();
    let mut incoming_streams =stream_control.accept(rpc::RPC_STREAM_PROTOCOL).unwrap();
    let mut stream_handles: HashMap<PeerId, Arc<StreamHandle>> = HashMap::new();

    let services_stream = services.clone();

    tokio::spawn(async move {
        while let Some((_, stream)) = incoming_streams.next().await {
            let services_clone = services_stream.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming_stream(stream, services_clone).await {
                    tracing::error!("failed to handle incoming stream: {}", e);
                }
            });
        }
    });

    if swarm.behaviour().is_kad_enabled() {
        if let Some(kad) = swarm.behaviour_mut().kad.as_mut() {
            kad.set_mode(Some(Mode::Server));
        }
    }

    let mut queries = FnvHashMap::<QueryId, QueryChannel>::default();
    let mut cmd_rx = ReceiverStream::new(cmd_rx);

    loop {
        match future::select(
            future::poll_fn(|cx| {
                swarm.poll_next_unpin(cx)
            }),
            cmd_rx.next(),
        ).await {
            Either::Left((None, _)) => {
                tracing::info!("poll_swarm: swarm stream ended, terminating");
                return;
            }
            Either::Left((Some(e), _)) => match e {
                SwarmEvent::NewListenAddr { address, .. } => {
                    tracing::info!("Listening on: {}", address.with_p2p(swarm.local_peer_id().clone()).unwrap());
                }
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, connection_id, .. } => {
                    let remote_addr = endpoint.get_remote_address();
                    let remote_protocol = common::get_transport_protocol(remote_addr);
                    let is_direct = !endpoint.get_remote_address().to_string().contains("p2p-circuit");

                    tracing::info!("Connection established with peer: {}, via: {}, use: {:?}, is_direct: {}", peer_id, remote_addr, remote_protocol, is_direct);
                    
                    // update address book
                    let mut address_book_guard = address_book.write().await;
                    let source = if endpoint.is_dialer() {
                        AddressSource::Dial
                    } else {
                        AddressSource::Incoming
                    };
                    address_book_guard.add_address(&peer_id, remote_addr.clone(), source, endpoint.is_relayed(), connection_id);
                    address_book_guard.set_last_seen(&peer_id, Utc::now());

                    // check nat travel success, remove relayed address
                    if !endpoint.is_relayed() {
                        if let Some(info) = address_book_guard.info(&peer_id) {
                            for (_, _, _, relayed, cid) in info.addresses() {
                                if relayed {
                                    swarm.close_connection(cid);
                                }
                            }
                        }
                    }

                    tracing::debug!("Added connection info to AddressBook for peer {}", peer_id);
                }
                SwarmEvent::ConnectionClosed { peer_id,.. } => {
                    tracing::debug!("swarm stream closed, terminating {:?}", e);
                    swarm.behaviour_mut().connection_closed(peer_id);
                }
                SwarmEvent::Behaviour(event) => {
                    match event {
                        LatticaBehaviourEvent::Identify(identify_event) => {
                            let mut address_book_guard = address_book.write().await;
                            handle_identity_event(&config, identify_event, &mut swarm, &mut address_book_guard).await;
                        }
                        LatticaBehaviourEvent::Relay(relay_event) => {
                            handle_relay_event(&config, &mut swarm, relay_event).await;
                        }
                        LatticaBehaviourEvent::Dcutr(dcutr_event) => {
                            tracing::debug!("Dcutr event: {:?}", dcutr_event);
                        }
                        LatticaBehaviourEvent::RequestResponse(request_event) => {
                            handle_request_event(request_event, services.clone(), &mut swarm, &mut pending_requests, &config).await;
                        }
                        LatticaBehaviourEvent::Ping(ping_event) => {
                            tracing::debug!("network::Ping {:?}", ping_event);
                            match &ping_event.result {
                                Ok(rtt) => {
                                    tracing::debug!("Ping success: peer = {:?}, RTT = {:?}", ping_event.peer, rtt);
                                    // update rtt info
                                    let mut address_book_guard = address_book.write().await;
                                    address_book_guard.set_rtt(&ping_event.peer, Some(*rtt));
                                    address_book_guard.set_last_seen(&ping_event.peer, Utc::now());
                                    tracing::debug!("Updated RTT for peer {}: {:?}", ping_event.peer, rtt);
                                }
                                Err(e) => {
                                    tracing::debug!("Ping failed: peer = {:?}, error = {:?}", ping_event.peer, e);
                                    // record ping failed
                                    let mut address_book_guard = address_book.write().await;
                                    address_book_guard.set_rtt(&ping_event.peer, None);
                                    tracing::warn!("Ping failed for peer {}: {:?}", ping_event.peer, e);
                                }
                            }
                        }
                        LatticaBehaviourEvent::Kad(kad_event) => {
                            tracing::debug!("kademlia event {:?}", kad_event);
                            handle_kad_event(kad_event, &mut queries).await
                        },
                        LatticaBehaviourEvent::Rendezvous(rendezvous_event) => {
                            tracing::debug!("rendezvous client event {:?}", rendezvous_event);
                            if let Some(discovered_peers) = handle_rendezvous_client_event(rendezvous_event, &mut queries).await {
                                // Auto-connect to discovered addressbook
                                for (peer_id, addresses) in discovered_peers {
                                    if !addresses.is_empty() {
                                        // Try to connect using the first address with peer ID
                                        let first_addr = &addresses[0];
                                        let addr_with_peer = if first_addr.to_string().contains(&peer_id.to_string()) {
                                            first_addr.clone()
                                        } else {
                                            first_addr.clone().with(Protocol::P2p(peer_id))
                                        };
                                        
                                        tracing::debug!("Auto-connecting to discovered peer {} at {}", peer_id, addr_with_peer);
                                        if let Err(e) = swarm.dial(addr_with_peer) {
                                            tracing::warn!("Failed to dial discovered peer {}: {}", peer_id, e);
                                        }
                                    }
                                }
                            }
                        }
                        LatticaBehaviourEvent::Mdns(mdns_event) => {
                            tracing::debug!("mdns event {:?}", mdns_event);
                            handle_mdns_event(mdns_event, &mut swarm).await;
                        }
                        LatticaBehaviourEvent::Upnp(upnp_event) => {
                            tracing::debug!("upnp event {:?}", upnp_event);
                            handle_upnp_event(upnp_event, &mut swarm).await;
                        }
                        LatticaBehaviourEvent::Gossipsub(gossipsub_event) => {
                            tracing::debug!("gossipsub event {:?}", gossipsub_event);
                            handle_gossipsub_event(gossipsub_event, &mut swarm).await;
                        }
                        LatticaBehaviourEvent::Bitswap(bitswap_event) => {
                            tracing::info!("bitswap event {:?}", bitswap_event);
                            handle_bitswap_event(bitswap_event, &mut queries).await;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            Either::Right((None, _)) => {
                tracing::debug!("poll_swarm: command sender dropped, terminating");
                return;
            }
            Either::Right((Some(cmd), _)) => match cmd {
                Command::GetRecord(key, quorum, tx) => {
                    swarm.behaviour_mut().get_record(key, quorum, tx, &mut queries);
                }
                Command::PutRecord(record, quorum, tx) => {
                    swarm.behaviour_mut().put_record(record, quorum, tx, &mut queries);
                }
                Command::RpcConnect(peer_id, response) => {
                    let result = swarm.dial(peer_id).map_err(|e| anyhow!(e.to_string()));
                    let _ = response.send(result);
                }
                Command::Dial(addr, response) => {
                    let result = swarm.dial(addr).map_err(|e| anyhow!(e.to_string()));
                    let _ = response.send(result);
                }
                Command::RpcSend(peer_id, request, response) => {
                    // check direct connection
                    if !common::has_direct_connection(&address_book, &peer_id).await {
                        let _ = response.send(Err(anyhow!("no direct connection for peer {}", peer_id)));
                        continue
                    }
                    
                    let message = rpc::RpcMessage::Request(request);
                    let request_id = swarm.behaviour_mut().request_response.send_request(&peer_id, message);
                    pending_requests.insert(request_id, response);
                }
                Command::RpcGetOrCreateStreamHandle(peer_id, response) => {
                    // check direct connection
                    if !common::has_direct_connection(&address_book, &peer_id).await {
                        stream_handles.remove(&peer_id);
                        let _ = response.send(Err(anyhow!("no direct connection for peer {}", peer_id)));
                        continue
                    }

                    // check stream handle
                    if let Some(handle) = stream_handles.get(&peer_id) {
                        if handle.is_alive().await {
                            let _ = response.send(Ok(handle.clone()));
                            continue
                        } else {
                            stream_handles.remove(&peer_id);
                        }
                    }

                    // create stream handle
                    let stream_control = swarm.behaviour().stream.new_control();

                    match handle_stream_request(peer_id, stream_control).await {
                        Ok(stream) => {
                            let handle = Arc::new(StreamHandle::new(stream, peer_id));
                            stream_handles.insert(peer_id, handle.clone());
                            let _ = response.send(Ok(handle));
                        }
                        Err(e) => {
                            let _ = response.send(Err(e));
                        }
                    }
                }
                Command::RpcRegister(service, response) => {
                    let service_name = service.service_name().to_string();
                    let mut services_guard = services.write().await;
                    services_guard.insert(service_name.clone(), service);
                    let _ = response.send(Ok(()));
                }
                Command::RendezvousRegister(rendezvous_peer, namespace, ttl, response) => {
                    swarm.behaviour_mut().register_to_rendezvous(rendezvous_peer, namespace, ttl, response, &mut queries);
                }
                Command::RendezvousDiscover(rendezvous_peer, namespace, limit, response) => {
                    swarm.behaviour_mut().discover_from_rendezvous(rendezvous_peer, namespace, limit, response, &mut queries);
                }
                Command::GetVisibleMAddrs(tx) => {
                    let result = swarm.external_addresses()
                        .map(|addr| addr.clone())
                        .collect::<Vec<Multiaddr>>();
                    let _ = tx.send(Ok(result));
                }
                Command::Get(cid, tx) => {
                    swarm.behaviour_mut().get(cid, &mut queries, tx);
                }
                Command::StartProviding(key, tx) => {
                    swarm.behaviour_mut().start_providing(key, tx, &mut queries);
                }
                Command::GetProviders(key, tx) => {
                    swarm.behaviour_mut().get_providers(key, tx, &mut queries);
                }
                Command::StopProviding(key, tx) => {
                    swarm.behaviour_mut().stop_providing(key, tx);
                }
            }
        }
    }
}