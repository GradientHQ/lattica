//! Helpers used in and provided by the crate

use std::future::Future;

use cid::CidGeneric;
use futures_util::future::FutureExt;
use libp2p_core::multihash::Multihash;
use libp2p_swarm::StreamProtocol;

/// Helper utility that converts `CidGeneric<S>` to `CidGeneric<NEW_S>`
pub fn convert_cid<const S: usize, const NEW_S: usize>(
    cid: &CidGeneric<S>,
) -> Option<CidGeneric<NEW_S>> {
    let hash = convert_multihash(cid.hash())?;
    CidGeneric::new(cid.version(), cid.codec(), hash).ok()
}

/// Helper utility that converts `Multihash<S>` to `Multihash<NEW_S>`
pub fn convert_multihash<const S: usize, const NEW_S: usize>(
    hash: &Multihash<S>,
) -> Option<Multihash<NEW_S>> {
    Multihash::<NEW_S>::wrap(hash.code(), hash.digest()).ok()
}

pub(crate) fn stream_protocol(
    prefix: Option<&str>,
    protocol: &'static str,
) -> Option<StreamProtocol> {
    match prefix {
        Some(prefix) => StreamProtocol::try_from_owned(format!("{prefix}{protocol}")).ok(),
        None => Some(StreamProtocol::new(protocol)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type BoxFuture<'a, T> = futures_core::future::BoxFuture<'a, T>;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn box_future<'a, F, T>(f: F) -> BoxFuture<'a, T>
where
    F: Future<Output = T> + Send + 'a,
{
    f.boxed()
}

#[cfg(target_arch = "wasm32")]
pub(crate) type BoxFuture<'a, T> = futures_core::future::LocalBoxFuture<'a, T>;

#[cfg(target_arch = "wasm32")]
pub(crate) fn box_future<'a, F, T>(f: F) -> BoxFuture<'a, T>
where
    F: Future<Output = T> + Sized + 'a,
{
    f.boxed_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::RAW_CODEC;
    use multihash_codetable::{Code, MultihashDigest};

    #[test]
    fn convert_cid_len() {
        let hash = Code::Sha2_256.digest(&[]);
        let cid = CidGeneric::<64>::new_v1(RAW_CODEC, hash);

        assert!(convert_cid::<64, 32>(&cid).is_some());
        assert!(convert_cid::<64, 31>(&cid).is_none());
    }

    #[test]
    fn convert_multihash_len() {
        let hash = Code::Sha2_256.digest(&[]);

        assert!(convert_multihash::<64, 32>(&hash).is_some());
        assert!(convert_multihash::<64, 31>(&hash).is_none());
    }
}
