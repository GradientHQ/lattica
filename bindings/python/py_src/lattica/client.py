from typing import Optional, Any, List, Union, Tuple
import pickle
import time
from dataclasses import dataclass
from lattica_python_core import LatticaSDK, RpcClient, PeerInfo

@dataclass
class ValueWithExpiration:
    value: Any
    expiration_time: float

    def __iter__(self):
        return iter((self.value, self.expiration_time))

    def __getitem__(self, item):
        if item == 0:
            return self.value
        elif item == 1:
            return self.expiration_time
        else:
            return getattr(self, item)

    def __eq__(self, item):
        if isinstance(item, ValueWithExpiration):
            return self.value == item.value and self.expiration_time == item.expiration_time
        elif isinstance(item, tuple):
            return tuple.__eq__((self.value, self.expiration_time), item)
        else:
            return False

def get_dht_time():
    return time.time()


class Lattica:
    def __init__(self):
        self.config = {}
        self._lattica_instance = None
        self._initialized = False

    @classmethod
    def builder(cls) -> 'Lattica':
        return cls()

    def with_bootstraps(self, bootstrap_nodes: List[str]) -> 'Lattica':
        self.config['bootstrap_nodes'] = bootstrap_nodes
        return self
    def with_listen_addrs(self, listen_addrs: List[str]) -> 'Lattica':
        self.config['listen_addrs'] = listen_addrs
        return self

    def with_idle_timeout(self, timeout_seconds: int) -> 'Lattica':
        self.config['idle_timeout'] = timeout_seconds
        return self

    def with_mdns(self, with_mdns: bool) -> 'Lattica':
        self.config['with_mdns'] = with_mdns
        return self

    def with_upnp(self, with_upnp: bool) -> 'Lattica':
        self.config['with_upnp'] = with_upnp
        return self

    def with_relay_servers(self, relay_servers: List[str]) -> 'Lattica':
        self.config['relay_servers'] = relay_servers
        return self

    def with_autonat(self, with_autonat: bool) -> 'Lattica':
        self.config['with_autonat'] = with_autonat
        return self

    def with_dcutr(self, with_dcutr: bool) -> 'Lattica':
        self.config['with_dcutr'] = with_dcutr
        return self

    def with_external_addrs(self, external_addrs: List[str]) -> 'Lattica':
        self.config['external_addrs'] = external_addrs
        return self

    def with_storage_path(self, storage_path: str) -> 'Lattica':
        self.config['storage_path'] = storage_path
        return self

    def with_dht_db_path(self, db_path: str) -> 'Lattica':
        self.config['dht_db_path'] = db_path
        return self

    def with_key_path(self, key_path: str) -> 'Lattica':
        self.config['key_path'] = key_path
        return self

    def with_protocol(self, protocol: str) -> 'Lattica':
        self.config['protocol'] = protocol
        return self

    def build(self) -> 'Lattica':
        self._initialize_client()
        return self

    def _initialize_client(self):
        if self._initialized:
            return

        try:
            self._initialize_lattica()
            self._initialized = True
        except Exception as e:
            raise RuntimeError(f"Failed to initialize Lattica: {e}")

    def _initialize_lattica(self):
        try:
            if self.config:
                self._lattica_instance = LatticaSDK(self.config)
            else:
                self._lattica_instance = LatticaSDK()
        except Exception as e:
            raise RuntimeError(f"Failed to initialize the lattica1 instance: {e}")

    def _ensure_initialized(self):
        if not self._initialized:
            self._initialize_client()

    def store(self, key: str, value: Any, expiration_time: Optional[float] = None, subkey: Optional[str] = None) -> Union[bool, None]:
        try:
            # default expiration 10min
            if expiration_time is None:
                expiration_time = get_dht_time() + 600

            serialized_value = pickle.dumps(value)
            self._lattica_instance.store_with_subkey(key, serialized_value, expiration_time, subkey)
            return True
        except Exception as e:
            print(f"Failed to store value: {e}")
            return False

    def get(self, key: str) -> Union[ValueWithExpiration, None]:
        try:
            result = self._lattica_instance.get_with_subkey(key)
            if result is None:
                return None

            if isinstance(result, dict):
                parsed_value = {}

                for subkey, (serialized_value, expiration) in result.items():
                    value = pickle.loads(serialized_value)
                    parsed_value[subkey] = ValueWithExpiration(value=value, expiration_time=expiration)

                first_expiration = next(iter(result.values()))[1]
                return ValueWithExpiration(value=parsed_value, expiration_time=first_expiration)
            else:
                serialized_value, expiration = result
                value = pickle.loads(serialized_value)
                return ValueWithExpiration(value=value, expiration_time=expiration)

        except Exception as e:
            print(f"Error getting value: {e}")
            return None

    def get_visible_maddrs(self) -> List[str]:
        try:
            return self._lattica_instance.get_visible_maddrs()
        except Exception as e:
            raise RuntimeError(f"Failed to get visible addresses: {e}")

    def get_client(self, peer_id: str) -> RpcClient:
        try:
            return self._lattica_instance.get_client(peer_id)
        except Exception as e:
            raise RuntimeError(f"Failed to get client: {e}")

    def register_service(self, service_instance) -> None:
        try:
            self._lattica_instance.register_service(service_instance)
        except Exception as e:
            raise RuntimeError(f"Failed to register service: {e}")

    def peer_id(self) -> str:
        try:
            return self._lattica_instance.peer_id()
        except Exception as e:
            raise RuntimeError(f"Failed to get peer ID: {e}")

    def get_peer_info(self, peer_id: str) -> Optional[PeerInfo]:
        try:
            return self._lattica_instance.get_peer_info(peer_id)
        except Exception as e:
            raise RuntimeError(f"Failed to get peer info: {e}")

    def get_all_peers(self) -> List[str]:
        try:
            return self._lattica_instance.get_all_peers()
        except Exception as e:
            raise RuntimeError(f"Failed to get all peers: {e}")

    def get_peer_addresses(self, peer_id: str) -> List[str]:
        try:
            return self._lattica_instance.get_peer_addresses(peer_id)
        except Exception as e:
            raise RuntimeError(f"Failed to get peer addresses: {e}")

    def get_peer_rtt(self, peer_id: str) -> float:
        try:
            return self._lattica_instance.get_peer_rtt(peer_id)
        except Exception as e:
            raise RuntimeError(f"Failed to get peer RTT: {e}")

    def put_block(self, data: bytes) -> str:
        try:
            return self._lattica_instance.put_block(data)
        except Exception as e:
            raise RuntimeError(f"Failed to put block: {e}")

    def get_block(self, cid: str, timeout_secs: int = 30) -> Tuple[Optional[str], bytes]:
        try:
            return self._lattica_instance.get_block(cid, timeout_secs=timeout_secs)
        except Exception as e:
            raise RuntimeError(f"Failed to get block: {e}")

    def remove_block(self, cid: str):
        try:
            return self._lattica_instance.remove_block(cid)
        except Exception as e:
            raise RuntimeError(f"Failed to remove block: {e}")

    def start_providing(self, key: str):
        try:
            return self._lattica_instance.start_providing(key)
        except Exception as e:
            raise RuntimeError(f"Failed to start providing: {e}")

    def get_providers(self, key: str) -> List[str]:
        try:
            return self._lattica_instance.get_providers(key)
        except Exception as e:
            raise RuntimeError(f"Failed to get providers: {e}")

    def stop_providing(self, key: str):
        try:
            return self._lattica_instance.stop_providing(key)
        except Exception as e:
            raise RuntimeError(f"Failed to stop providing: {e}")

    def close(self):
        try:
            self._lattica_instance.close()
        except Exception as e:
            raise RuntimeError(f"Failed to close client: {e}")

    def is_symmetric_nat(self):
        try:
            return self._lattica_instance.is_symmetric_nat()
        except Exception as e:
            raise RuntimeError(f"Failed to check is_symmetric_nat error: {e}")

    def configure_bitswap_peer_selection(
        self,
        top_n: int = 3,
        enabled: bool = True,
        min_peers: int = 2,
        enable_randomness: bool = True
    ) -> None:
        """Configure Bitswap peer selection strategy.
        
        Args:
            top_n: Number of top peers to select
            enabled: Enable smart selection
            min_peers: Minimum peers threshold
            enable_randomness: Enable randomness in selection
        """
        try:
            self._lattica_instance.configure_bitswap_peer_selection(top_n, enabled, min_peers, enable_randomness)
        except Exception as e:
            raise RuntimeError(f"Failed to configure bitswap peer selection: {e}")

    def get_bitswap_global_stats(self) -> dict:
        """Get Bitswap global statistics.
        
        Returns:
            dict with total_requests, successful_requests, failed_requests, total_bytes_received
        """
        try:
            return self._lattica_instance.get_bitswap_global_stats()
        except Exception as e:
            raise RuntimeError(f"Failed to get bitswap global stats: {e}")

    def get_bitswap_peer_rankings(self) -> List[dict]:
        """Get Bitswap peer rankings with detailed metrics.
        
        Returns:
            List of dicts sorted by score descending, each containing:
            - peer_id: Peer ID string
            - score: Composite score (0-100)
            - blocks_received: Number of successfully received blocks
            - failures: Number of failures
            - success_rate: Success rate (0.0-1.0)
            - avg_speed: Average speed in bytes/sec
            - avg_rtt_ms: Average RTT in milliseconds
        """
        try:
            return self._lattica_instance.get_bitswap_peer_rankings_detailed()
        except Exception as e:
            raise RuntimeError(f"Failed to get bitswap peer rankings: {e}")

    def print_bitswap_stats(self) -> None:
        """Print Bitswap stats report to stdout.
        
        This method combines get_bitswap_global_stats() and get_bitswap_peer_rankings()
        to display a formatted statistics report in Python.
        """
        try:
            # Get global stats
            stats = self.get_bitswap_global_stats()
            total_requests = stats.get('successful_requests', 0) + stats.get('failed_requests', 0)
            success_rate = (stats.get('successful_requests', 0) / total_requests * 100) if total_requests > 0 else 0.0
            bytes_mb = stats.get('total_bytes_received', 0) / (1024 * 1024)
            
            print(f"\n{'='*100}")
            print("Bitswap Statistics Report")
            print(f"{'='*100}")
            print(f"  Total Requests:      {total_requests}")
            print(f"  Successful:          {stats.get('successful_requests', 0)}")
            print(f"  Failed:              {stats.get('failed_requests', 0)}")
            print(f"  Success Rate:        {success_rate:.2f}%")
            print(f"  Total Received:      {bytes_mb:.2f} MB")
            
            # Get peer rankings with details
            rankings = self.get_bitswap_peer_rankings()
            if rankings:
                print(f"\nTop {min(len(rankings), 10)} Peers:")
                print(f"  {'#':<3} {'Peer ID':<54} {'Score':>7} {'Success':>8} {'Fail':>5} {'Rate':>7} {'Speed':>12} {'RTT':>8}")
                print(f"  {'-'*3} {'-'*54} {'-'*7} {'-'*8} {'-'*5} {'-'*7} {'-'*12} {'-'*8}")
                for i, peer in enumerate(rankings[:10], 1):
                    peer_id = peer['peer_id']
                    display_id = peer_id[:52] + ".." if len(peer_id) > 54 else peer_id
                    speed_mb = peer['avg_speed'] / (1024 * 1024)
                    rate_pct = peer['success_rate'] * 100
                    print(f"  {i:<3} {display_id:<54} {peer['score']:>7.2f} {peer['blocks_received']:>8} {peer['failures']:>5} {rate_pct:>6.1f}% {speed_mb:>9.2f}MB/s {peer['avg_rtt_ms']:>6.0f}ms")
            else:
                print("\n  No peer data available.")
            print(f"{'='*100}\n")
        except Exception as e:
            raise RuntimeError(f"Failed to print bitswap stats: {e}")

    def __enter__(self):
        self._ensure_initialized()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        pass

    def __del__(self):
        if self._lattica_instance is not None:
            try:
                self._lattica_instance.close()
            except Exception as e:
                print(f"Warning: Failed to shutdown Lattica: {e}")