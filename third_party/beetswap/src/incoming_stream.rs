use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use asynchronous_codec::FramedRead;
use cid::CidGeneric;
use fnv::FnvHashMap;
use futures_util::future::{BoxFuture, Fuse, FusedFuture, FutureExt};
use futures_util::stream::StreamExt;
use tracing::debug;

use crate::cid_prefix::CidPrefix;
use crate::message::Codec;
use crate::multihasher::{MultihasherError, MultihasherTable};
use crate::proto::message::mod_Message::BlockPresenceType;
use crate::proto::message::mod_Message::Wantlist;
use crate::proto::message::Message;

/// Stream that reads `Message` and converts it to `IncomingMessage`.
///
/// On fatal errors `None` is returned which instruct `SelectAll` to drop the stream.
pub(crate) struct IncomingStream<const S: usize> {
    multihasher: Arc<MultihasherTable<S>>,
    stream: FramedRead<libp2p_swarm::Stream, Codec>,
    processing: Fuse<BoxFuture<'static, Option<IncomingMessage<S>>>>,
}

#[derive(Debug, Default)]
pub(crate) struct ClientMessage<const S: usize> {
    pub(crate) block_presences: FnvHashMap<CidGeneric<S>, BlockPresenceType>,
    pub(crate) blocks: FnvHashMap<CidGeneric<S>, Vec<u8>>,
}

#[derive(Debug, Default)]
pub(crate) struct ServerMessage {
    pub(crate) wantlist: Wantlist,
}

#[derive(Debug, Default)]
#[doc(hidden)]
pub struct IncomingMessage<const S: usize> {
    pub(crate) client: Option<ClientMessage<S>>,
    pub(crate) server: Option<ServerMessage>,
}

impl<const S: usize> IncomingStream<S> {
    pub(crate) fn new(stream: libp2p_swarm::Stream, multihasher: Arc<MultihasherTable<S>>) -> Self {
        IncomingStream {
            multihasher,
            stream: FramedRead::new(stream, Codec),
            processing: Fuse::terminated(),
        }
    }
}

impl<const S: usize> fmt::Debug for IncomingStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IncomingStream { .. }")
    }
}

impl<const S: usize> futures_core::stream::Stream for IncomingStream<S> {
    type Item = IncomingMessage<S>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<IncomingMessage<S>>> {
        loop {
            // If processing future is activated then poll it and return
            // its result.
            if !self.processing.is_terminated() {
                match self.processing.poll_unpin(cx) {
                    Poll::Ready(Some(msg)) => {
                        // There is no need to forward an empty message.
                        if msg.client.is_some() || msg.server.is_some() {
                            return Poll::Ready(Some(msg));
                        }
                    }
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Receive a decoded `Message` from underlying stream.
            let msg = match self.stream.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(msg))) => msg,
                Poll::Ready(Some(Err(e))) => {
                    debug!("Message decoding failed: {e}");
                    return Poll::Ready(None);
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            };

            // Create a future that processes the message and converts it to `IncomingMessage`.
            self.processing = process_message(self.multihasher.clone(), msg)
                .boxed()
                .fuse();
        }
    }
}

/// Convert `Message` to `IncomingMessage`. On fatal error this function
/// returns `None` which will cause the underlying stream to close.
///
/// We consider non-decodable messages as fatal.
async fn process_message<const S: usize>(
    multihasher: Arc<MultihasherTable<S>>,
    mut msg: Message,
) -> Option<IncomingMessage<S>> {
    let mut incoming_msg = IncomingMessage::default();

    for block_presence in msg.blockPresences {
        let cid = match CidGeneric::try_from(&block_presence.cid[..]) {
            Ok(cid) => cid,
            Err(e) => {
                debug!("Invalid CID bytes: {}: {:?}", e, block_presence.cid);
                return None;
            }
        };

        incoming_msg
            .client
            .get_or_insert_with(ClientMessage::default)
            .block_presences
            .insert(cid, block_presence.type_pb);
    }

    for payload in msg.payload {
        let Some(cid_prefix) = CidPrefix::from_bytes(&payload.prefix) else {
            debug!("block.prefix not decodable: {:?}", payload.prefix);
            return None;
        };

        let cid = match cid_prefix.to_cid(&multihasher, &payload.data).await {
            Ok(cid) => cid,
            Err(MultihasherError::UnknownMultihashCode) => {
                debug!(
                    "Multihasher non-fatal error: Unknown multihash code: {}",
                    cid_prefix.multihash_code()
                );
                continue;
            }
            Err(MultihasherError::Custom(e)) => {
                debug!("Multihasher non-fatal error: {e}");
                continue;
            }
            // Any other error is fatal and we need to close the stream.
            Err(e) => {
                debug!("Multihasher fatal error: {e}");
                return None;
            }
        };

        incoming_msg
            .client
            .get_or_insert_with(ClientMessage::default)
            .blocks
            .insert(cid, payload.data);
    }

    if let Some(wantlist) = msg.wantlist.take() {
        // If wantlist is marked as full then we accept it even if it's empty.
        // Otherwise we accept wantlists that have 1 or more entries.
        if wantlist.full || !wantlist.entries.is_empty() {
            incoming_msg
                .server
                .get_or_insert_with(ServerMessage::default)
                .wantlist = wantlist;
        }
    }

    Some(incoming_msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::message::mod_Message::mod_Wantlist::{Entry, WantType};
    use crate::proto::message::mod_Message::{Block, BlockPresence};
    use crate::test_utils::cid_of_data;

    #[tokio::test]
    async fn parse_client_message() {
        let cid1 = cid_of_data(b"x1");
        let cid2 = cid_of_data(b"x2");
        let cid3 = cid_of_data(b"x3");
        let cid4 = cid_of_data(b"x4");

        let proto_msg = Message {
            payload: vec![
                Block {
                    prefix: CidPrefix::from_cid(&cid1).to_bytes(),
                    data: b"x1".to_vec(),
                },
                Block {
                    prefix: CidPrefix::from_cid(&cid2).to_bytes(),
                    data: b"x2".to_vec(),
                },
            ],
            blockPresences: vec![
                BlockPresence {
                    cid: cid3.to_bytes(),
                    type_pb: BlockPresenceType::Have,
                },
                BlockPresence {
                    cid: cid4.to_bytes(),
                    type_pb: BlockPresenceType::DontHave,
                },
            ],
            ..Default::default()
        };

        let multihasher = Arc::new(MultihasherTable::new());
        let msg = process_message(multihasher.clone(), proto_msg)
            .await
            .unwrap();

        let client_msg = msg.client.unwrap();
        assert!(msg.server.is_none());

        assert_eq!(client_msg.blocks.get(&cid1).unwrap(), b"x1");
        assert_eq!(client_msg.blocks.get(&cid2).unwrap(), b"x2");

        assert_eq!(
            client_msg.block_presences.get(&cid3).unwrap(),
            &BlockPresenceType::Have
        );
        assert_eq!(
            client_msg.block_presences.get(&cid4).unwrap(),
            &BlockPresenceType::DontHave
        );
    }

    #[tokio::test]
    async fn parse_server_message() {
        let cid1 = cid_of_data(b"x1");
        let cid2 = cid_of_data(b"x2");

        let proto_msg = Message {
            wantlist: Some(Wantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                ],
                full: false,
            }),
            ..Default::default()
        };

        let multihasher = Arc::new(MultihasherTable::<64>::new());
        let msg = process_message(multihasher, proto_msg.clone())
            .await
            .unwrap();

        let server_msg = msg.server.unwrap();
        assert!(msg.client.is_none());

        assert_eq!(proto_msg.wantlist.unwrap(), server_msg.wantlist);
    }

    #[tokio::test]
    async fn parse_server_message_without_entries() {
        let proto_msg = Message {
            wantlist: Some(Wantlist {
                entries: Vec::new(),
                full: false,
            }),
            ..Default::default()
        };

        let multihasher = Arc::new(MultihasherTable::<64>::new());
        let msg = process_message(multihasher, proto_msg).await.unwrap();

        assert!(msg.server.is_none());
        assert!(msg.client.is_none());
    }

    #[tokio::test]
    async fn parse_server_message_full_without_entries() {
        let proto_msg = Message {
            wantlist: Some(Wantlist {
                entries: Vec::new(),
                full: true,
            }),
            ..Default::default()
        };

        let multihasher = Arc::new(MultihasherTable::<64>::new());
        let msg = process_message(multihasher, proto_msg.clone())
            .await
            .unwrap();

        let server_msg = msg.server.unwrap();
        assert!(msg.client.is_none());

        assert_eq!(proto_msg.wantlist.unwrap(), server_msg.wantlist);
    }
}
