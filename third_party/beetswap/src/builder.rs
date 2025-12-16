use std::sync::Arc;

use blockstore::Blockstore;

use crate::client::{ClientBehaviour, ClientConfig};
use crate::multihasher::{Multihasher, MultihasherTable};
use crate::server::ServerBehaviour;
use crate::utils::stream_protocol;
use crate::{Behaviour, Error, Result};

/// Builder for [`Behaviour`].
///
/// # Example
///
/// ```rust,no_run
/// # use std::sync::Arc;
/// # use blockstore::InMemoryBlockstore;
/// # fn new() -> beetswap::Behaviour<64, InMemoryBlockstore<64>> {
/// beetswap::Behaviour::builder(Arc::new(InMemoryBlockstore::new()))
///     .build()
/// # }
pub struct BehaviourBuilder<const S: usize, B>
where
    B: Blockstore + 'static,
{
    protocol_prefix: Option<String>,
    blockstore: Arc<B>,
    client: ClientConfig,
    multihasher: MultihasherTable<S>,
}

impl<const S: usize, B> BehaviourBuilder<S, B>
where
    B: Blockstore + 'static,
{
    /// Creates a new builder for [`Behaviour`].
    pub(crate) fn new(blockstore: Arc<B>) -> Self {
        BehaviourBuilder {
            protocol_prefix: None,
            blockstore,
            client: ClientConfig {
                set_send_dont_have: true,
            },
            multihasher: MultihasherTable::<S>::new(),
        }
    }

    /// Set a prefix on the protocol name.
    ///
    /// The prefix will be added on `/ipfs/bitswap/1.2.0`.
    ///
    /// # Errors
    ///
    /// This function will return an error if `prefix` does not start with a forward slash (`/`).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use blockstore::InMemoryBlockstore;
    /// # fn new() -> beetswap::Result<beetswap::Behaviour<64, InMemoryBlockstore<64>>> {
    /// #   Ok(
    /// beetswap::Behaviour::builder(Arc::new(InMemoryBlockstore::new()))
    ///     .protocol_prefix("/celestia/celestia")?
    ///     .build()
    /// #   )
    /// # }
    /// ```
    pub fn protocol_prefix(mut self, prefix: &str) -> Result<Self> {
        if !prefix.starts_with('/') {
            return Err(Error::InvalidProtocolPrefix(prefix.to_owned()));
        }

        self.protocol_prefix = Some(prefix.to_owned());
        Ok(self)
    }

    /// Client will set `send_dont_have` flag on each query (enabled by default).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use blockstore::InMemoryBlockstore;
    /// # fn new() -> beetswap::Behaviour<64, InMemoryBlockstore<64>> {
    /// beetswap::Behaviour::builder(Arc::new(InMemoryBlockstore::new()))
    ///     .client_set_send_dont_have(false)
    ///     .build()
    /// # }
    pub fn client_set_send_dont_have(mut self, enable: bool) -> Self {
        self.client.set_send_dont_have = enable;
        self
    }

    /// Register an extra [`Multihasher`].
    ///
    /// Every registration adds new hasher to [`Behaviour`]. Hashers are used to
    /// reconstruct the [`Cid`] from the received data. `Behaviour` will try them
    /// in the reverse order they were registered until one successfully constructs
    /// [`Multihash`].
    ///
    /// By default `BehaviourBuilder` is pre-loaded with [`StandardMultihasher`].
    ///
    /// [`StandardMultihasher`]: crate::multihasher::StandardMultihasher
    /// [`Cid`]: cid::CidGeneric
    /// [`Multihash`]: libp2p_core::multihash::Multihash
    pub fn register_multihasher<M>(mut self, multihasher: M) -> Self
    where
        M: Multihasher<S> + Send + Sync + 'static,
    {
        self.multihasher.register(multihasher);
        self
    }

    /// Build a [`Behaviour`].
    pub fn build(self) -> Behaviour<S, B> {
        let blockstore = self.blockstore;
        let multihasher = Arc::new(self.multihasher);
        let protocol_prefix = self.protocol_prefix.as_deref();

        Behaviour {
            protocol: stream_protocol(protocol_prefix, "/ipfs/bitswap/1.2.0")
                .expect("prefix checked by beetswap::BehaviourBuilder::protocol_prefix"),
            client: ClientBehaviour::new(self.client, blockstore.clone(), protocol_prefix),
            server: ServerBehaviour::new(blockstore, protocol_prefix),
            multihasher,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockstore::InMemoryBlockstore;

    #[test]
    fn invalid_protocol_prefix() {
        assert!(matches!(
            BehaviourBuilder::<64, _>::new(Arc::new(InMemoryBlockstore::<64>::new()))
                .protocol_prefix("foo"),
            Err(Error::InvalidProtocolPrefix(_))
        ));
    }
}
