use std::borrow::Cow;
use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode};
use std::fmt;
use async_trait::async_trait;
use std::io;
use futures::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use libp2p::request_response::{Codec};
use libp2p::{StreamProtocol};
use std::fmt::Debug;
use std::sync::Arc;
use crate::common::CompressionAlgorithm;

pub const RPC_REQUEST_RESPONSE_PROTOCOL: StreamProtocol = StreamProtocol::new("/rpc/req-resp/1.0.0");
pub const RPC_STREAM_PROTOCOL: StreamProtocol = StreamProtocol::new("/rpc/stream/1.0.0");

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct RpcRequest {
    pub id: String,
    pub method: String,
    pub data: Vec<u8>,
    pub compression: Option<CompressionAlgorithm>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct RpcResponse {
    pub id: String,
    pub data: Vec<u8>,
    pub compression: Option<CompressionAlgorithm>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct RpcError {
    pub id: String,
    pub error: String,
}


#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub enum RpcMessage {
    Request(RpcRequest),
    Response(RpcResponse),
    Error(RpcError),
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct StreamRequest {
    pub id: String,
    pub method: String,
    pub data: Arc<[u8]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct StreamResponse {
    pub id: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct StreamMessage<'a> {
    pub id: String,
    pub data: Cow<'a, [u8]>,
    pub is_end: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct StreamError {
    pub id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub enum StreamFrame<'a> {
    Request(StreamRequest),
    Data(StreamMessage<'a>),
    Error(StreamError),
    Close(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RPC Error [{}]: {}", self.id, self.error)
    }
}
impl std::error::Error for RpcError {}
pub type RpcResult<T> = Result<T, String>;

#[async_trait]
pub trait RpcService: Send + Sync {
    fn service_name(&self) -> &str;
    fn methods(&self) -> Vec<String>;
    async fn handle_request(&self, method: &str, request: RpcRequest) -> RpcResult<RpcResponse>;
    async fn handle_stream(&self, method: &str, request: StreamRequest) -> RpcResult<StreamResponse>;
}

#[derive(Debug, Clone, Default)]
pub struct RpcCodec;

#[async_trait]
impl Codec for RpcCodec {
    type Protocol = StreamProtocol;
    type Request = RpcMessage;
    type Response = RpcMessage;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::decode_from_slice(&buf, bincode::config::standard()).map(|(data, _)| data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::decode_from_slice(&buf, bincode::config::standard()).map(|(data, _)| data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(&mut self, _: &Self::Protocol, io: &mut T, req: Self::Request) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = bincode::encode_to_vec(&req, bincode::config::standard()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await?;
        io.close().await
    }

    async fn write_response<T>(&mut self, _: &Self::Protocol, io: &mut T, res: Self::Response) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        let data = bincode::encode_to_vec(&res, bincode::config::standard()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await?;
        io.close().await
    }
}
