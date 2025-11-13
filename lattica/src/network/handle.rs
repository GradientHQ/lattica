use super::{*};
use crate::{common, rpc};
use std::{sync::Arc};
use anyhow::{Result, anyhow};
use libp2p::{Swarm, identify,
             kad::{Event, BootstrapOk, GetRecordOk,
             PeerRecord, PutRecordOk, QueryResult}, Multiaddr, multiaddr::{Protocol},
             PeerId, request_response::{OutboundRequestId, ResponseChannel}, Stream, request_response,
             rendezvous, mdns, upnp, relay, gossipsub};
use futures::{AsyncReadExt, AsyncWriteExt};
use std::borrow::Cow;
use std::collections::HashMap;
use bincode::config::standard;
use fnv::{FnvHashMap};
use libp2p_stream::Control;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use crate::common::{compress_data, decompress_data, should_compress, BytesBlock, QueryId, P2P_CIRCUIT_TOPIC};
use libp2p::kad::{AddProviderOk, GetProvidersOk};
use std::sync::atomic::{AtomicBool, Ordering};
use futures::io::{WriteHalf};

pub(crate) async fn handle_incoming_stream(stream: Stream, services: Arc<RwLock<HashMap<String, Box<dyn rpc::RpcService>>>>) -> Result<()> {
    // split writer and reader
    let (mut reader, writer) = stream.split();
    let writer = Arc::new(Mutex::new(writer));
    let cancel_flags: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>> = Arc::new(RwLock::new(HashMap::new()));

    // channel for pass frame
    let (frame_tx, mut frame_rx) = mpsc::channel::<rpc::StreamFrame>(32);

    let frame_tx_clone = frame_tx.clone();
    tokio::spawn(async move {
        let mut buf = Vec::with_capacity(8 * 1024);
        loop {
            let mut len_buf = [0u8; 4];
            if reader.read_exact(&mut len_buf).await.is_err() {
                break;
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            buf.resize(len, 0);
            if reader.read_exact(&mut buf).await.is_err() {
                break;
            }

            if let Ok((frame, _)) = bincode::decode_from_slice::<rpc::StreamFrame, _>(&buf, standard()) {
                if frame_tx_clone.send(frame).await.is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    });

    struct RequestState {
        data_tx: mpsc::Sender<Vec<u8>>,
    }
    let mut pending_requests: HashMap<String, RequestState> = HashMap::new();

    while let Some(frame) = frame_rx.recv().await {
        match frame {
            rpc::StreamFrame::Cancel(id) => {
                pending_requests.remove(&id);

                if let Some(flag) = cancel_flags.read().await.get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            rpc::StreamFrame::Request(req) => {
                let request_id = req.id.clone();
                let request_method = req.method.clone();

                let parts: Vec<&str> = request_method.split('.').collect();
                if parts.len() != 2 {
                    let error_frame = rpc::StreamFrame::Error(rpc::StreamError{
                        id: request_id.clone(),
                        error: "Invalid method format".to_string()
                    });
                    let _ = send_frame(&writer, error_frame).await;
                    continue;
                }

                let service_name = parts[0].to_string();
                let method_name = parts[1].to_string();

                // create data channel
                let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(16);

                pending_requests.insert(request_id.clone(), RequestState {
                    data_tx,
                });

                let services_clone = services.clone();
                let writer_clone = writer.clone();
                let cancel_flags_clone = cancel_flags.clone();
                let request_id_clone = request_id.clone();

                tokio::spawn(async move {
                    let mut complete_data = Vec::with_capacity(64 * 1024);
                    while let Some(chunk) = data_rx.recv().await {
                        complete_data.extend_from_slice(&chunk);
                    }
                    let services_guard = services_clone.read().await;
                    if let Some(service) = services_guard.get(&service_name) {
                        let complete_request = rpc::StreamRequest {
                            id: request_id_clone.clone(),
                            method: request_method.clone(),
                            data: Arc::from(complete_data),
                        };

                        match service.handle_stream_iter(&method_name, complete_request.clone()).await {
                            Ok(Some(mut rx)) => {
                                let cancel_flag = Arc::new(AtomicBool::new(false));
                                cancel_flags_clone.write().await.insert(request_id_clone.clone(), cancel_flag.clone());

                                while let Some(item) = rx.recv().await {
                                    // check cancel
                                    if cancel_flag.load(Ordering::Relaxed) {
                                        drop(rx);
                                        let close_frame = rpc::StreamFrame::Close(request_id_clone.clone());
                                        let _ = send_frame(&writer_clone, close_frame).await;
                                        cancel_flags_clone.write().await.remove(&request_id_clone);
                                        break;
                                    }

                                    let frame = rpc::StreamFrame::Data(rpc::StreamMessage {
                                        id: request_id_clone.clone(),
                                        data: Cow::Borrowed(&item),
                                        is_end: false,
                                        });
                                    if send_frame(&writer_clone, frame).await.is_err() {
                                        break;
                                    }
                                }
                                if !cancel_flag.load(Ordering::Relaxed) {
                                    let close_frame = rpc::StreamFrame::Close(request_id_clone.clone());
                                    let _ = send_frame(&writer_clone, close_frame).await;
                                    cancel_flags_clone.write().await.remove(&request_id_clone);
                                }
                            }
                            _ => {
                                match service.handle_stream(&method_name, complete_request).await {
                                    Ok(stream_response) => {
                                        let chunk_size = 16 * 1024 * 1024;
                                        let total_chunks = (stream_response.data.len() + chunk_size - 1) / chunk_size;

                                        for index in 0..total_chunks {
                                            let start = index * chunk_size;
                                            let end = std::cmp::min(start + chunk_size, stream_response.data.len());
                                            let chunk = &stream_response.data[start..end];
                                            let is_last = index == total_chunks - 1;

                                            let stream_message = rpc::StreamMessage {
                                                id: stream_response.id.clone(),
                                                data: Cow::Borrowed(chunk),
                                                is_end: is_last,
                                            };

                                            let frame = rpc::StreamFrame::Data(stream_message);
                                            if send_frame(&writer_clone, frame).await.is_err() {
                                                break;
                                            }
                                        }

                                        let close_frame = rpc::StreamFrame::Close(stream_response.id.clone());
                                        let _ = send_frame(&writer_clone, close_frame).await;
                                    }
                                    Err(error) => {
                                        let error_frame = rpc::StreamFrame::Error(rpc::StreamError {
                                            id: request_id_clone.clone(),
                                            error
                                        });
                                        let _ = send_frame(&writer_clone, error_frame).await;
                                        let close_frame = rpc::StreamFrame::Close(request_id_clone.clone());
                                        let _ = send_frame(&writer_clone, close_frame).await;
                                    }
                                }
                            }
                        }
                    } else {
                        let error_frame = rpc::StreamFrame::Error(rpc::StreamError{
                            id: request_id_clone.clone(),
                            error: format!("Service {} not found", service_name)
                        });
                        let _ = send_frame(&writer_clone, error_frame).await;
                    }
                });
            }
            rpc::StreamFrame::Data(msg) => {
                if let Some(state) = pending_requests.get(&msg.id) {
                    let _ = state.data_tx.send(msg.data.to_vec()).await;

                    // end close
                    if msg.is_end {
                        pending_requests.remove(&msg.id);
                    }
                }
            }
            rpc::StreamFrame::Error(err) => {
                tracing::error!("Received error frame: {:?}", err);
                pending_requests.remove(&err.id);
            }
            _ => break,
        }
    }
    Ok(())
}

async fn send_frame<'a>(writer: &Arc<Mutex<WriteHalf<Stream>>>, frame: rpc::StreamFrame<'a>) -> Result<()> {
    let data = bincode::encode_to_vec(&frame, standard())?;
    let len = data.len() as u32;

    let mut w = writer.lock().await;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&data).await?;
    w.flush().await?;
    Ok(())
}

pub(crate) async fn handle_request_event(event: request_response::Event<rpc::RpcMessage, rpc::RpcMessage, rpc::RpcMessage>,
                              services: Arc<RwLock<HashMap<String, Box<dyn rpc::RpcService>>>>,
                              swarm: &mut Swarm<LatticaBehaviour>,
                              pending_requests: &mut HashMap<OutboundRequestId, oneshot::Sender<Result<rpc::RpcResponse>>>,
                              config: &Config) {
    match event {
        request_response::Event::Message {message, ..} => {
            match message {
                request_response::Message::Request {request, channel, ..} => {
                    {
                        let swarm_temp = swarm;
                        handle_rpc_request(request, channel, services, swarm_temp, config).await;
                    }
                }
                request_response::Message::Response {response, request_id, ..} => {
                    if let Some(tx) = pending_requests.remove(&request_id) {
                        match response {
                            rpc::RpcMessage::Response(resp) => {
                                let _ = tx.send(Ok(resp));
                            }
                            rpc::RpcMessage::Error(err) => {
                                let _ = tx.send(Err(anyhow!(err.error)));
                            }
                            _ => {
                                let _ = tx.send(Err(anyhow!("Unexpected response type")));
                            }
                        }
                    }
                }
            }
        }
        request_response::Event::OutboundFailure {request_id, error, ..} => {
            if let Some(tx) = pending_requests.remove(&request_id) {
                let _ = tx.send(Err(anyhow!("Outbound failure: {:?}", error)));
            }
        }
        request_response::Event::InboundFailure { error, ..} => {
            tracing::error!("Inbound failure: {:?}", error);
        }
        _ => {}
    }
}

pub(crate) async fn handle_kad_event(event: Event, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
    match event {
        Event::RoutingUpdated {peer,..} => {
            tracing::debug!("Routing updated {:?}", peer);
        }
        Event::OutboundQueryProgressed {id, result, ..} => {
            match result {
                QueryResult::Bootstrap(Ok(BootstrapOk{..})) => {
                }
                QueryResult::Bootstrap(Err(_)) => {
                }
                QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(PeerRecord { record, .. }))) => {
                    if let Some(QueryChannel::GetRecord(expected_results, mut records, tx)) = queries.remove(&id.into()) {
                        records.push(PeerRecord { peer: None, record });
                        if records.len() >= expected_results {
                            let _ = tx.send(Ok(records));
                        } else {
                            queries.insert(id.into(), QueryChannel::GetRecord(expected_results, records, tx));
                        }
                    }
                }
                QueryResult::GetRecord(Err(err)) => {
                    tracing::debug!("get record error: {:?}", err);
                    if let Some(QueryChannel::GetRecord(_, _, ch)) = queries.remove(&id.into()) {
                        ch.send(Err(anyhow!("get record error: {:?}", err))).ok();
                    }
                }
                QueryResult::PutRecord(Ok(PutRecordOk{..})) => {
                    if let Some(QueryChannel::PutRecord(ch)) = queries.remove(&id.into()) {
                        ch.send(Ok(())).ok();
                    }
                }
                QueryResult::PutRecord(Err(err)) => {
                    let err_str = format!("{:?}", err);
                    if err_str.contains("QuorumFailed") || err_str.contains("NoKnownPeers") {
                        if let Some(QueryChannel::PutRecord(ch)) = queries.remove(&id.into()) {
                            ch.send(Ok(())).ok();
                        }
                        return;
                    }

                    tracing::warn!("put record error: {:?}", err);
                    if let Some(QueryChannel::PutRecord(ch)) = queries.remove(&id.into()) {
                        ch.send(Err(anyhow!("put record error: {:?}", err))).ok();
                    }

                }
                QueryResult::StartProviding(Ok(AddProviderOk{..})) => {
                    if let Some(QueryChannel::StartProviding(ch)) = queries.remove(&id.into()) {
                        ch.send(Ok(())).ok();
                    }
                }
                QueryResult::StartProviding(Err(err)) => {
                    tracing::warn!("add provider error: {:?}", err);
                    if let Some(QueryChannel::StartProviding(ch)) = queries.remove(&id.into()) {
                        ch.send(Err(anyhow!("add provider error: {:?}", err))).ok();
                    }
                }
                QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders{providers, ..})) => {
                    let peers = providers.into_iter().collect();
                    if let Some(QueryChannel::GetProviders(tx)) = queries.remove(&id.into()) {
                        let _ = tx.send(Ok(peers));
                    }
                }
                QueryResult::GetProviders(Err(err)) => {
                    tracing::warn!("get providers error: {:?}", err);
                    if let Some(QueryChannel::GetProviders(ch)) = queries.remove(&id.into()) {
                        ch.send(Err(anyhow!("get providers error: {:?}", err))).ok();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_rpc_request(
    request: rpc::RpcMessage,
    channel: ResponseChannel<rpc::RpcMessage>,
    services: Arc<RwLock<HashMap<String, Box<dyn rpc::RpcService>>>>,
    swarm: &mut Swarm<LatticaBehaviour>,
    config: &Config) {
    match request {
        rpc::RpcMessage::Request(mut req) => {
            let parts: Vec<&str> = req.method.split('.').collect();
            if parts.len() != 2 {
                let error_response = rpc::RpcMessage::Error(rpc::RpcError {
                    id: req.id,
                    error: "Invalid method format".to_string(),
                });
                let _ = swarm.behaviour_mut().request_response.send_response(channel, error_response);
                return;
            }

            let service_name = parts[0];
            let method_name = parts[1];

            if let Some(compression_algo) = req.compression {
                match decompress_data(&req.data, compression_algo).await {
                    Ok(data) => {
                        req.data = data;
                    }
                    Err(e) => {
                        let error_response = rpc::RpcMessage::Error(rpc::RpcError{
                            id: req.id,
                            error: format!("Failed to decompress request data: {}", e),
                        });
                        let _ = swarm.behaviour_mut().request_response.send_response(channel, error_response);
                        return;
                    }
                }
            }

            let services_guard = services.read().await;
            if let Some(service) = services_guard.get(service_name) {
                match service.handle_request(method_name, req.clone()).await {
                    Ok(mut response) => {
                        if should_compress(response.data.len(), config.compression_algorithm) {
                            match compress_data(&response.data, config.compression_algorithm, config.compression_level).await {
                                Ok(data) => {
                                    response.data = data;
                                    response.compression = Some(config.compression_algorithm);
                                },
                                Err(e) => {
                                    tracing::warn!("compress error: {:?}", e);
                                }
                            }
                        }
                        let rpc_response = rpc::RpcMessage::Response(response);
                        let _ = swarm.behaviour_mut().request_response.send_response(channel, rpc_response);
                    }
                    Err(error) => {
                        let error_response = rpc::RpcMessage::Error(rpc::RpcError{
                            id: req.id,
                            error,
                        });
                        let _ = swarm.behaviour_mut().request_response.send_response(channel, error_response);
                    }
                }
            } else {
                let error_response = rpc::RpcMessage::Error(rpc::RpcError{
                    id: req.id,
                    error: format!("Service {} not found", service_name),
                });
                let _ = swarm.behaviour_mut().request_response.send_response(channel, error_response);
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_stream_request(peer_id: PeerId, mut stream_control: Control) -> Result<Stream> {
    let stream = stream_control.open_stream(peer_id, rpc::RPC_STREAM_PROTOCOL).await?;
    Ok(stream)
}

pub(crate) async fn handle_rendezvous_client_event(
    event: rendezvous::client::Event,
    _queries: &mut FnvHashMap<QueryId, QueryChannel>,
) -> Option<Vec<(PeerId, Vec<Multiaddr>)>> {
    match event {
        rendezvous::client::Event::Registered { 
            namespace, 
            ttl, 
            rendezvous_node 
        } => {
            tracing::debug!(
                "Successfully registered to rendezvous server {} in namespace '{}' with TTL {:?}",
                rendezvous_node, namespace, ttl
            );
            None
        }
        rendezvous::client::Event::RegisterFailed { 
            rendezvous_node, 
            namespace, 
            error 
        } => {
            tracing::error!(
                "Failed to register to rendezvous server {} in namespace '{}': {:?}",
                rendezvous_node, namespace, error
            );
            None
        }
        rendezvous::client::Event::Discovered { 
            rendezvous_node, 
            registrations, 
            cookie: _ 
        } => {
            let mut discovered_peers = Vec::new();
            
            for registration in registrations {
                let peer_id = registration.record.peer_id();
                let addresses: Vec<Multiaddr> = registration.record.addresses().iter().cloned().collect();
                
                tracing::debug!(
                    "Discovered peer {} from rendezvous server {} with {} addresses",
                    peer_id, rendezvous_node, addresses.len()
                );
                
                // Add peer and its addresses to the list
                discovered_peers.push((peer_id, addresses));
            }
            
            tracing::debug!(
                "Total discovered {} addressbook from rendezvous server {}",
                discovered_peers.len(), rendezvous_node
            );
            
            if discovered_peers.is_empty() {
                None
            } else {
                Some(discovered_peers)
            }
        }
        rendezvous::client::Event::DiscoverFailed { 
            rendezvous_node, 
            namespace, 
            error 
        } => {
            tracing::error!(
                "Failed to discover from rendezvous server {} in namespace {:?}: {:?}",
                rendezvous_node, namespace, error
            );
            None
        }
        rendezvous::client::Event::Expired { peer } => {
            tracing::debug!("Registration expired for peer {}", peer);
            None
        }
    }
}

pub(crate) async fn handle_mdns_event(event: mdns::Event, swarm: &mut Swarm<LatticaBehaviour>) {
    match event {
        mdns::Event::Discovered(peers) => {
            for (_, addr) in peers {
                swarm.dial(addr).unwrap();
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_upnp_event(event: upnp::Event, swarm: &mut Swarm<LatticaBehaviour>) {
    match event {
        upnp::Event::NewExternalAddr(addr) => {
            swarm.add_external_address(addr)
        }
        upnp::Event::ExpiredExternalAddr(addr) => {
            swarm.remove_external_address(&addr);
        }
        _ => {}
    }
}

pub(crate) async fn handle_identity_event(
    config: &Config, 
    event: identify::Event, 
    swarm: &mut Swarm<LatticaBehaviour>,
    address_book: &mut AddressBook,
) {
    match event {
        identify::Event::Received {peer_id, info, ..} => {
            if !common::is_relay_server(&config.relay_servers, peer_id) && !common::is_relay_server(&config.bootstrap_nodes, peer_id) {
                if config.protocol_version != info.protocol_version {
                    address_book.remove_peer(&peer_id);
                    match swarm.disconnect_peer_id(peer_id) {
                        Ok(()) => {
                            tracing::debug!("protocol version mismatch, disconnect from peer {}", peer_id);
                        },
                        Err(()) => {
                        }
                    }
                    return;
                }
                
                address_book.set_info(&peer_id, info.clone());
                address_book.set_protocol_check(&peer_id, true);
                
                for addr in info.listen_addrs {
                    // add other node address to kad
                    if swarm.behaviour_mut().is_kad_enabled() {
                        swarm.behaviour_mut().kad.as_mut().unwrap().add_address(&peer_id, addr);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_relay_event(config: &Config, swarm: &mut Swarm<LatticaBehaviour>, event: relay::client::Event, pending_relay_addrs: &Arc<RwLock<Vec<Multiaddr>>>) {
    let local_peer_id = swarm.local_peer_id().clone();
    match event {
        relay::client::Event::ReservationReqAccepted {relay_peer_id,..} => {
            for relay_addr in &config.relay_servers {
                if let Some(last) = relay_addr.iter().last() {
                    if last == Protocol::P2p(relay_peer_id) {
                        let after_relay_addr = common::construct_relayed_addr(&relay_addr, &local_peer_id);
                        swarm.add_external_address(after_relay_addr.clone());

                        // broadcast by gossipsub
                        let topic = gossipsub::IdentTopic::new(P2P_CIRCUIT_TOPIC);
                        let ret = swarm.behaviour_mut().gossipsub.publish(topic, after_relay_addr.to_string().into_bytes());
                        match ret {
                            Ok(msgid) => {
                                tracing::debug!("publish gossipsub message success: {:?}",msgid )
                            },
                            Err(gossipsub::PublishError::InsufficientPeers) => {
                                pending_relay_addrs.write().await.push(after_relay_addr.clone());
                            }
                            Err(err) => {
                                tracing::debug!("publish gossipsub message error: {:?}", err)
                            }
                        }

                        tracing::debug!("handle_relay_event add relay circuit address: {:?}", after_relay_addr);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_gossipsub_event(event: gossipsub::Event, swarm: &mut Swarm<LatticaBehaviour>, pending_relay_addrs: &Arc<RwLock<Vec<Multiaddr>>>) {
    match event {
        gossipsub::Event::Subscribed {topic,..} => {
            if topic.as_str() == P2P_CIRCUIT_TOPIC {
                let pending = pending_relay_addrs.read().await;
                if !pending.is_empty() {
                    tracing::debug!("Publishing {} cached relay addresses", pending.len());
                    for addr in pending.iter() {
                        let topic = gossipsub::IdentTopic::new(P2P_CIRCUIT_TOPIC);
                        match swarm.behaviour_mut().gossipsub.publish(topic, addr.to_string().into_bytes()) {
                            Ok(msgid) => {
                                tracing::debug!("Published cached relay address: {}, msgid: {:?}", addr, msgid);
                            },
                            Err(err) => {
                                tracing::error!("Failed to publish gossipsub message: {:?}", err);
                            }
                        }
                    }
                }

                drop(pending);
                pending_relay_addrs.write().await.clear();
            }
        }
        gossipsub::Event::Message {message,..} => {
            let topic = message.topic.as_str();
            tracing::debug!("Gossipsub received message from topic {:?}", topic);
            match topic {
                P2P_CIRCUIT_TOPIC => {
                    if let Ok(addr_str) = std::str::from_utf8(&message.data) {
                        if let Some(source_peer_id) = message.source {
                            if let Ok(addr) = addr_str.parse::<Multiaddr>() {
                                // add to swarm
                                swarm.add_peer_address(source_peer_id, addr.clone());

                                // add other node address to kad
                                if swarm.behaviour_mut().is_kad_enabled() {
                                    swarm.behaviour_mut().kad.as_mut().unwrap().add_address(&source_peer_id, addr.clone());
                                }
                                tracing::debug!("Gossipsub message from {:?} to {:?}", source_peer_id, addr.clone());
                            } else {
                                tracing::error!("Gossipsub message from {:?} is not a Multiaddr {:?}", source_peer_id, addr_str);
                            }
                        } else {
                            tracing::error!("Gossipsub message message.source error")
                        }
                    } else {
                        tracing::error!("Gossipsub std::str::from_utf8 error")
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(crate) async fn handle_bitswap_event(event: beetswap::Event, queries: &mut FnvHashMap<QueryId, QueryChannel>) {
    match event {
        beetswap::Event::GetQueryResponse {query_id, data} => {
            if let Some(QueryChannel::Get(ch)) = queries.remove(&query_id.into()) {
                let block = BytesBlock(data);
                ch.send(Ok(block)).ok();
            }
        },
        
        beetswap::Event::GetQueryError {query_id, error} => {
            if let Some(QueryChannel::Get(ch)) = queries.remove(&query_id.into()) {
                ch.send(Err(anyhow!(error))).ok();
            }
        }
    }
}