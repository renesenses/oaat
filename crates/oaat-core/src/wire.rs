use crate::error::OaatError;
use crate::format::AudioFormat;

pub const AUDIO_HEADER_SIZE: usize = 32;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PacketFlags: u8 {
        const FIRST_PACKET = 0x01;
        const LAST_PACKET = 0x02;
        const FEC = 0x04;
        const FORMAT_CHANGE = 0x08;
    }
}

/// Audio packet header (32 bytes, network byte order).
///
/// ```text
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |  Ver  | Flags |    Format     |        Sequence (u16 BE)      |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     Stream ID (u32 BE)                        |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |              Presentation Timestamp (u64 BE, ns)              |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                    Sample Offset (u64 BE)                     |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |     Payload Length (u16 BE)    | FEC grp size  |  FEC index   |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |     FEC length XOR (u16 BE)    |       Reserved (u16)         |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// FEC fields (all zero when FEC is disabled):
/// - `fec_group_size`: number of data packets per parity packet (2-16).
/// - `fec_index`: this data packet's position within its group
///   (0..group_size-1). Unused (0) on parity packets.
/// - `fec_len_xor`: on parity packets only, XOR of the `payload_len` of all
///   data packets in the group — lets the receiver restore the exact length
///   of a recovered packet (payloads are XORed zero-extended to the longest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacketHeader {
    pub version: u8,
    pub flags: PacketFlags,
    pub format: AudioFormat,
    pub sequence: u16,
    pub stream_id: u32,
    pub pts_ns: u64,
    pub sample_offset: u64,
    pub payload_len: u16,
    pub fec_group_size: u8,
    pub fec_index: u8,
    pub fec_len_xor: u16,
}

impl AudioPacketHeader {
    pub const CURRENT_VERSION: u8 = 1;

    pub fn encode(&self, buf: &mut [u8; AUDIO_HEADER_SIZE]) {
        let ver_flags = (self.version << 4) | self.flags.bits();
        buf[0] = ver_flags;
        buf[1] = self.format.wire_id();
        buf[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        buf[4..8].copy_from_slice(&self.stream_id.to_be_bytes());
        buf[8..16].copy_from_slice(&self.pts_ns.to_be_bytes());
        buf[16..24].copy_from_slice(&self.sample_offset.to_be_bytes());
        buf[24..26].copy_from_slice(&self.payload_len.to_be_bytes());
        buf[26] = self.fec_group_size;
        buf[27] = self.fec_index;
        buf[28..30].copy_from_slice(&self.fec_len_xor.to_be_bytes());
        buf[30..32].fill(0);
    }

    pub fn decode(buf: &[u8; AUDIO_HEADER_SIZE]) -> Result<Self, OaatError> {
        let version = buf[0] >> 4;
        if version != Self::CURRENT_VERSION {
            return Err(OaatError::VersionMismatch {
                expected: Self::CURRENT_VERSION as u32,
                got: version as u32,
            });
        }
        let flags = PacketFlags::from_bits(buf[0] & 0x0F)
            .ok_or(OaatError::InvalidPacketFlags(buf[0] & 0x0F))?;
        let format = AudioFormat::from_wire_id(buf[1]).ok_or(OaatError::UnknownFormat(buf[1]))?;
        let sequence = u16::from_be_bytes([buf[2], buf[3]]);
        let stream_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let pts_ns = u64::from_be_bytes(buf[8..16].try_into().unwrap());
        let sample_offset = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let payload_len = u16::from_be_bytes([buf[24], buf[25]]);
        let fec_group_size = buf[26];
        let fec_index = buf[27];
        let fec_len_xor = u16::from_be_bytes([buf[28], buf[29]]);

        Ok(Self {
            version,
            flags,
            format,
            sequence,
            stream_id,
            pts_ns,
            sample_offset,
            payload_len,
            fec_group_size,
            fec_index,
            fec_len_xor,
        })
    }
}

/// Clock sync packet (28 bytes).
/// Ver (4 bits) + Type (4 bits) + Sequence (u16) + T1/T2/T3 (u64 each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSyncPacket {
    pub version: u8,
    pub kind: ClockSyncType,
    pub sequence: u16,
    pub t1: u64,
    pub t2: u64,
    pub t3: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSyncType {
    Request = 0x01,
    Response = 0x02,
}

impl ClockSyncPacket {
    pub const SIZE: usize = 28;

    pub fn encode(&self, buf: &mut [u8; Self::SIZE]) {
        let ver_type = (self.version << 4) | (self.kind as u8);
        buf[0] = ver_type;
        buf[1] = 0; // reserved
        buf[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        buf[4..12].copy_from_slice(&self.t1.to_be_bytes());
        buf[12..20].copy_from_slice(&self.t2.to_be_bytes());
        buf[20..28].copy_from_slice(&self.t3.to_be_bytes());
    }

    pub const CURRENT_VERSION: u8 = 1;

    pub fn decode(buf: &[u8; Self::SIZE]) -> Result<Self, OaatError> {
        let version = buf[0] >> 4;
        if version != Self::CURRENT_VERSION {
            return Err(OaatError::VersionMismatch {
                expected: Self::CURRENT_VERSION as u32,
                got: version as u32,
            });
        }
        let kind = match buf[0] & 0x0F {
            0x01 => ClockSyncType::Request,
            0x02 => ClockSyncType::Response,
            other => return Err(OaatError::InvalidClockSyncType(other)),
        };
        let sequence = u16::from_be_bytes([buf[2], buf[3]]);
        let t1 = u64::from_be_bytes(buf[4..12].try_into().unwrap());
        let t2 = u64::from_be_bytes(buf[12..20].try_into().unwrap());
        let t3 = u64::from_be_bytes(buf[20..28].try_into().unwrap());

        Ok(Self {
            version,
            kind,
            sequence,
            t1,
            t2,
            t3,
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_header_roundtrip() {
        let hdr = AudioPacketHeader {
            version: 1,
            flags: PacketFlags::FIRST_PACKET | PacketFlags::FEC,
            format: AudioFormat::PcmS24le,
            sequence: 42,
            stream_id: 0xDEADBEEF,
            pts_ns: 1_000_000_000,
            sample_offset: 192000,
            payload_len: 1440,
            fec_group_size: 8,
            fec_index: 3,
            fec_len_xor: 0,
        };
        let mut buf = [0u8; AUDIO_HEADER_SIZE];
        hdr.encode(&mut buf);
        let decoded = AudioPacketHeader::decode(&buf).unwrap();
        assert_eq!(hdr, decoded);
    }

    #[test]
    fn clock_sync_roundtrip() {
        let pkt = ClockSyncPacket {
            version: 1,
            kind: ClockSyncType::Response,
            sequence: 7,
            t1: 100_000,
            t2: 100_050,
            t3: 100_060,
        };
        let mut buf = [0u8; ClockSyncPacket::SIZE];
        pkt.encode(&mut buf);
        let decoded = ClockSyncPacket::decode(&buf).unwrap();
        assert_eq!(pkt, decoded);
    }

    #[test]
    fn audio_header_rejects_unknown_version() {
        let hdr = AudioPacketHeader {
            version: 1,
            flags: PacketFlags::empty(),
            format: AudioFormat::PcmS16le,
            sequence: 1,
            stream_id: 1,
            pts_ns: 0,
            sample_offset: 0,
            payload_len: 0,
            fec_group_size: 0,
            fec_index: 0,
            fec_len_xor: 0,
        };
        let mut buf = [0u8; AUDIO_HEADER_SIZE];
        hdr.encode(&mut buf);
        buf[0] = (2 << 4) | (buf[0] & 0x0F); // forge version 2
        assert!(matches!(
            AudioPacketHeader::decode(&buf),
            Err(OaatError::VersionMismatch { expected: 1, got: 2 })
        ));
    }

    #[test]
    fn clock_sync_rejects_unknown_version() {
        let pkt = ClockSyncPacket {
            version: 1,
            kind: ClockSyncType::Request,
            sequence: 1,
            t1: 1,
            t2: 0,
            t3: 0,
        };
        let mut buf = [0u8; ClockSyncPacket::SIZE];
        pkt.encode(&mut buf);
        buf[0] = (3 << 4) | (buf[0] & 0x0F); // forge version 3
        assert!(matches!(
            ClockSyncPacket::decode(&buf),
            Err(OaatError::VersionMismatch { expected: 1, got: 3 })
        ));
    }
}
