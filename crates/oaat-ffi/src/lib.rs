//! C FFI bindings for the OAAT endpoint SDK.
//!
//! Allows hardware manufacturers to implement OAAT endpoints in C/C++ firmware.
//! The FFI layer runs a tokio runtime internally, presenting a synchronous
//! polling API to C callers.

use std::ffi::{CStr, CString};
use std::net::SocketAddr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use oaat_core::Message;
use oaat_core::format::AudioFormat;
use oaat_core::message::EndpointCapabilities;
use oaat_endpoint::transport::{EndpointConfig, EndpointEvent, EndpointTransport};

// ---------------------------------------------------------------------------
// C-visible enums (repr(C) for ABI stability)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OaatAudioFormat {
    PcmS16le = 0x01,
    PcmS24le = 0x02,
    PcmS24le4 = 0x03,
    PcmS32le = 0x04,
    PcmF32le = 0x05,
    DsdU8 = 0x10,
    DsdU16le = 0x11,
    DsdU32le = 0x12,
    Flac = 0x20,
    Opus = 0x21,
    TrueHd = 0x30,
    Eac3 = 0x31,
}

impl From<AudioFormat> for OaatAudioFormat {
    fn from(f: AudioFormat) -> Self {
        match f {
            AudioFormat::PcmS16le => Self::PcmS16le,
            AudioFormat::PcmS24le => Self::PcmS24le,
            AudioFormat::PcmS24le4 => Self::PcmS24le4,
            AudioFormat::PcmS32le => Self::PcmS32le,
            AudioFormat::PcmF32le => Self::PcmF32le,
            AudioFormat::DsdU8 => Self::DsdU8,
            AudioFormat::DsdU16le => Self::DsdU16le,
            AudioFormat::DsdU32le => Self::DsdU32le,
            AudioFormat::Flac => Self::Flac,
            AudioFormat::Opus => Self::Opus,
            AudioFormat::TrueHd => Self::TrueHd,
            AudioFormat::Eac3 => Self::Eac3,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OaatEventType {
    None = 0,
    Connected = 1,
    FormatProposed = 2,
    Audio = 3,
    Play = 4,
    Pause = 5,
    Stop = 6,
    Metadata = 7,
    Disconnected = 8,
    VolumeSet = 9,
    VolumeGet = 10,
    VolumeMute = 11,
    FormatAccepted = 12,
    FormatRejected = 13,
    Error = 14,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OaatStatus {
    Idle = 0,
    Connected = 1,
    Streaming = 2,
    Paused = 3,
    Error = 4,
}

// ---------------------------------------------------------------------------
// C-visible event data structs
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct OaatConnectedData {
    pub controller_id: *const c_char,
    pub controller_name: *const c_char,
}

#[repr(C)]
pub struct OaatFormatProposedData {
    pub stream_id: *const c_char,
    pub format: OaatAudioFormat,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
}

#[repr(C)]
pub struct OaatAudioData {
    pub data: *const u8,
    pub len: usize,
    pub format: OaatAudioFormat,
    pub sequence: u16,
    pub pts_ns: u64,
    pub sample_offset: u64,
}

#[repr(C)]
pub struct OaatPlaybackData {
    pub stream_id: *const c_char,
}

#[repr(C)]
pub struct OaatMetadataData {
    pub title: *const c_char,
    pub artist: *const c_char,
    pub album: *const c_char,
    pub duration_ms: u64,
}

#[repr(C)]
pub struct OaatVolumeData {
    pub level: u8,
    pub muted: bool,
}

#[repr(C)]
pub struct OaatFormatRejectedData {
    pub stream_id: *const c_char,
    pub reason: *const c_char,
}

#[repr(C)]
pub struct OaatErrorData {
    pub message: *const c_char,
}

// ---------------------------------------------------------------------------
// Event union — C-compatible tagged union
// ---------------------------------------------------------------------------

#[repr(C)]
pub union OaatEventData {
    pub connected: std::mem::ManuallyDrop<OaatConnectedData>,
    pub format_proposed: std::mem::ManuallyDrop<OaatFormatProposedData>,
    pub audio: std::mem::ManuallyDrop<OaatAudioData>,
    pub playback: std::mem::ManuallyDrop<OaatPlaybackData>,
    pub metadata: std::mem::ManuallyDrop<OaatMetadataData>,
    pub volume: std::mem::ManuallyDrop<OaatVolumeData>,
    pub format_rejected: std::mem::ManuallyDrop<OaatFormatRejectedData>,
    pub error: std::mem::ManuallyDrop<OaatErrorData>,
}

#[repr(C)]
pub struct OaatEvent {
    pub event_type: OaatEventType,
    pub data: OaatEventData,
}

// ---------------------------------------------------------------------------
// Audio callback type
// ---------------------------------------------------------------------------

pub type OaatAudioCallback = Option<
    unsafe extern "C" fn(
        data: *const u8,
        len: usize,
        format: OaatAudioFormat,
        sample_rate: u32,
        channels: u8,
        user_data: *mut std::ffi::c_void,
    ),
>;

// ---------------------------------------------------------------------------
// Internal endpoint state
// ---------------------------------------------------------------------------

struct AudioCallbackState {
    callback: OaatAudioCallback,
    user_data: *mut std::ffi::c_void,
    /// Current negotiated sample rate (set on FormatProposed accept).
    sample_rate: u32,
    /// Current negotiated channel count.
    channels: u8,
}

// Safety: user_data is a raw pointer provided by the C caller.
// The C caller is responsible for ensuring it is valid and thread-safe.
unsafe impl Send for AudioCallbackState {}
unsafe impl Sync for AudioCallbackState {}

pub struct OaatEndpoint {
    runtime: Runtime,
    event_rx: Mutex<mpsc::Receiver<EndpointEvent>>,
    _control_tx: mpsc::Sender<Message>,
    audio_cb: Arc<Mutex<AudioCallbackState>>,
    status: AtomicU8,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Rust string to a heap-allocated C string.
/// Returns null pointer if the string contains interior NUL bytes.
fn to_c_string(s: &str) -> *const c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw() as *const c_char,
        Err(_) => std::ptr::null(),
    }
}

fn none_event() -> OaatEvent {
    OaatEvent {
        event_type: OaatEventType::None,
        data: OaatEventData {
            volume: std::mem::ManuallyDrop::new(OaatVolumeData {
                level: 0,
                muted: false,
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// C API — Lifecycle
// ---------------------------------------------------------------------------

/// Create a new OAAT endpoint.
///
/// Starts a tokio runtime, binds to the given port, and begins listening
/// for controller connections. Returns NULL on failure.
///
/// # Safety
/// `name` must be a valid null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_new(name: *const c_char, port: u16) -> *mut OaatEndpoint {
    if name.is_null() {
        return std::ptr::null_mut();
    }

    let name_str = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };

    // Build the runtime
    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return std::ptr::null_mut(),
    };

    let (event_tx, event_rx) = mpsc::channel::<EndpointEvent>(256);
    let (control_tx, control_rx) = mpsc::channel::<Message>(64);

    let endpoint_id = uuid::Uuid::new_v4().to_string();

    let control_port = port;
    let audio_port = if port == 0 { 0 } else { port + 1 };
    let clock_port = if port == 0 { 0 } else { port + 2 };

    let config = EndpointConfig {
        endpoint_id,
        endpoint_name: name_str,
        control_addr: SocketAddr::from(([0, 0, 0, 0], control_port)),
        audio_addr: SocketAddr::from(([0, 0, 0, 0], audio_port)),
        clock_addr: SocketAddr::from(([0, 0, 0, 0], clock_port)),
        capabilities: EndpointCapabilities {
            pcm_max_rate: 768_000,
            pcm_max_bits: 32,
            dsd_max_rate: Some(256),
            channels_max: 8,
            formats: vec![
                AudioFormat::PcmS16le,
                AudioFormat::PcmS24le,
                AudioFormat::PcmS24le4,
                AudioFormat::PcmS32le,
                AudioFormat::PcmF32le,
                AudioFormat::DsdU8,
                AudioFormat::DsdU16le,
                AudioFormat::DsdU32le,
                AudioFormat::Flac,
            ],
            volume: None,
            gapless: true,
            seek: true,
        },
        buffer_size_ms: 200,
        tls: false,
    };

    // Spawn the endpoint transport on the runtime
    runtime.spawn(async move {
        if let Err(e) = EndpointTransport::run(config, event_tx, control_rx).await {
            tracing::error!(error = %e, "endpoint transport exited");
        }
    });

    let audio_cb = Arc::new(Mutex::new(AudioCallbackState {
        callback: None,
        user_data: std::ptr::null_mut(),
        sample_rate: 0,
        channels: 0,
    }));

    let ep = OaatEndpoint {
        runtime,
        event_rx: Mutex::new(event_rx),
        _control_tx: control_tx,
        audio_cb,
        status: AtomicU8::new(OaatStatus::Idle as u8),
    };

    Box::into_raw(Box::new(ep))
}

/// Destroy an endpoint and free all resources.
///
/// # Safety
/// `ep` must be a pointer returned by `oaat_endpoint_new` and must not be
/// used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_free(ep: *mut OaatEndpoint) {
    if !ep.is_null() {
        unsafe { drop(Box::from_raw(ep)) };
    }
}

// ---------------------------------------------------------------------------
// C API — Event polling
// ---------------------------------------------------------------------------

/// Poll for the next event from the endpoint.
///
/// Blocks up to `timeout_ms` milliseconds. Returns an event with type
/// `OAAT_EVENT_NONE` if the timeout expires with no event.
///
/// # Safety
/// `ep` must be a valid pointer returned by `oaat_endpoint_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_poll_event(
    ep: *mut OaatEndpoint,
    timeout_ms: u32,
) -> OaatEvent {
    if ep.is_null() {
        return none_event();
    }

    let ep = unsafe { &*ep };
    let mut rx = match ep.event_rx.lock() {
        Ok(rx) => rx,
        Err(_) => return none_event(),
    };

    let event = if timeout_ms == 0 {
        // Non-blocking
        rx.try_recv().ok()
    } else {
        // Blocking with timeout — use the endpoint's runtime
        ep.runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_millis(timeout_ms as u64), rx.recv()).await
            })
            .ok()
            .flatten()
    };

    let Some(event) = event else {
        return none_event();
    };

    convert_event(ep, event)
}

/// Convert an internal EndpointEvent to a C-visible OaatEvent.
fn convert_event(ep: &OaatEndpoint, event: EndpointEvent) -> OaatEvent {
    match event {
        EndpointEvent::Connected {
            controller_id,
            controller_name,
        } => {
            ep.status
                .store(OaatStatus::Connected as u8, Ordering::Relaxed);
            OaatEvent {
                event_type: OaatEventType::Connected,
                data: OaatEventData {
                    connected: std::mem::ManuallyDrop::new(OaatConnectedData {
                        controller_id: to_c_string(&controller_id),
                        controller_name: to_c_string(&controller_name),
                    }),
                },
            }
        }

        EndpointEvent::FormatProposed(fp) => {
            // Update the audio callback state with the negotiated format
            if let Ok(mut cb) = ep.audio_cb.lock() {
                cb.sample_rate = fp.sample_rate;
                cb.channels = fp.channels;
            }
            OaatEvent {
                event_type: OaatEventType::FormatProposed,
                data: OaatEventData {
                    format_proposed: std::mem::ManuallyDrop::new(OaatFormatProposedData {
                        stream_id: to_c_string(&fp.stream_id),
                        format: OaatAudioFormat::from(fp.format),
                        sample_rate: fp.sample_rate,
                        channels: fp.channels,
                        bits_per_sample: fp.bits_per_sample,
                    }),
                },
            }
        }

        EndpointEvent::FormatAccepted { stream_id } => {
            ep.status
                .store(OaatStatus::Streaming as u8, Ordering::Relaxed);
            OaatEvent {
                event_type: OaatEventType::FormatAccepted,
                data: OaatEventData {
                    playback: std::mem::ManuallyDrop::new(OaatPlaybackData {
                        stream_id: to_c_string(&stream_id),
                    }),
                },
            }
        }

        EndpointEvent::FormatRejected { stream_id, reason } => OaatEvent {
            event_type: OaatEventType::FormatRejected,
            data: OaatEventData {
                format_rejected: std::mem::ManuallyDrop::new(OaatFormatRejectedData {
                    stream_id: to_c_string(&stream_id),
                    reason: to_c_string(&reason),
                }),
            },
        },

        EndpointEvent::AudioPacket { header, payload } => {
            // If an audio callback is registered, invoke it directly
            if let Ok(cb) = ep.audio_cb.lock()
                && let Some(callback) = cb.callback
            {
                unsafe {
                    callback(
                        payload.as_ptr(),
                        payload.len(),
                        OaatAudioFormat::from(header.format),
                        cb.sample_rate,
                        cb.channels,
                        cb.user_data,
                    );
                }
                // Still return the event so the caller knows audio arrived
            }

            // Leak the payload into a stable pointer for the C caller.
            // The data pointer is valid only until the next poll_event call
            // from the same thread — in practice the C caller should copy it.
            let leaked = payload.leak();
            OaatEvent {
                event_type: OaatEventType::Audio,
                data: OaatEventData {
                    audio: std::mem::ManuallyDrop::new(OaatAudioData {
                        data: leaked.as_ptr(),
                        len: leaked.len(),
                        format: OaatAudioFormat::from(header.format),
                        sequence: header.sequence,
                        pts_ns: header.pts_ns,
                        sample_offset: header.sample_offset,
                    }),
                },
            }
        }

        EndpointEvent::Playback(cmd) => {
            use oaat_endpoint::transport::PlaybackCommand;
            let (event_type, stream_id) = match &cmd {
                PlaybackCommand::Play(s) => {
                    ep.status
                        .store(OaatStatus::Streaming as u8, Ordering::Relaxed);
                    (OaatEventType::Play, s.as_str())
                }
                PlaybackCommand::Pause(s) => {
                    ep.status.store(OaatStatus::Paused as u8, Ordering::Relaxed);
                    (OaatEventType::Pause, s.as_str())
                }
                PlaybackCommand::Stop(s) => {
                    ep.status
                        .store(OaatStatus::Connected as u8, Ordering::Relaxed);
                    (OaatEventType::Stop, s.as_str())
                }
                PlaybackCommand::Seek(s, _) => (OaatEventType::Play, s.as_str()),
            };
            OaatEvent {
                event_type,
                data: OaatEventData {
                    playback: std::mem::ManuallyDrop::new(OaatPlaybackData {
                        stream_id: to_c_string(stream_id),
                    }),
                },
            }
        }

        EndpointEvent::Metadata(m) => OaatEvent {
            event_type: OaatEventType::Metadata,
            data: OaatEventData {
                metadata: std::mem::ManuallyDrop::new(OaatMetadataData {
                    title: to_c_string(&m.track.title),
                    artist: to_c_string(&m.track.artist),
                    album: to_c_string(&m.track.album),
                    duration_ms: m.track.duration_ms,
                }),
            },
        },

        EndpointEvent::Volume(cmd) => {
            use oaat_endpoint::transport::VolumeCommand;
            match cmd {
                VolumeCommand::Set(level) => OaatEvent {
                    event_type: OaatEventType::VolumeSet,
                    data: OaatEventData {
                        volume: std::mem::ManuallyDrop::new(OaatVolumeData {
                            level,
                            muted: false,
                        }),
                    },
                },
                VolumeCommand::Get => OaatEvent {
                    event_type: OaatEventType::VolumeGet,
                    data: OaatEventData {
                        volume: std::mem::ManuallyDrop::new(OaatVolumeData {
                            level: 0,
                            muted: false,
                        }),
                    },
                },
                VolumeCommand::Mute(muted) => OaatEvent {
                    event_type: OaatEventType::VolumeMute,
                    data: OaatEventData {
                        volume: std::mem::ManuallyDrop::new(OaatVolumeData { level: 0, muted }),
                    },
                },
            }
        }

        EndpointEvent::Disconnected => {
            ep.status.store(OaatStatus::Idle as u8, Ordering::Relaxed);
            OaatEvent {
                event_type: OaatEventType::Disconnected,
                data: OaatEventData {
                    volume: std::mem::ManuallyDrop::new(OaatVolumeData {
                        level: 0,
                        muted: false,
                    }),
                },
            }
        }

        EndpointEvent::Error(e) => {
            ep.status.store(OaatStatus::Error as u8, Ordering::Relaxed);
            OaatEvent {
                event_type: OaatEventType::Error,
                data: OaatEventData {
                    error: std::mem::ManuallyDrop::new(OaatErrorData {
                        message: to_c_string(&e.to_string()),
                    }),
                },
            }
        }

        // Events that don't have a direct C mapping — surface as None
        EndpointEvent::NextTrackReady { .. }
        | EndpointEvent::NextTrackReformat { .. }
        | EndpointEvent::ZoneAssigned { .. }
        | EndpointEvent::ZoneUpdated { .. }
        | EndpointEvent::ZoneReleased { .. } => none_event(),
    }
}

// ---------------------------------------------------------------------------
// C API — Audio callback
// ---------------------------------------------------------------------------

/// Register a callback for incoming audio data.
///
/// # Safety
/// `ep` must be a valid pointer returned by `oaat_endpoint_new`.
/// `user_data` must remain valid for the lifetime of the callback registration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_set_audio_callback(
    ep: *mut OaatEndpoint,
    callback: OaatAudioCallback,
    user_data: *mut std::ffi::c_void,
) {
    if ep.is_null() {
        return;
    }

    let ep = unsafe { &*ep };
    if let Ok(mut cb) = ep.audio_cb.lock() {
        cb.callback = callback;
        cb.user_data = user_data;
    }
}

// ---------------------------------------------------------------------------
// C API — Control
// ---------------------------------------------------------------------------

/// Set the endpoint volume level (0-100).
///
/// # Safety
/// `ep` must be a valid pointer returned by `oaat_endpoint_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_set_volume(ep: *mut OaatEndpoint, level: u8) {
    if ep.is_null() {
        return;
    }

    let ep = unsafe { &*ep };
    let msg = Message::VolumeReport(oaat_core::message::VolumeReport {
        level,
        muted: false,
    });
    // Best-effort send — if the channel is full, drop the message
    let _ = ep._control_tx.try_send(msg);
}

/// Get the current endpoint status.
///
/// # Safety
/// `ep` must be a valid pointer returned by `oaat_endpoint_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_endpoint_get_status(ep: *const OaatEndpoint) -> OaatStatus {
    if ep.is_null() {
        return OaatStatus::Error;
    }

    let ep = unsafe { &*ep };
    match ep.status.load(Ordering::Relaxed) {
        0 => OaatStatus::Idle,
        1 => OaatStatus::Connected,
        2 => OaatStatus::Streaming,
        3 => OaatStatus::Paused,
        _ => OaatStatus::Error,
    }
}

// ---------------------------------------------------------------------------
// C API — String management
// ---------------------------------------------------------------------------

/// Free a string returned by an OaatEvent.
///
/// # Safety
/// `s` must be a pointer previously returned in an OaatEvent field,
/// or NULL (which is a no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oaat_string_free(s: *const c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s as *mut c_char)) };
    }
}
