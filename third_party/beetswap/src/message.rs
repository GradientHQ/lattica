use std::io;

use asynchronous_codec::{Decoder, Encoder};
use bytes::{Buf, BytesMut};
use cid::CidGeneric;
use quick_protobuf::{BytesReader, BytesWriter, MessageWrite, Writer, WriterBackend};

use crate::proto::message::mod_Message::mod_Wantlist::{Entry, WantType};
use crate::proto::message::Message;

/// Bitswap spec defines maximum `Message` size to 4MiB.
pub(crate) const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

pub(crate) struct Codec;

impl Encoder for Codec {
    type Item<'a> = &'a Message;
    type Error = io::Error;

    fn encode(&mut self, msg: &Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut varint_buf = unsigned_varint::encode::usize_buffer();
        let varint = unsigned_varint::encode::usize(msg.get_size(), &mut varint_buf);

        let prev_len = dst.len();
        dst.resize(dst.len() + varint.len() + msg.get_size(), 0);

        let mut bytes_writer = BytesWriter::new(&mut dst[prev_len..]);
        bytes_writer.pb_write_all(varint).expect("buffer too small");

        let mut writer = Writer::new(bytes_writer);
        msg.write_message(&mut writer).expect("buffer too small");

        Ok(())
    }
}

impl Decoder for Codec {
    type Item = Message;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Ok((len, rest)) = unsigned_varint::decode::usize(&src[..]) else {
            return Ok(None);
        };

        let varint_len = src.len() - rest.len();

        if varint_len > MAX_MESSAGE_SIZE {
            return Err(io::Error::other("Message too large"));
        }

        if rest.len() < len {
            return Ok(None);
        }

        let mut reader = BytesReader::from_bytes(rest);

        let msg = reader
            .read_message_by_len(rest, len)
            .map_err(io::Error::other)?;

        src.advance(varint_len + len);

        Ok(Some(msg))
    }
}

pub(crate) fn new_want_block_entry<const S: usize>(
    cid: &CidGeneric<S>,
    set_send_dont_have: bool,
) -> Entry {
    Entry {
        block: cid.to_bytes(),
        priority: 1,
        wantType: WantType::Block,
        sendDontHave: set_send_dont_have,
        ..Default::default()
    }
}

pub(crate) fn new_want_have_entry<const S: usize>(
    cid: &CidGeneric<S>,
    set_send_dont_have: bool,
) -> Entry {
    Entry {
        block: cid.to_bytes(),
        priority: 1,
        wantType: WantType::Have,
        sendDontHave: set_send_dont_have,
        ..Default::default()
    }
}

pub(crate) fn new_cancel_entry<const S: usize>(cid: &CidGeneric<S>) -> Entry {
    Entry {
        block: cid.to_bytes(),
        cancel: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::message::mod_Message::Block;
    use crate::proto::message::mod_Message::Wantlist;
    use bytes::BufMut;

    fn response_sample() -> Message {
        Message {
            payload: vec![Block {
                prefix: vec![1, 85, 18, 32],
                data: vec![97, 98, 99],
            }],
            ..Default::default()
        }
    }

    fn response_sample_bytes() -> Vec<u8> {
        hex::decode("0d1a0b0a04015512201203616263").unwrap()
    }

    fn request_sample() -> Message {
        Message {
            wantlist: Some(Wantlist {
                entries: vec![Entry {
                    block: vec![
                        1, 85, 18, 32, 186, 120, 22, 191, 143, 1, 207, 234, 65, 65, 64, 222, 93,
                        174, 34, 35, 176, 3, 97, 163, 150, 23, 122, 156, 180, 16, 255, 97, 242, 0,
                        21, 173,
                    ],
                    priority: 1,
                    sendDontHave: true,
                    ..Default::default()
                }],
                full: false,
            }),
            ..Default::default()
        }
    }

    fn request_sample_bytes() -> Vec<u8> {
        hex::decode("2e0a2c0a2a0a2401551220ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad10012801").unwrap()
    }

    #[test]
    fn encode() {
        let mut codec = Codec;
        let mut buf = BytesMut::new();

        codec.encode(&request_sample(), &mut buf).unwrap();
        assert_eq!(buf, request_sample_bytes());

        codec.encode(&response_sample(), &mut buf).unwrap();
        assert_eq!(
            &buf[..],
            [request_sample_bytes(), response_sample_bytes()].concat()
        );
    }

    #[test]
    fn decode() {
        let mut codec = Codec;
        let mut buf = BytesMut::new();

        buf.put_slice(&request_sample_bytes());
        buf.put_slice(&response_sample_bytes());

        let msg = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, request_sample());

        let msg = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(msg, response_sample());
    }
}
