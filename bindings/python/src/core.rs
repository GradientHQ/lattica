use lattica::{network, rpc, common};
use std::sync::{Arc};
use tokio::sync::{Mutex};
use pyo3::{prelude::*, types::PyDict, IntoPyObjectExt};
use tokio::runtime::Runtime;
use libp2p::{Multiaddr, PeerId};
use async_trait::async_trait;
use pyo3::types::{PyAny};
use std::time::Duration;
use tokio::task::JoinHandle;
use lattica::common::{retry_with_backoff, BytesBlock, RetryConfig};
use cid::{Cid};
use libp2p::kad::RecordKey;

#[pyclass]
pub struct LatticaSDK {
    lattica: Arc<network::Lattica>,
    runtime: Arc<Runtime>,
}

#[pyclass]
pub struct RpcClient {
    peer_id: String,
    lattica: Arc<network::Lattica>,
    runtime: Arc<Runtime>,
}

#[pyclass]
pub struct LFuture {
    pub handle: Arc<Mutex<Option<JoinHandle<Result<Vec<u8>, String>>>>>,
    pub runtime: Arc<Runtime>,
}

#[pyclass]
#[derive(Clone)]
pub struct PeerInfo {
    #[pyo3(get)]
    pub protocol_version: Option<String>,
    #[pyo3(get)]
    pub agent_version: Option<String>,
    #[pyo3(get)]
    pub protocols: Vec<String>,
    #[pyo3(get)]
    pub listen_addresses: Vec<String>,
    #[pyo3(get)]
    pub addresses: Vec<String>,
    #[pyo3(get)]
    pub last_seen: Option<String>,
    #[pyo3(get)]
    pub rtt_ms: Option<u64>,
    #[pyo3(get)]
    pub decay_3: Option<u64>,
    #[pyo3(get)]
    pub decay_10: Option<u64>,
    #[pyo3(get)]
    pub failures: u32,
    #[pyo3(get)]
    pub failure_rate: u32,
}

impl From<&network::peer_info::PeerInfo> for PeerInfo {
    fn from(info: &network::peer_info::PeerInfo) -> Self {
        Self {
            protocol_version: info.protocol_version().map(|s| s.to_string()),
            agent_version: info.agent_version().map(|s| s.to_string()),
            protocols: info.protocols().map(|s| s.to_string()).collect(),
            listen_addresses: info.listen_addresses().map(|addr| addr.to_string()).collect(),
            addresses: info.addresses().map(|(addr, _, _, _, _)| addr.to_string()).collect(),
            last_seen: info.last_seen().map(|s| s.to_string()),
            rtt_ms: info.rtt().map(|d| d.as_millis() as u64),
            decay_3: info.decay_3().map(|d| d.as_millis() as u64),
            decay_10: info.decay_10().map(|d| d.as_millis() as u64),
            failures: info.failures(),
            failure_rate: info.failure_rate(),
        }
    }
}

#[pymethods]
impl LFuture {
    pub fn result(&self, timeout: u64) -> PyResult<Vec<u8>> {
        let handle_opt = self.runtime.block_on(async {
            self.handle.lock().await.take()
        });

        Python::with_gil(|py| {
            py.allow_threads(|| {
                if let Some(handle) = handle_opt {
                    let join_ret = self.runtime.block_on(async { tokio::time::timeout(Duration::from_secs(timeout), handle).await});

                    let handle_ret = match join_ret {
                        Ok(ret) => ret,
                        Err(_) => {
                            return Err(pyo3::exceptions::PyRuntimeError::new_err("operation timeout"));
                        }
                    };

                    let task_ret = match handle_ret {
                        Ok(ret) => ret,
                        Err(e) => {
                            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))
                        }
                    };

                    match task_ret {
                        Ok(ret) => Ok(ret),
                        Err(e) => {
                            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e)))
                        }
                    }
                } else {
                    Err(pyo3::exceptions::PyRuntimeError::new_err("future already consumed"))
                }
            })
        })
    }

    pub fn __await__(slf: PyRef<'_, Self>) -> PyResult<PyObject> {
        let handle_arc = slf.handle.clone();

        Python::with_gil(|py| {
            let future = pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let mut handle_lock = handle_arc.lock().await;
                if let Some(handle) = handle_lock.take() {
                    let res = handle.await.map_err(|e| {
                        pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e))
                    })?;
                    match res {
                        Ok(res) => Ok(res),
                        Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(format!("{:?}", e))),
                    }
                } else {
                    Err(pyo3::exceptions::PyRuntimeError::new_err("future already consumed"))
                }
            })?;
            let awaitable = future.getattr("__await__")?.call0()?;
            Ok(awaitable.unbind())
        })
    }
}

#[pymethods]
impl LatticaSDK {
    #[new]
    #[pyo3(signature = (config_dict = None))]
    fn new(config_dict: Option<&Bound<PyAny>>) -> PyResult<Self> {
        tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

        let bootstrap_nodes = config_dict
            .and_then(|dict| dict.get_item("bootstrap_nodes").ok())
            .and_then(|item| item.extract::<Vec<String>>().ok())
            .map(|nodes| {
                nodes
                    .into_iter()
                    .filter_map(|addr| addr.parse::<Multiaddr>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let relay_servers = config_dict
            .and_then(|dict| dict.get_item("relay_servers").ok())
            .and_then(|item| item.extract::<Vec<String>>().ok())
            .map(|servers| {
                servers
                    .into_iter()
                    .filter_map(|addr| addr.parse::<Multiaddr>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let with_mdns = config_dict
            .and_then(|dict| dict.get_item("with_mdns").ok())
            .and_then(|item| item.extract::<bool>().ok())
            .unwrap_or(true);

        let with_upnp = config_dict
            .and_then(|dict| dict.get_item("with_upnp").ok())
            .and_then(|item| item.extract::<bool>().ok())
            .unwrap_or(true);

        let with_autonat = config_dict
            .and_then(|dict| dict.get_item("with_autonat").ok())
            .and_then(|item| item.extract::<bool>().ok())
            .unwrap_or(false);

        let with_dcutr = config_dict
            .and_then(|dict| dict.get_item("with_dcutr").ok())
            .and_then(|item| item.extract::<bool>().ok())
            .unwrap_or(false);

        let listen_addrs = config_dict
            .and_then(|dict| dict.get_item("listen_addrs").ok())
            .and_then(|item| item.extract::<Vec<String>>().ok())
            .map(|servers| {
                servers
                    .into_iter()
                    .filter_map(|addr| addr.parse::<Multiaddr>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let external_addrs = config_dict
            .and_then(|dict| dict.get_item("external_addrs").ok())
            .and_then(|item| item.extract::<Vec<String>>().ok())
            .map(|servers| {
                servers
                    .into_iter()
                    .filter_map(|addr| addr.parse::<Multiaddr>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let storage_path = config_dict
            .and_then(|dict| dict.get_item("storage_path").ok())
            .and_then(|item| item.extract::<String>().ok());

        let dht_db_path = config_dict
            .and_then(|dict| dict.get_item("dht_db_path").ok())
            .and_then(|item| item.extract::<String>().ok());

        let runtime = Arc::new(Runtime::new()?);
        let lattica = runtime.block_on(async move {
            network::Lattica::builder()
                .with_bootstrap_nodes(bootstrap_nodes)
                .with_mdns(with_mdns)
                .with_upnp(with_upnp)
                .with_relay_servers(relay_servers)
                .with_autonat(with_autonat)
                .with_dcutr(with_dcutr)
                .with_listen_addrs(listen_addrs)
                .with_external_addrs(external_addrs)
                .with_storage_path(storage_path)
                .with_dht_db_path(dht_db_path)
                .build().await.map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to create Lattica: {:?}", e)))
        })?;

        Ok(Self {
            lattica: Arc::new(lattica),
            runtime,
        })
    }

    fn store_with_subkey(&self, key: &str, value: &[u8], expiration_time: f64, subkey: Option<&str>) -> PyResult<()> {
        Python::with_gil(|py| {
            py.allow_threads(|| {
                self.runtime.block_on(async move {
                    if let Some(subkey) = subkey {
                        self.lattica.store_subkey(key, subkey, value.to_vec(), expiration_time).await
                    } else {
                        self.lattica.store_simple(key, value.to_vec(), expiration_time).await
                    }
                        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeWarning, _>(format!("Failed to store: {:?}", e)))
                })
            })
        })
    }

    fn get_with_subkey(&self, key: &str) -> PyResult<Option<PyObject>> {
        Python::with_gil(|py| {
            py.allow_threads(|| {
                self.runtime.block_on(async move {
                    match self.lattica.get_with_subkey(key).await {
                        Ok(result) => {
                            Python::with_gil(|py| {
                                match result {
                                    Some(common::types::DhtValue::Simple { value, expiration }) => {
                                        // return (value, expiration)
                                        let tuple = (value, expiration).into_py_any(py)?;
                                        Ok(Some(tuple))
                                    }
                                    Some(common::types::DhtValue::WithSubkeys { subkeys }) => {
                                        // return {subkey: (value, expiration)}
                                        let dict = PyDict::new(py);
                                        for (subkey, value, expiration) in subkeys {
                                            let tuple = (value, expiration).into_py_any(py)?;
                                            dict.set_item(subkey, tuple)?;
                                        }
                                        Ok(Some(dict.into_py_any(py)?))
                                    }
                                    None => Ok(None)
                                }
                            })
                        }
                        Err(_) => {
                            Ok(None)
                        }
                    }
                })
            })
        })
    }

    fn get_visible_maddrs(&self) -> PyResult<Vec<String>> {
        self.runtime.block_on(async move {
            match self.lattica.get_visible_maddrs().await {
                Ok(addrs) => Ok(addrs.into_iter()
                    .map(|addr| addr.to_string())
                    .collect()),
                Err(e) => Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                    format!("Failed to get visible addresses: {}", e))),
            }
        })
    }

    fn connect(&self, peer_id: &str) -> PyResult<()> {
        self.runtime.block_on(async move {
            let peer_id: PeerId = peer_id.parse()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
            self.lattica.connect(peer_id).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Failed to connect: {:?}", e)))
        })
    }

    fn register_service(&self, py: Python, service: &Bound<PyAny>) -> PyResult<()> {
        let service_name = service.getattr("__class__")?.getattr("__name__")?.extract::<String>()?;
        let service_impl = PythonRpcService::new(service_name, service.into_py_any(py).unwrap())?;

        self.runtime.block_on(async move {
            self.lattica.register_service(Box::new(service_impl)).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to register service: {:?}", e)))
        })
    }

    fn get_client(&self, peer_id: &str) -> PyResult<RpcClient> {
        let peer_id_str = peer_id.to_string();
        Ok(RpcClient {
            peer_id: peer_id_str,
            lattica: self.lattica.clone(),
            runtime: self.runtime.clone(),
        })
    }

    fn peer_id(&self) -> PyResult<String> {
        self.runtime.block_on(async move {
            Ok(self.lattica.peer_id().to_string())
        })
    }

    fn get_peer_info(&self, peer_id: &str) -> PyResult<Option<PeerInfo>> {
        self.runtime.block_on(async move {
            let peer_id: PeerId = peer_id.parse()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
            
            match self.lattica.get_peer_info(&peer_id).await {
                Some(info) => Ok(Some(PeerInfo::from(&info))),
                None => Ok(None),
            }
        })
    }

    fn get_all_peers(&self) -> PyResult<Vec<String>> {
        self.runtime.block_on(async move {
            let peers = self.lattica.get_all_peers().await;
            Ok(peers.into_iter().map(|p| p.to_string()).collect())
        })
    }

    fn get_peer_addresses(&self, peer_id: &str) -> PyResult<Vec<String>> {
        self.runtime.block_on(async move {
            let peer_id: PeerId = peer_id.parse()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
            
            let addresses = self.lattica.get_peer_addresses(&peer_id).await;
            Ok(addresses.into_iter().map(|addr| addr.to_string()).collect())
        })
    }

    fn get_peer_rtt(&self, peer_id: &str) -> PyResult<Option<f64>> {
        self.runtime.block_on(async move {
            let peer_id: PeerId = peer_id.parse()
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
            
            match self.lattica.get_peer_rtt(&peer_id).await {
                Some(rtt) => Ok(Some(rtt.as_secs_f64())),
                None => Ok(None),
            }
        })
    }

    fn get_block(&self, cid_str: &str) -> PyResult<Vec<u8>> {
        self.runtime.block_on(async move {
            let cid = Cid::try_from(cid_str)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid CID: {:?}", e)))?;
            let bc = self.lattica.get_block(&cid).await?;
            Ok(bc.0)
        })
    }

    fn put_block(&self, data: Vec<u8>) -> PyResult<String> {
        self.runtime.block_on(async move {
            let block = BytesBlock(data);
            let cid = self.lattica.put_block(&block).await?;
            Ok(cid.to_string())
        })
    }

    fn remove_block(&self, cid_str: &str) -> PyResult<()> {
        self.runtime.block_on(async move {
            let cid = Cid::try_from(cid_str)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid CID: {:?}", e)))?;
            self.lattica.remove_block(&cid).await?;
            Ok(())
        })
    }

    fn start_providing(&self, key: &str) -> PyResult<()> {
        self.runtime.block_on(async move {
            let record = RecordKey::new(&key);
            self.lattica.start_providing(record).await?;
            Ok(())
        })
    }

    fn get_providers(&self, key: &str) -> PyResult<Vec<String>> {
        self.runtime.block_on(async move {
            let record = RecordKey::new(&key);
            let peers = self.lattica.get_providers(record).await?;
            Ok(peers.into_iter().map(|p| p.to_string()).collect())
        })
    }

    fn stop_providing(&self, key: &str) -> PyResult<()> {
        self.runtime.block_on(async move {
            let record = RecordKey::new(&key);
            self.lattica.stop_providing(record).await?;
            Ok(())
        })
    }
}

#[pymethods]
impl RpcClient {
    fn call(&self, method: &str, data: &[u8]) -> PyResult<LFuture>  {
        let lattica_clone = self.lattica.clone();
        Python::with_gil(|py| {
            py.allow_threads(|| {
                let peer_id: PeerId = self.peer_id.parse()
                    .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
                let method = method.to_string();
                let data = data.to_vec();

                let handle = self.runtime.spawn(async move {
                    let retry_config = RetryConfig::default();
                    let result = retry_with_backoff(||{
                        let lattica_clone = lattica_clone.clone();
                        let method = method.clone();
                        let data = data.clone();

                        async move {
                            lattica_clone.call(peer_id, method, data).await
                                .map(|resp| resp.data)
                                .map_err(|e| format!("RPC call failed: {:?}", e))
                        }
                    }, retry_config).await;
                    result
                });

                Ok(LFuture{
                    handle: Arc::new(Mutex::new(Some(handle))),
                    runtime: self.runtime.clone()
                })
            })
        })
    }

    fn call_stream<'py>(&self, method: &str, data: &[u8]) -> PyResult<LFuture> {
        let lattica_clone = self.lattica.clone();
        Python::with_gil(|py| {
            py.allow_threads(|| {
                let peer_id: PeerId = self.peer_id.parse()
                    .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(format!("Invalid peer ID: {:?}", e)))?;
                
                let method = method.to_string();
                let data = data.to_vec();
                let handle = self.runtime.spawn(async move {
                    let retry_config = RetryConfig::default();
                    let result = retry_with_backoff(||{
                        let lattica_clone = lattica_clone.clone();
                        let method = method.clone();
                        let data = data.clone();

                        async move {
                            lattica_clone.call_stream(peer_id, method, data).await.map_err(|e| format!("RPC call failed: {:?}", e))
                        }
                    }, retry_config).await;
                    result
                });
                
                Ok(LFuture{
                    handle: Arc::new(Mutex::new(Some(handle))),
                    runtime: self.runtime.clone()
                })
            })
        })
    }
}

struct PythonRpcService {
    name: String,
    instance: PyObject,
}

impl PythonRpcService {
    fn new(name: String, instance: PyObject) -> PyResult<Self> {
        Ok(Self{name, instance})
    }
}

#[async_trait]
impl rpc::RpcService for PythonRpcService {
    fn service_name(&self) -> &str {
        &self.name
    }

    fn methods(&self) -> Vec<String> {
        Python::with_gil(|py| {
            let instance = self.instance.bind(py);
            let methods = instance.getattr("_rpc_methods").unwrap();
            methods.extract::<Vec<String>>().unwrap_or_default()
        })
    }

    async fn handle_request(&self, method: &str, request: rpc::RpcRequest) -> rpc::RpcResult<rpc::RpcResponse> {
        let method_name = format!("_handle_{}", method);
        let instance_ptr = Python::with_gil(|_py| {
            self.instance.clone()
        });

        let result = tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let instance = instance_ptr.bind(py);

                if let Ok(handler) = instance.getattr(&method_name) {
                    match handler.call1((request.data, )) {
                        Ok(result) => {
                            result.extract::<Vec<u8>>().map_err(|e| format!("Failed to extract response: {}", e))
                        }
                        Err(e) => Err(format!("Handler execution failed: {}", e)),
                    }
                } else {
                    Err(format!("Method {} not found", method_name))
                }
            })
        }).await.map_err(|e| format!("RPC call failed: {:?}", e))?;

        match result {
            Ok(response_data) => {
                Ok(rpc::RpcResponse{
                    id: request.id,
                    data: response_data,
                    compression: None
                })
            }
            Err(e) => {
                Err(e)
            }
        }
    }

    async fn handle_stream(&self, method: &str, request: rpc::StreamRequest) -> rpc::RpcResult<rpc::StreamResponse> {
        let method_name = format!("_handle_stream_{}", method);

        let instance_ptr = Python::with_gil(|_py| {
            self.instance.clone()
        });

        let result = tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let instance = instance_ptr.bind(py);
                if let Ok(handler) = instance.getattr(&method_name) {
                    match handler.call1((request.data.as_ref(), )) {
                        Ok(response) => {
                            if let Ok(bytes_data) = response.extract::<Vec<u8>>() {
                                Ok(bytes_data)
                            } else {
                                Err(format!("Failed to extract bytes data from response: {:?}", response))
                            }
                        }
                        Err(e) => {
                            Err(format!("Failed to handle stream: {}", e))
                        }
                    }
                } else {
                    Err(format!("Stream method {} not found", method_name))
                }
            })
        }).await.map_err(|e| format!("RPC call failed: {:?}", e))?;

        match result {
            Ok(response_data) => {
                Ok(rpc::StreamResponse{
                    id: request.id,
                    data: response_data,
                })
            }
            Err(e) => {
                Err(e)
            }
        }
    }
}