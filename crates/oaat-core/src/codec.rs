use crate::error::OaatError;
use crate::message::Message;
use bytes::{Buf, BytesMut};

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

pub struct FrameCodec {
    buf: BytesMut,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8192),
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn decode_next(&mut self) -> Result<Option<Message>, OaatError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }

        let len = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;

        if len > MAX_FRAME_SIZE {
            return Err(OaatError::MessageTooLarge(len));
        }

        if self.buf.len() < 4 + len {
            return Ok(None);
        }

        self.buf.advance(4);
        let json_bytes = self.buf.split_to(len);
        let msg = Message::decode_json(&json_bytes)?;
        Ok(Some(msg))
    }

    pub fn encode(msg: &Message) -> Vec<u8> {
        msg.encode_framed()
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Hello;

    #[test]
    fn codec_feed_and_decode() {
        let msg = Message::Hello(Hello {
            protocol_version: 1,
            controller_id: "ctrl-1".into(),
            controller_name: "Test".into(),
            clock_port: 9742,
            features: vec![],
        });

        let frame = FrameCodec::encode(&msg);
        let mut codec = FrameCodec::new();

        // Feed in two parts to test buffering
        let mid = frame.len() / 2;
        codec.feed(&frame[..mid]);
        assert!(codec.decode_next().unwrap().is_none());

        codec.feed(&frame[mid..]);
        let decoded = codec.decode_next().unwrap().unwrap();
        match decoded {
            Message::Hello(h) => assert_eq!(h.controller_id, "ctrl-1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn codec_multiple_messages() {
        let mut codec = FrameCodec::new();
        for i in 0..5 {
            let msg = Message::Hello(Hello {
                protocol_version: 1,
                controller_id: format!("ctrl-{i}"),
                controller_name: "Test".into(),
                clock_port: 9742,
                features: vec![],
            });
            codec.feed(&FrameCodec::encode(&msg));
        }

        for i in 0..5 {
            let decoded = codec.decode_next().unwrap().unwrap();
            match decoded {
                Message::Hello(h) => assert_eq!(h.controller_id, format!("ctrl-{i}")),
                _ => panic!("wrong variant"),
            }
        }
        assert!(codec.decode_next().unwrap().is_none());
    }
}
