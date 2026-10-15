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

    // -- Health --
    StreamStats(StreamStats),

    // -- Endpoint release --
    EndpointRelease(EndpointRelease),

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
    /// Optional protocol features the endpoint supports, the counterpart of
    /// `Hello::features` (e.g. [`FEATURE_ENDPOINT_RELEASE`]). Absent from
    /// endpoints that predate it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// Feature flag for `endpoint_release` (RFC §7.3.1): in `Hello::features`
/// the controller handles it, in `HelloAck::features` the endpoint may send
/// it. An endpoint only sends it to a controller that announced the flag.
pub const FEATURE_ENDPOINT_RELEASE: &str = "endpoint_release";

/// `format_reject` reason for a negotiation that an `endpoint_release` ended
/// (RFC §7.3.1).
pub const RELEASE_REJECT_REASON: &str = "released";

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

/// Periodic endpoint → controller health report (RFC §6.6, §7.3): lets the
/// controller see buffer health, residual drift and link quality without
/// polling. Sent every few seconds while streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStats {
    pub stream_id: String,
    /// Ring buffer fill level, in frames.
    pub buffer_frames: u64,
    /// Measured playback drift vs the controller clock, in microseconds.
    /// Positive: playback is behind schedule.
    pub drift_us: i64,
    /// Net frames skipped (−duplicated) by drift correction so far.
    pub corrections_net_frames: i64,
    /// Packets lost beyond FEC recovery since stream start.
    pub packets_lost: u64,
    /// Packets recovered from FEC parity since stream start.
    pub packets_recovered: u64,
    /// Whether the active output path delivers samples bit-exact.
    pub bit_perfect: bool,
    /// Output underruns: times the audio device ran out of samples during
    /// playback (#28). Absent from older endpoints (defaults to 0).
    #[serde(default)]
    pub underruns: u64,
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

// -- Endpoint release --

/// Endpoint -> Controller (RFC §7.3.1): the endpoint has given up its audio
/// output, typically because another source on the same device took the DAC.
/// It has stopped and closed the device; the controller pauses the stream
/// and does not resume it on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRelease {
    /// The stream that was playing, empty if none.
    pub stream_id: String,
    pub reason: ReleaseReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    /// Another source on the same device took the audio output.
    LocalSource,
    /// Any reason this version of the protocol does not know.
    #[serde(other)]
    Other,
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

    /// #28: `underruns` is optional on the wire, so stats from endpoints that
    /// predate it still parse.
    #[test]
    fn stream_stats_without_underruns_defaults_to_zero() {
        let json = r#"{"type":"stream_stats","stream_id":"s","buffer_frames":1,"drift_us":0,"corrections_net_frames":0,"packets_lost":0,"packets_recovered":0,"bit_perfect":true}"#;
        match serde_json::from_str::<Message>(json).unwrap() {
            Message::StreamStats(ss) => assert_eq!(ss.underruns, 0),
            other => panic!("expected StreamStats, got {other:?}"),
        }
    }

    #[test]
    fn endpoint_release_json() {
        let msg = Message::EndpointRelease(EndpointRelease {
            stream_id: "tune-1".into(),
            reason: ReleaseReason::LocalSource,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"endpoint_release","stream_id":"tune-1","reason":"local_source"}"#
        );
        // A reason added by a later version still parses.
        let later = r#"{"type":"endpoint_release","stream_id":"","reason":"power_save"}"#;
        match serde_json::from_str::<Message>(later).unwrap() {
            Message::EndpointRelease(r) => assert_eq!(r.reason, ReleaseReason::Other),
            other => panic!("expected EndpointRelease, got {other:?}"),
        }
    }

    /// `features` is optional on the wire both ways: a HelloAck from an
    /// endpoint that predates it parses, and an empty list is not sent.
    #[test]
    fn hello_ack_features_are_optional() {
        let json = r#"{"type":"hello_ack","protocol_version":1,"endpoint_id":"e","endpoint_name":"n","capabilities":{"pcm_max_rate":192000,"pcm_max_bits":24,"channels_max":2,"formats":["PCM_S16LE"]},"audio_port":1,"clock_port":2,"buffer_size_ms":1000}"#;
        let Message::HelloAck(mut ack) = serde_json::from_str::<Message>(json).unwrap() else {
            panic!("expected HelloAck");
        };
        assert!(ack.features.is_empty());
        let out = serde_json::to_string(&Message::HelloAck(ack.clone())).unwrap();
        assert!(!out.contains("features"));
        ack.features.push(FEATURE_ENDPOINT_RELEASE.into());
        let out = serde_json::to_string(&Message::HelloAck(ack)).unwrap();
        assert!(out.contains(r#""features":["endpoint_release"]"#));
    }

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
