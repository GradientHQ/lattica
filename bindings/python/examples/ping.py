#!/usr/bin/env python3
import asyncio
import sys
import traceback
from lattica import Lattica

def parse_multiaddr(addr_str: str):
    if not addr_str.startswith('/'):
        raise ValueError(f"Invalid multiaddr format: {addr_str}")

    parts = addr_str.strip().split('/')
    if len(parts) < 6:
        raise ValueError(f"Incomplete multiaddr: {addr_str}")

    peer_id = None
    for i, part in enumerate(parts):
        if part == 'p2p' and i + 1 < len(parts):
            peer_id = parts[i + 1]
            break

    if not peer_id:
        raise ValueError(f"No peer ID found in multiaddr: {addr_str}")

    return addr_str, peer_id

async def print_peer_info(node, node_name):
    print(f"\n{node_name}:")
    peers = node.get_all_peers()
    print(f"  - peers: {len(peers)}")
    
    for peer_id in peers:
        peer_info = node.get_peer_info(peer_id)
        if peer_info:
            print(f"  - Peer {peer_id}")
            print(f"    - addresses: {peer_info.addresses}")
            print(f"    - RTT: {peer_info.rtt_ms}ms")
            print(f"    - last_seen: {peer_info.last_seen}")
            if peer_info.decay_3:
                print(f"    - decay_3: {peer_info.decay_3}ms")
            if peer_info.decay_10:
                print(f"    - decay_10: {peer_info.decay_10}ms")
            if peer_info.failures:
                print(f"    - failures: {peer_info.failures}")
            if peer_info.failure_rate:
                print(f"    - failure_rate: {peer_info.failure_rate}")

async def run_client():
    bootstrap_addr = sys.argv[1]
    bootstrap_addr, server_peer_id = parse_multiaddr(bootstrap_addr)
    bootstrap_nodes = [bootstrap_addr]

    client = Lattica.builder().with_bootstraps(bootstrap_nodes).build()

    await print_peer_info(client, "client")

    for i in range(1, 20):
        print(f"\nPing test {i}/5:")

        before_rtt = client.get_peer_rtt(server_peer_id)
        print(f"  - Before RTT: {before_rtt}s")

        # wait for next ping finish
        await asyncio.sleep(3)

        after_rtt = client.get_peer_rtt(server_peer_id)
        print(f"  - After RTT: {after_rtt}s")
        
        if before_rtt is not None and after_rtt is not None:
            diff = abs(after_rtt - before_rtt)
            print(f"  - RTT Change: {diff:.6f}s")

        peer_info = client.get_peer_info(server_peer_id)
        if peer_info:
            print(f"  - Current Server RTT: RTT={peer_info.rtt_ms}ms")
            addresses = client.get_peer_addresses(server_peer_id)
            print(f"  - Server address count: {len(addresses)}")

    print("\nfinal info:")
    await print_peer_info(client, "client")

async def run_server():
    server = Lattica.builder().build()

    while True:
        await print_peer_info(server, "server")
        await asyncio.sleep(10)

# node1: python ping.py
# node2: python ping.py /ip4/127.0.0.1/tcp/x/p2p/x
if __name__ == "__main__":
    try:
        if len(sys.argv) > 1:
            asyncio.run(run_client())
        else:
            asyncio.run(run_server())
    except KeyboardInterrupt:
        print("Shutting down...")
    except Exception as e:
        print(f"error: {e}")
        traceback.print_exc()
