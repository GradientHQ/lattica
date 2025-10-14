use std::sync::Arc;
use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use libp2p::multiaddr::PeerId;
use tokio::sync::RwLock;
use crate::network::AddressBook;

pub fn get_transport_protocol(addr: &Multiaddr) -> Option<Protocol<'_>>  {
    addr.iter().nth(1)
}

pub fn extract_peer_id(addr: Multiaddr) -> Option<PeerId> {
    for protocol in addr.iter() {
        if let Protocol::P2p(peer_id) = protocol {
            return Some(peer_id);
        }
    }
    None
}

pub fn construct_relayed_addr(relay_addr: &Multiaddr, my_peer: &PeerId) -> Multiaddr {
    let mut addr = relay_addr.clone();
    addr.push(Protocol::P2pCircuit);
    addr.push(Protocol::P2p(*my_peer));
    addr
}

pub fn is_relay_server(relay_addrs: &Vec<Multiaddr>, peer_id: PeerId) -> bool {
    for relay_addr in relay_addrs {
        if let Some(last) = relay_addr.iter().last() {
            if last == Protocol::P2p(peer_id) {
                return true;
            }
        }
    }
    false
}

pub async fn has_direct_connection(address_book: &Arc<RwLock<AddressBook>>, peer_id: &PeerId) -> bool {
    let book = address_book.read().await;
    if let Some(info) = book.info(peer_id) {
        for (_, _, _, relayed, _) in info.addresses() {
            if !relayed {return true;}
        }
    }
    false
}