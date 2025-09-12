use serde::{Serialize, Deserialize};
use bincode::{Encode, Decode};
use blockstore::block::{Block, CidError};
use cid::{CidGeneric, Cid};
use multihash_codetable::{Code, MultihashDigest};

const RAW_CODEC: u64 = 0x55;

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct SubkeyIndex {
    pub subkeys: Vec<String>,
    pub expiration: f64,
}

#[derive(Debug, Clone)]
pub enum DhtValue {
    Simple {
        value: Vec<u8>,
        expiration: f64,
    },
    WithSubkeys {
        subkeys: Vec<(String, Vec<u8>, f64)>, // (subkey, value, expiration)
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
pub struct ValueWithTime {
    pub value: Vec<u8>,
    pub expiration_time: f64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct QueryId(InnerQueryId);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InnerQueryId {
    Bitswap(beetswap::QueryId),
    Kad(libp2p::kad::QueryId),
}

impl From<beetswap::QueryId> for QueryId {
    fn from(id: beetswap::QueryId) -> Self {
        Self(InnerQueryId::Bitswap(id))
    }
}

impl From<libp2p::kad::QueryId> for QueryId {
    fn from(id: libp2p::kad::QueryId) -> Self {
        Self(InnerQueryId::Kad(id))
    }
}


#[derive(Debug, Clone)]
pub struct BytesBlock(pub Vec<u8>);
impl Block<64> for BytesBlock {
    fn cid(&self) -> Result<CidGeneric<64>, CidError> {
        let hash = Code::Sha2_256.digest(&self.0);
        Ok(Cid::new_v1(RAW_CODEC, hash))
    }
    fn data(&self) -> &[u8] {
        &self.0
    }
}