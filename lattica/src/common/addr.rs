use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;
use libp2p::multiaddr::PeerId;

pub fn get_transport_protocol(addr: &Multiaddr) -> Option<Protocol> {
    addr.iter().nth(1)
}

pub fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
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