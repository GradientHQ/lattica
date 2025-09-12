use super::peer_info::{AddressSource, PeerInfo};
use chrono::{DateTime, Utc};
use fnv::{FnvHashMap};
use libp2p::{
    identify,
    multiaddr::Protocol,
    Multiaddr, PeerId,
};
use std::{
    time::Duration,
};
use libp2p::swarm::ConnectionId;

pub(crate) fn normalize_addr(addr: &mut Multiaddr, peer: &PeerId) {
    if addr.iter().any(|p| matches!(p, Protocol::P2p(_))) {
        return;
    }
    addr.push(Protocol::P2p(*peer));
}

pub struct AddressBook {
    peers: FnvHashMap<PeerId, PeerInfo>,
}

impl AddressBook {
    pub fn new() -> Self {
        Self {
            peers: FnvHashMap::default(),
        }
    }

    pub fn add_address(&mut self, peer: &PeerId, address: Multiaddr, source: AddressSource, is_relayed: bool, connection_id: ConnectionId) {
        let mut addr = address.clone();
        normalize_addr(&mut addr, peer);
        
        let peer_info = self.peers.entry(*peer).or_insert_with(PeerInfo::default);
        peer_info.ingest_address(addr, source, is_relayed, connection_id);
        
        tracing::debug!("Added address for peer {}: {:?}", peer, address);
    }

    pub fn peers(&self) -> Vec<PeerId> {
        self.peers.keys().cloned().collect()
    }

    pub fn info(&self, peer_id: &PeerId) -> Option<PeerInfo> {
        self.peers.get(peer_id).cloned()
    }
    pub fn set_last_seen(&mut self, peer_id: &PeerId, last_seen: DateTime<Utc>) {
        if let Some(peer_info) = self.peers.get_mut(peer_id) {
            peer_info.last_seen = Some(last_seen.to_string());
        }
    }

    pub fn set_rtt(&mut self, peer_id: &PeerId, rtt: Option<Duration>) {
        if let Some(peer_info) = self.peers.get_mut(peer_id) {
            peer_info.set_rtt(rtt);
        }
    }

    pub fn set_info(&mut self, peer_id: &PeerId, identify: identify::Info) {
        let peer_info = self.peers.entry(*peer_id).or_insert_with(PeerInfo::default);
        
        peer_info.protocol_version = Some(identify.protocol_version.clone());
        peer_info.agent_version = Some(identify.agent_version.clone());
        peer_info.protocols = identify.protocols.iter().map(|p| p.to_string()).collect();
        peer_info.listeners = identify.listen_addrs.clone();
        
        tracing::debug!("Updated info for peer {}: {:?}", peer_id, identify);
    }

    pub fn get_peer_addresses(&self, peer_id: &PeerId) -> Vec<Multiaddr> {
        self.peers
            .get(peer_id)
            .map(|info| info.addresses().map(|(addr, _, _, _, _)| addr.clone()).collect())
            .unwrap_or_default()
    }
}