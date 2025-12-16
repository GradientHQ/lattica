use cid::{CidGeneric, Version};
use tracing::debug;

use crate::multihasher::{MultihasherError, MultihasherTable};

const DAG_PB: u64 = 0x70;
const SHA2_256: u64 = 0x12;
const SHA2_256_SIZE: usize = 0x20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CidPrefix {
    version: Version,
    codec: u64,
    multihash_code: u64,
    multihash_size: usize,
}

impl CidPrefix {
    pub(crate) fn from_cid<const S: usize>(cid: &CidGeneric<S>) -> CidPrefix {
        CidPrefix {
            version: cid.version(),
            codec: cid.codec(),
            multihash_code: cid.hash().code(),
            multihash_size: cid.hash().size().into(),
        }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<CidPrefix> {
        let (raw_version, rest) = unsigned_varint::decode::u64(bytes).ok()?;
        let (codec, rest) = unsigned_varint::decode::u64(rest).ok()?;

        // CIDv0 is a naked multihash that with fixed code and size
        if raw_version == SHA2_256 && codec == SHA2_256_SIZE as u64 {
            return Some(CidPrefix {
                version: Version::V0,
                codec: DAG_PB,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            });
        }

        let version = Version::try_from(raw_version).ok()?;
        let (multihash_code, rest) = unsigned_varint::decode::u64(rest).ok()?;
        let (multihash_size, _rest) = unsigned_varint::decode::usize(rest).ok()?;

        Some(CidPrefix {
            version,
            codec,
            multihash_code,
            multihash_size,
        })
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        match self.version {
            // CIDv0 is a naked multihash that with fixed code and size
            Version::V0 => {
                let mut buf = unsigned_varint::encode::u64_buffer();
                let bytes1 = unsigned_varint::encode::u64(SHA2_256, &mut buf);

                let mut buf = unsigned_varint::encode::u64_buffer();
                let bytes2 = unsigned_varint::encode::u64(SHA2_256_SIZE as u64, &mut buf);

                [bytes1, bytes2].concat()
            }
            Version::V1 => {
                let mut buf = unsigned_varint::encode::u64_buffer();
                let version = unsigned_varint::encode::u64(self.version as u64, &mut buf);

                let mut buf = unsigned_varint::encode::u64_buffer();
                let codec = unsigned_varint::encode::u64(self.codec, &mut buf);

                let mut buf = unsigned_varint::encode::u64_buffer();
                let multihash_code = unsigned_varint::encode::u64(self.multihash_code, &mut buf);

                let mut buf = unsigned_varint::encode::usize_buffer();
                let multihash_size = unsigned_varint::encode::usize(self.multihash_size, &mut buf);

                [version, codec, multihash_code, multihash_size].concat()
            }
        }
    }

    pub(crate) async fn to_cid<const S: usize>(
        &self,
        hasher: &MultihasherTable<S>,
        data: &[u8],
    ) -> Result<CidGeneric<S>, MultihasherError> {
        if self.multihash_size > S {
            debug!(
                "Multihash<{}> can not hold multihash of size {}",
                S, self.multihash_size
            );
            return Err(MultihasherError::InvalidMultihashSize);
        }

        let hash = hasher.hash(self.multihash_code, data).await?;

        let cid = CidGeneric::new(self.version, self.codec, hash)
            .expect("prefix for cidv0 was initalized incorrectly");

        Ok(cid)
    }

    pub(crate) fn multihash_code(&self) -> u64 {
        self.multihash_code
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::RAW_CODEC;
    use cid::Cid;

    #[test]
    fn cid_v1_to_cid_prefix() {
        let cid = "bafkreigjh3xc2dnqf4ikzr2gbwkxnyjc3t4m2u6ex6g7zlq3hz2oxt77li"
            .parse::<Cid>()
            .unwrap();
        let cid_prefix = CidPrefix::from_cid(&cid);

        assert_eq!(
            cid_prefix,
            CidPrefix {
                version: Version::V1,
                codec: RAW_CODEC,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
        );
    }

    #[test]
    fn cid_v1_bytes_to_cid_prefix() {
        let cid = "bafkreigjh3xc2dnqf4ikzr2gbwkxnyjc3t4m2u6ex6g7zlq3hz2oxt77li"
            .parse::<Cid>()
            .unwrap();
        let cid_prefix = CidPrefix::from_bytes(&cid.to_bytes()).unwrap();

        assert_eq!(
            cid_prefix,
            CidPrefix {
                version: Version::V1,
                codec: RAW_CODEC,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
        );
    }

    #[test]
    fn cid_prefix_v1_to_bytes() {
        assert_eq!(
            CidPrefix {
                version: Version::V1,
                codec: RAW_CODEC,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
            .to_bytes(),
            [1, 0x55, SHA2_256 as u8, SHA2_256_SIZE as u8],
        );
    }

    #[test]
    fn cid_v0_to_cid_prefix() {
        let cid = "QmPK1s3pNYLi9ERiq3BDxKa4XosgWwFRQUydHUtz4YgpqB"
            .parse::<Cid>()
            .unwrap();
        let cid_prefix = CidPrefix::from_cid(&cid);

        assert_eq!(
            cid_prefix,
            CidPrefix {
                version: Version::V0,
                codec: DAG_PB,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
        );
    }

    #[test]
    fn cid_v0_bytes_to_cid_prefix() {
        let cid = "QmPK1s3pNYLi9ERiq3BDxKa4XosgWwFRQUydHUtz4YgpqB"
            .parse::<Cid>()
            .unwrap();
        let cid_prefix = CidPrefix::from_bytes(&cid.to_bytes()).unwrap();

        assert_eq!(
            cid_prefix,
            CidPrefix {
                version: Version::V0,
                codec: DAG_PB,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
        );
    }

    #[test]
    fn cid_prefix_v0_to_bytes() {
        assert_eq!(
            CidPrefix {
                version: Version::V0,
                codec: DAG_PB,
                multihash_code: SHA2_256,
                multihash_size: SHA2_256_SIZE,
            }
            .to_bytes(),
            [SHA2_256 as u8, SHA2_256_SIZE as u8],
        );
    }

    #[test]
    fn invalid_bytes() {
        assert!(CidPrefix::from_bytes(&[]).is_none());
        assert!(CidPrefix::from_bytes(&[1, 0x55]).is_none());
        assert!(CidPrefix::from_bytes(&[1, 0x55, 0x12]).is_none());
        assert!(CidPrefix::from_bytes(&[66, 0x55, 0x12, 32]).is_none());
        assert!(CidPrefix::from_bytes(&[0x12, 33]).is_none());
        assert!(CidPrefix::from_bytes(&[0x13, 32]).is_none());
    }
}
