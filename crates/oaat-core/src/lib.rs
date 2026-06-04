pub mod capability;
pub mod clock;
pub mod codec;
pub mod error;
pub mod fec;
pub mod format;
pub mod message;
pub mod session;
#[cfg(feature = "tls")]
pub mod tls;
pub mod wire;

pub use capability::Capabilities;
pub use clock::ClockState;
pub use codec::FrameCodec;
pub use error::OaatError;
pub use format::{AudioFormat, ChannelLayout, DsdRate, SampleRateFamily};
pub use message::Message;
pub use session::SessionState;
pub use wire::{AudioPacketHeader, ClockSyncPacket, PacketFlags};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_CONTROL_PORT: u16 = 9740;
pub const DEFAULT_AUDIO_PORT: u16 = 9741;
pub const DEFAULT_CLOCK_PORT: u16 = 9742;
pub const SERVICE_TYPE: &str = "_oaat._tcp";
pub const CTRL_SERVICE_TYPE: &str = "_oaat-ctrl._tcp";
pub const AUDIO_HEADER_SIZE: usize = 32;
pub const MAX_AUDIO_PAYLOAD: usize = 8192;
pub const DEFAULT_SINGLE_PLAY_DELAY_MS: u64 = 200;
pub const DEFAULT_MULTIROOM_PLAY_DELAY_MS: u64 = 500;
