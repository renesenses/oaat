use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudioFormat {
    PcmS16le,
    PcmS24le,
    PcmS24le4,
    PcmS32le,
    PcmF32le,
    DsdU8,
    DsdU16le,
    DsdU32le,
    Flac,
    Opus,
    TrueHd,
    Eac3,
}

impl AudioFormat {
    pub fn wire_id(self) -> u8 {
        match self {
            Self::PcmS16le => 0x01,
            Self::PcmS24le => 0x02,
            Self::PcmS24le4 => 0x03,
            Self::PcmS32le => 0x04,
            Self::PcmF32le => 0x05,
            Self::DsdU8 => 0x10,
            Self::DsdU16le => 0x11,
            Self::DsdU32le => 0x12,
            Self::Flac => 0x20,
            Self::Opus => 0x21,
            Self::TrueHd => 0x30,
            Self::Eac3 => 0x31,
        }
    }

    pub fn from_wire_id(id: u8) -> Option<Self> {
        match id {
            0x01 => Some(Self::PcmS16le),
            0x02 => Some(Self::PcmS24le),
            0x03 => Some(Self::PcmS24le4),
            0x04 => Some(Self::PcmS32le),
            0x05 => Some(Self::PcmF32le),
            0x10 => Some(Self::DsdU8),
            0x11 => Some(Self::DsdU16le),
            0x12 => Some(Self::DsdU32le),
            0x20 => Some(Self::Flac),
            0x21 => Some(Self::Opus),
            0x30 => Some(Self::TrueHd),
            0x31 => Some(Self::Eac3),
            _ => None,
        }
    }

    pub fn is_pcm(self) -> bool {
        matches!(
            self,
            Self::PcmS16le | Self::PcmS24le | Self::PcmS24le4 | Self::PcmS32le | Self::PcmF32le
        )
    }

    pub fn is_dsd(self) -> bool {
        matches!(self, Self::DsdU8 | Self::DsdU16le | Self::DsdU32le)
    }

    pub fn is_compressed(self) -> bool {
        matches!(self, Self::Flac | Self::Opus)
    }

    pub fn is_bitstream(self) -> bool {
        matches!(self, Self::TrueHd | Self::Eac3)
    }

    pub fn bytes_per_sample(self) -> Option<usize> {
        match self {
            Self::PcmS16le => Some(2),
            Self::PcmS24le => Some(3),
            Self::PcmS24le4 | Self::PcmS32le | Self::PcmF32le => Some(4),
            _ => None,
        }
    }
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PcmS16le => write!(f, "PCM_S16LE"),
            Self::PcmS24le => write!(f, "PCM_S24LE"),
            Self::PcmS24le4 => write!(f, "PCM_S24LE4"),
            Self::PcmS32le => write!(f, "PCM_S32LE"),
            Self::PcmF32le => write!(f, "PCM_F32LE"),
            Self::DsdU8 => write!(f, "DSD_U8"),
            Self::DsdU16le => write!(f, "DSD_U16LE"),
            Self::DsdU32le => write!(f, "DSD_U32LE"),
            Self::Flac => write!(f, "FLAC"),
            Self::Opus => write!(f, "OPUS"),
            Self::TrueHd => write!(f, "TRUEHD"),
            Self::Eac3 => write!(f, "EAC3"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    Mono,
    Stereo,
    #[serde(rename = "2.1")]
    TwoPointOne,
    Quad,
    #[serde(rename = "5.1")]
    FivePointOne,
    #[serde(rename = "7.1")]
    SevenPointOne,
    #[serde(rename = "5.1.2")]
    FivePointOnePointTwo,
    #[serde(rename = "5.1.4")]
    FivePointOnePointFour,
    #[serde(rename = "7.1.2")]
    SevenPointOnePointTwo,
    #[serde(rename = "7.1.4")]
    SevenPointOnePointFour,
    #[serde(rename = "7.1.6")]
    SevenPointOnePointSix,
}

impl ChannelLayout {
    pub fn channel_count(self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::TwoPointOne => 3,
            Self::Quad => 4,
            Self::FivePointOne => 6,
            Self::SevenPointOne => 8,
            Self::FivePointOnePointTwo => 8,
            Self::FivePointOnePointFour => 10,
            Self::SevenPointOnePointTwo => 10,
            Self::SevenPointOnePointFour => 12,
            Self::SevenPointOnePointSix => 14,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DsdRate {
    Dsd64 = 64,
    Dsd128 = 128,
    Dsd256 = 256,
    Dsd512 = 512,
}

impl DsdRate {
    pub fn bitstream_rate_hz(self) -> u64 {
        match self {
            Self::Dsd64 => 2_822_400,
            Self::Dsd128 => 5_644_800,
            Self::Dsd256 => 11_289_600,
            Self::Dsd512 => 22_579_200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleRateFamily {
    F44100,
    F48000,
}

impl SampleRateFamily {
    pub fn of(rate: u32) -> Option<Self> {
        const F441: [u32; 5] = [44100, 88200, 176400, 352800, 705600];
        const F480: [u32; 5] = [48000, 96000, 192000, 384000, 768000];
        if F441.contains(&rate) {
            Some(Self::F44100)
        } else if F480.contains(&rate) {
            Some(Self::F48000)
        } else {
            None
        }
    }

    pub fn rates(self) -> &'static [u32] {
        match self {
            Self::F44100 => &[44100, 88200, 176400, 352800, 705600],
            Self::F48000 => &[48000, 96000, 192000, 384000, 768000],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_id_roundtrip() {
        for fmt in [
            AudioFormat::PcmS16le,
            AudioFormat::PcmS24le,
            AudioFormat::PcmS24le4,
            AudioFormat::PcmS32le,
            AudioFormat::PcmF32le,
            AudioFormat::DsdU8,
            AudioFormat::DsdU16le,
            AudioFormat::DsdU32le,
            AudioFormat::Flac,
            AudioFormat::Opus,
            AudioFormat::TrueHd,
            AudioFormat::Eac3,
        ] {
            assert_eq!(AudioFormat::from_wire_id(fmt.wire_id()), Some(fmt));
        }
    }

    #[test]
    fn sample_rate_family() {
        assert_eq!(SampleRateFamily::of(44100), Some(SampleRateFamily::F44100));
        assert_eq!(SampleRateFamily::of(192000), Some(SampleRateFamily::F48000));
        assert_eq!(SampleRateFamily::of(50000), None);
    }
}
