use serde::{Deserialize, Serialize};

use crate::format::{AudioFormat, ChannelLayout};

/// All control protocol messages, framed as length-prefixed JSON over TCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    // -- Handshake --
    Hello(Hello),
    HelloAck(HelloAck),

    // -- Format negotiation --
    FormatPropose(FormatPropose),
    FormatAccept(FormatAccept),
    FormatCounter(FormatCounter),
    FormatReject(FormatReject),

    // -- Playback --
    Play(Play),
    Pause(Pause),
    Stop(Stop),
    Seek(Seek),

    // -- Volume --
    VolumeSet(VolumeSet),
    VolumeGet(VolumeGet),
    VolumeReport(VolumeReport),
    Mute(Mute),

    // -- Metadata --
    Metadata(Metadata),

    // -- Zone --
    ZoneAssign(ZoneAssign),
    ZoneUpdate(ZoneUpdate),
    ZoneRelease(ZoneRelease),
    ZoneAck(ZoneAck),

    // -- Gapless --
    NextTrackPrepare(NextTrackPrepare),
    NextTrackReady(NextTrackReady),
    NextTrackReformat(NextTrackReformat),

    // -- Error --
    Error(ErrorMsg),
}

// -- Handshake types --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub controller_id: String,
    pub controller_name: String,
    pub clock_port: u16,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub capabilities: EndpointCapabilities,
    pub audio_port: u16,
    pub clock_port: u16,
    pub buffer_size_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointCapabilities {
    pub pcm_max_rate: u32,
    pub pcm_max_bits: u8,
    #[serde(default)]
    pub dsd_max_rate: Option<u16>,
    pub channels_max: u8,
    pub formats: Vec<AudioFormat>,
    #[serde(default)]
    pub volume: Option<VolumeCapability>,
    #[serde(default)]
    pub gapless: bool,
    #[serde(default)]
    pub seek: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeCapability {
    #[serde(rename = "type")]
    pub vol_type: VolumeType,
    pub range: [u8; 2],
    pub step: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeType {
    Hw,
    Sw,
    Fixed,
    None,
}

// -- Format negotiation --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatPropose {
    pub stream_id: String,
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bits_per_sample: u8,
    #[serde(default)]
    pub dsd_rate: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatAccept {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatCounter {
    pub stream_id: String,
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bits_per_sample: u8,
    #[serde(default)]
    pub dsd_rate: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatReject {
    pub stream_id: String,
    pub reason: String,
}

// -- Playback --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Play {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pause {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seek {
    pub stream_id: String,
    pub position_ms: u64,
}

// -- Volume --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeSet {
    pub level: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeGet {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeReport {
    pub level: u8,
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mute {
    pub muted: bool,
}

// -- Metadata --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub track: TrackMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

// -- Zone management --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneAssign {
    pub zone_id: String,
    pub endpoint_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneUpdate {
    pub zone_id: String,
    pub endpoint_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneRelease {
    pub zone_id: String,
    pub endpoint_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneAck {
    pub zone_id: String,
    pub endpoint_id: String,
    pub accepted: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

// -- Gapless --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextTrackPrepare {
    pub stream_id: String,
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bits_per_sample: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextTrackReady {
    pub stream_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextTrackReformat {
    pub stream_id: String,
    pub format: AudioFormat,
    pub sample_rate: u32,
}

// -- Error --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: u32,
    pub message: String,
}

// -- Framing --

impl Message {
    pub fn encode_framed(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).expect("message serialization cannot fail");
        let len = json.len() as u32;
        let mut buf = Vec::with_capacity(4 + json.len());
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&json);
        buf
    }

    pub fn decode_json(json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let msg = Message::Hello(Hello {
            protocol_version: 1,
            controller_id: "abc".into(),
            controller_name: "Tune Server".into(),
            clock_port: 9742,
            features: vec!["flac_transport".into(), "dsd_native".into()],
        });
        let framed = msg.encode_framed();
        let len = u32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
        assert_eq!(len, framed.len() - 4);
        let decoded = Message::decode_json(&framed[4..]).unwrap();
        match decoded {
            Message::Hello(h) => {
                assert_eq!(h.controller_name, "Tune Server");
                assert_eq!(h.features.len(), 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn format_propose_json() {
        let msg = Message::FormatPropose(FormatPropose {
            stream_id: "abc123".into(),
            format: AudioFormat::PcmS24le,
            sample_rate: 192000,
            channels: 2,
            channel_layout: ChannelLayout::Stereo,
            bits_per_sample: 24,
            dsd_rate: None,
        });
        let json = serde_json::to_string_pretty(&msg).unwrap();
        assert!(json.contains("format_propose"));
        assert!(json.contains("192000"));
    }
}
