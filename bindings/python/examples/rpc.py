import asyncio
import time
import sys
from lattica import Lattica, rpc_method, rpc_stream, ConnectionHandler
import pickle

class MockProtoRequest:
    def __init__(self, data=None):
        self.data = data
        self.timestamp = time.time()

    def SerializeToString(self):
        return pickle.dumps({
            'data': self.data,
            'timestamp': self.timestamp
        })

    def ParseFromString(self, data):
        parsed = pickle.loads(data)
        self.data = parsed['data']
        self.timestamp = parsed['timestamp']
        return self

class MockProtoResponse:
    def __init__(self, data=None, message=''):
        self.data = data
        self.message = message

    def SerializeToString(self):
        return pickle.dumps({
            'data': self.data,
            'message': self.message,
        })

    def ParseFromString(self, data):
        parsed = pickle.loads(data)
        self.message = parsed['message']
        self.data = parsed['data']
        return self

class RPCHandler(ConnectionHandler):
    @rpc_method
    def add(self, a: int, b: int) -> int:
        return a + b

    @rpc_method
    def simple_rpc(
            self,
            request: MockProtoRequest,
    ) -> MockProtoResponse:
        return MockProtoResponse(
            message=f"Processed data of size {len(request.data)}",
            data=None
        )

    @rpc_stream
    def stream_rpc(self, request: MockProtoRequest ) -> MockProtoResponse:
        return MockProtoResponse(
            message=f"Processed data of size {len(request.data)}",
            data=None
        )

async def run_server():
    lattica_inst = Lattica.builder().build()
    RPCHandler(lattica_inst)

    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        print("Server shutting down...")

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

async def run_client():
    bootstrap_addr = sys.argv[1]
    bootstrap_addr, server_peer_id = parse_multiaddr(bootstrap_addr)
    bootstrap_nodes = [bootstrap_addr]

    lattica_inst = Lattica.builder().with_bootstraps(bootstrap_nodes).build()
    handler = RPCHandler(lattica_inst)

    try:
        stub = handler.get_stub(server_peer_id)

        print("\n=== Testing Basic RPC ===")
        future = stub.add(10, 20)
        result = future.result()
        print(f"10 + 20 = {result}")

        print("\n=== Testing Protobuf RPC ===")
        request = MockProtoRequest(data=b"Hello from protobuf!")
        future = stub.simple_rpc(request)
        result = future.result()
        print(f"Response message: {result.message}")

        print("\n=== Testing Stream RPC (1GB) ===")
        request = MockProtoRequest(data=bytearray(1024 * 1024 * 1024))
        start_time = time.time()
        future = stub.stream_rpc(request)
        result = future.result(timeout=10)
        transfer_time = time.time() - start_time
        print(f"result: {result.message}")
        print(f"Total transfer time: {transfer_time:.2f}s")

    except Exception as e:
        print(f"Client error: {e}")


# node1: python rpc.py
# node2: python rpc.py /ip4/127.0.0.1/tcp/x/p2p/x
if __name__ == "__main__":
    if len(sys.argv) > 1:
        asyncio.run(run_client())
    else:
        asyncio.run(run_server())