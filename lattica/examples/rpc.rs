use lattica::{network, rpc};
use tokio::signal;
use std::env;
use libp2p::Multiaddr;
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use bincode::{Decode, Encode};

#[derive(Serialize, Deserialize, Debug, Encode, Decode)]
struct EchoRequest {
    message: String,
}

#[derive(Serialize, Deserialize, Debug, Encode, Decode)]
struct EchoResponse {
    echo: String,
    timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, Encode, Decode)]
struct CalculateRequest {
    a: i32,
    b: i32,
    operation: String,
}

#[derive(Serialize, Deserialize, Debug, Encode, Decode)]
struct CalculateResponse {
    result: i32,
}

struct ExampleService {
    service_name: String,
}

impl ExampleService {
    fn new() -> Self {
        Self {
            service_name: "ExampleService".to_string(),
        }
    }
}

#[async_trait]
impl rpc::RpcService for ExampleService {
    fn service_name(&self) -> &str {
        &self.service_name
    }

    fn methods(&self) -> Vec<String> {
        vec!["echo".to_string(), "calculate".to_string(), "counter".to_string()]
    }

    async fn handle_request(&self, method: &str, request: rpc::RpcRequest) -> Result<rpc::RpcResponse, String> {
        match method {
            "echo" => {
                let echo_req: EchoRequest = bincode::decode_from_slice(&request.data, bincode::config::standard())
                    .map(|(data, _)| data)
                    .map_err(|e| format!("Failed to decode echo request: {}", e))?;

                let response = EchoResponse {
                    echo: format!("Echo: {}", echo_req.message),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                };

                let response_data = bincode::encode_to_vec(&response, bincode::config::standard())
                    .map_err(|e| format!("Failed to encode echo response: {}", e))?;

                Ok(rpc::RpcResponse {
                    id: request.id,
                    data: response_data,
                    compression: None,
                })
            }
            "calculate" => {
                let calc_req: CalculateRequest = bincode::decode_from_slice(&request.data, bincode::config::standard())
                    .map(|(data, _)| data)
                    .map_err(|e| format!("Failed to decode calculate request: {}", e))?;

                let result = match calc_req.operation.as_str() {
                    "add" => calc_req.a + calc_req.b,
                    "subtract" => calc_req.a - calc_req.b,
                    "multiply" => calc_req.a * calc_req.b,
                    "divide" => {
                        if calc_req.b == 0 {
                            return Err("Division by zero".to_string());
                        }
                        calc_req.a / calc_req.b
                    }
                    _ => return Err(format!("Unknown operation: {}", calc_req.operation)),
                };

                let response = CalculateResponse { result };
                let response_data = bincode::encode_to_vec(&response, bincode::config::standard())
                    .map_err(|e| format!("Failed to encode calculate response: {}", e))?;

                Ok(rpc::RpcResponse {
                    id: request.id,
                    data: response_data,
                    compression: None,
                })
            }
            _ => Err(format!("Unknown method: {}", method)),
        }
    }

    async fn handle_stream(&self, method: &str, request: rpc::StreamRequest) -> Result<rpc::StreamResponse, String> {
        match method {
            "counter" => {
                let count: u32 = if request.data.is_empty() {
                    10
                } else {
                    bincode::decode_from_slice(&request.data, bincode::config::standard())
                        .map(|(data, _)| data)
                        .unwrap_or(10)
                };

                Ok(rpc::StreamResponse {
                    id: request.id.clone(),
                    data: vec![]
                })
            }
            _ => Err(format!("Unknown stream method: {}", method)),
        }
    }
}

// node1: cargo run --example rpc
// node2: cargo run --example rpc /ip4/127.0.0.1/tcp/x/p2p/xxxxxxxx(bootstraps) xxxxxxxx(server peerID)
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    let args: Vec<String> = env::args().collect();
    let mut bootstrap_nodes = vec![];
    let mut server_peer_id = String::new();
    if args.len() > 2 {
        bootstrap_nodes.push(args[1].parse::<Multiaddr>()?);
        server_peer_id = args[2].clone();
    }


    let mut lattica = network::Lattica::builder()
        .with_bootstrap_nodes(bootstrap_nodes.clone())
        .with_kad(true)
        .build().await?;

    if server_peer_id.is_empty() {
        println!("Running as SERVER mode");
        println!("Registering ExampleService...");
        let service = ExampleService::new();
        lattica.register_service(Box::new(service)).await?;
        println!("ExampleService registered successfully!");
        println!("Available methods: echo, calculate, counter (stream)");
        println!("Waiting for RPC calls...");
    } else {
        println!("Running as CLIENT mode");
        println!("Connecting to server: {}", server_peer_id);
        let peer_id = server_peer_id.parse().unwrap();
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        println!("\n=== Testing Echo RPC ===");
        let echo_request = EchoRequest {
            message: "Hello from client!".to_string(),
        };
        let request_data = bincode::encode_to_vec(&echo_request, bincode::config::standard())?;
        match lattica.call(peer_id, "ExampleService.echo".to_string(), request_data).await {
            Ok(response) => {
                let echo_response: EchoResponse = bincode::decode_from_slice(&response.data, bincode::config::standard())
                    .map(|(data, _)| data)?;
                println!("Echo response: {:?}", echo_response);
            }
            Err(e) => println!("Echo RPC failed: {}", e),
        }


        println!("\n=== Testing Calculate RPC ===");
        let calc_request = CalculateRequest {
            a: 15,
            b: 3,
            operation: "multiply".to_string(),
        };
        let request_data = bincode::encode_to_vec(&calc_request, bincode::config::standard())?;

        match lattica.call(peer_id, "ExampleService.calculate".to_string(), request_data).await {
            Ok(response) => {
                let calc_response: CalculateResponse = bincode::decode_from_slice(&response.data, bincode::config::standard())
                    .map(|(data, _)| data)?;
                println!("Calculate response: {:?}", calc_response);
            }
            Err(e) => println!("Calculate RPC failed: {}", e),
        }

        println!("\n=== Testing Counter Stream RPC ===");
        let count: u32 = 5;
        let request_data = bincode::encode_to_vec(&count, bincode::config::standard())?;

        match lattica.call_stream(peer_id, "ExampleService.counter".to_string(), request_data).await {
            Ok(mut data) => {
                println!("Receiving stream messages:");
                let content: String = bincode::decode_from_slice(&data, bincode::config::standard())
                    .map(|(data, _)| data)
                    .unwrap_or_else(|_| "Invalid message".to_string());
                println!("Content: {}", content);
            }
            Err(e) => println!("Stream RPC failed: {}", e),
        }

        println!("\nAll RPC calls completed!");
    }


    signal::ctrl_c().await.expect("failed to run");

    let peers = lattica.get_all_peers().await;
    println!("Peers: {:?}", peers);
    for peer in peers {
        let addresses = lattica.get_peer_addresses(&peer).await;
        println!("Peer: {:?}, Addresses: {:?}", peer, addresses);
        let rtt = lattica.get_peer_rtt(&peer).await;
        println!("Peer: {:?}, RTT: {:?}", peer, rtt);
        let peer_info = lattica.get_peer_info(&peer).await;
        println!("Peer: {:?}, PeerInfo: {:?}", peer, peer_info);
    }

    Ok(())
}