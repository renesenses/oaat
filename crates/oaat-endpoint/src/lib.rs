#[cfg(target_os = "linux")]
pub mod alsa_direct;
#[cfg(target_os = "linux")]
pub mod alsa_mixer;
#[cfg(feature = "audio-output")]
pub mod audio_output;
pub mod discovery;
#[cfg(feature = "flac")]
pub mod flac_decoder;
pub mod hal;
pub mod session;
pub mod sync;
pub mod transport;
#[cfg(feature = "web-ui")]
pub mod web_ui;

#[cfg(target_os = "linux")]
pub use alsa_direct::AlsaDirectOutput;
#[cfg(target_os = "linux")]
pub use alsa_mixer::AlsaMixer;
#[cfg(feature = "audio-output")]
pub use audio_output::CpalOutput;
pub use hal::OaatHal;
pub use sync::{PtsTracker, SharedClock};
pub use transport::{EndpointConfig, EndpointEvent, EndpointTransport};
#[cfg(feature = "web-ui")]
pub use web_ui::{BridgeStatus, BridgeStatusHandle, start_web_ui};
