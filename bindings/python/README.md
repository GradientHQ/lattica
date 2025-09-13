# Lattica Python SDK

A unified Python SDK for P2P networking with integrated DHT and NAT capabilities, built on top of libp2p.

## Features

- **Distributed Hash Table (DHT)**: Store and retrieve key-value pairs across the network
- **Remote Procedure Call (RPC)**: Execute remote functions with support for complex data types
- **Streaming RPC**: Handle large data transfers with streaming capabilities
- **NAT Traversal**: Automatic NAT traversal with UPnP support
- **Peer Discovery**: mDNS and rendezvous-based peer discovery
- **High Performance**: Built with Rust for optimal performance

## Installation
You can install from source using:
```bash
pip install git+https://github.com/GradientHQ/lattica.git#subdirectory=bindings/python
```

## Quick Start
### Basic Usage

```python
from lattica import Lattica

# Create a Lattica instance
lattica = Lattica.builder().build()

# Get your peer ID
peer_id = lattica.peer_id()
print(f"My peer ID: {peer_id}")
```

## Examples

### 1. DHT Operations

The DHT example demonstrates basic key-value storage and retrieval with subkey support.

```python
from lattica import Lattica

# Create client1 as bootstrap node
lattica = Lattica.builder()
    .build()

# Create client2 with bootstrap nodes
lattica = Lattica.builder() \
    .with_bootstraps(["/ip4/127.0.0.1/tcp/54282/p2p/QmServerPeerId"]) \
    .build()

# Store a simple key-value pair with default expiration 10 minute
lattica.store("name", "alice")

# get the value
result = lattica.get("name")
if result:
    print(f"Value: {result.value}")
    print(f"Expires: {result.expiration_time}")


# Store values with subkeys (useful for voting, user data, etc.)
key = "peer_list"
peers = ["alice", "bob", "carol"]

# Each peer stores their vote with a subkey
lattica.store(key, "yes", expiration_time, subkey="alice")
lattica.store(key, "no", expiration_time, subkey="bob")
lattica.store(key, "maybe", expiration_time, subkey="carol")

# get all votes
votes_result = lattica.get(key)
if votes_result:
    for peer, vote_info in votes_result.value.items():
        print(f"{peer}: {vote_info.value}")
```

### 2. RPC Operations

The RPC example demonstrates remote procedure calls with support for complex data types and streaming.

```python
from lattica import Lattica, rpc_method, rpc_stream, ConnectionHandler



class MyService(ConnectionHandler):
    @rpc_method
    def add(self, a: int, b: int) -> int:
        """Simple addition"""
        return a + b
    
    @rpc_stream
    def process_data(self, data: list) -> list:
        return data

# Create client1 as RPC server and bootstrap node
lattica = Lattica.builder()
    .build()
service = MyService(lattica)

# Create client2 with bootstrap nodes
lattica = Lattica.builder() \
    .with_bootstraps(["/ip4/127.0.0.1/tcp/54282/p2p/QmServerPeerId"]) \
    .build()
client_service = MyService(client_lattica)

# Make RPC calls
stub = client_service.get_stub(server_peer_id)
result = stub.add(10, 20)  # Returns 30

# Handle complex data types
num_floats = int(2 * 1024 * 1024 * 1024) // 8 #2GB
test_data = [random.random() for _ in range(num_floats)]
result = stub.process_data(test_data)

```

## Configuration

### Builder Pattern

```python
lattica = Lattica.builder() \
    .with_bootstraps([
        "/ip4/127.0.0.1/tcp/8080/p2p/QmBootstrap1",
        "/ip4/127.0.0.1/tcp/8081/p2p/QmBootstrap2"
    ]) \
    .with_listen_addrs(["/ip4/0.0.0.0/tcp/0", "/ip4/0.0.0.0/udp/0/quic-v1"])
    .with_external_addrs(["/ip4/0.0.0.0/tcp/0"])
    .with_mdns(True) \
    .with_upnp(True) \
    .build()
```

### Configuration Options

- `with_bootstraps(nodes)`: Set bootstrap nodes for network discovery
- `with_listen_addrs(addrs)`: Set listening address
- `with_mdns(enabled)`: Enable/disable mDNS peer discovery
- `with_upnp(enabled)`: Enable/disable UPnP NAT traversal
- `with_relay_servers(servers)`: Set relay servers for network relay
- `with_autonat(enabled)`: Enable/disable AutoNAT detect[need relay servers]
- `with_dcutr(enabled)`: Enable/disable TCP/QUIC NAT travelsal[need relay servers]
- `with_external_addrs(addrs)`: Set external address
- `with_storage_path`: Persistent storage path
- `with_dht_db_path`: DHT Persistent db path

## Development

### Building from Source

```bash
# Install maturin
pip install maturin

# Build the package
cd bindings/python
pip install .
```