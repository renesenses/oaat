//! cpal audio output with a bit-perfect native-format ring buffer.
//!
//! The ring stores the negotiated wire format's raw bytes — no intermediate
//! float normalization. When the device accepts a matching integer sample
//! type and the volume is at 100% unmuted, samples reach the DAC untouched:
//! - PCM_S16LE → i16 stream (exact)
//! - PCM_S24LE / PCM_S24LE4 → i32 stream, shifted left 8 (exact)
//! - PCM_S32LE → i32 stream (exact)
//! - PCM_F32LE → f32 stream (exact)
//!
//! FLAC decodes to normalized f32; DSD is decimated to f32 (software
//! fallback). Both use power-of-two scaling, so FLAC ≤ 24-bit remains
//! lossless end-to-end. Software volume (≠ 100%) necessarily breaks
//! bit-perfection; hardware volume (`vol=hw`) keeps it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use tracing::{error, info, warn};

use oaat_core::format::AudioFormat;

const RING_BUFFER_FRAMES: usize = 48000;

/// Names that indicate a built-in / onboard audio device (not a USB DAC).
const BUILTIN_DEVICE_KEYWORDS: &[&str] = &[
    "built-in",
    "speakers",
    "realtek",
    "internal",
    "hdmi",
    "displayport",
    "macbook",
];

/// Names that indicate a USB DAC or external audio device.
const USB_DAC_KEYWORDS: &[&str] = &["usb", "dac"];

/// Classify a device name: returns true if the device looks like a USB DAC / external device.
fn is_usb_dac(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Positive match: contains USB/DAC keywords
    if USB_DAC_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    // Negative match: if it matches built-in keywords, it is NOT a USB DAC
    if BUILTIN_DEVICE_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return false;
    }
    // Unknown device name — assume external (prefer it over built-in)
    true
}

/// Sample layout of the bytes stored in the ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RingFormat {
    S16,
    S24,
    S24In32,
    S32,
    F32,
}

impl RingFormat {
    fn bytes_per_sample(self) -> usize {
        match self {
            RingFormat::S16 => 2,
            RingFormat::S24 => 3,
            RingFormat::S24In32 | RingFormat::S32 | RingFormat::F32 => 4,
        }
    }

    fn of(format: AudioFormat) -> Self {
        match format {
            AudioFormat::PcmS16le => RingFormat::S16,
            AudioFormat::PcmS24le => RingFormat::S24,
            AudioFormat::PcmS24le4 => RingFormat::S24In32,
            AudioFormat::PcmS32le => RingFormat::S32,
            // FLAC decodes to normalized f32 (power-of-two scaling: exact
            // for ≤ 24-bit sources); DSD decimates to f32.
            _ => RingFormat::F32,
        }
    }

    /// Decode one sample from ring bytes to i32, left-justified to 32 bits
    /// (so an i32 stream gets full-scale output and S32 is a passthrough).
    #[inline]
    fn to_i32(self, b: &[u8]) -> i32 {
        match self {
            RingFormat::S16 => (i16::from_le_bytes([b[0], b[1]]) as i32) << 16,
            RingFormat::S24 => {
                let sign = if b[2] & 0x80 != 0 { 0xFF } else { 0 };
                i32::from_le_bytes([b[0], b[1], b[2], sign]) << 8
            }
            RingFormat::S24In32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) << 8,
            RingFormat::S32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            RingFormat::F32 => {
                let f = f32::from_le_bytes([b[0], b[1], b[2], b[3]]).clamp(-1.0, 1.0);
                // 2^31 scaling: exact for power-of-two normalized sources.
                (f as f64 * 2_147_483_648.0).clamp(i32::MIN as f64, i32::MAX as f64) as i32
            }
        }
    }

    /// Decode one sample from ring bytes to normalized f32 (power-of-two
    /// divisors: bijective for ≤ 24-bit integer sources).
    #[inline]
    fn to_f32(self, b: &[u8]) -> f32 {
        match self {
            RingFormat::S16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
            RingFormat::S24 => {
                let sign = if b[2] & 0x80 != 0 { 0xFF } else { 0 };
                i32::from_le_bytes([b[0], b[1], b[2], sign]) as f32 / 8_388_608.0
            }
            RingFormat::S24In32 => {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 8_388_608.0
            }
            RingFormat::S32 => {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
            }
            RingFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
}

/// Which sample types the device advertises.
#[derive(Debug, Clone, Copy, Default)]
struct DeviceSampleSupport {
    f32: bool,
    i16: bool,
    i32: bool,
}

pub struct CpalOutput {
    stream: Option<cpal::Stream>,
    producer: Option<ringbuf::HeapProd<u8>>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>, // 0-1000 (0.0-1.0 * 1000)
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
    ring_format: RingFormat,
    device_name: Option<String>,
    /// Samples actually consumed from the ring by the audio callback.
    /// Divided by `channels`, this is the playback position in frames —
    /// the reference the drift servo compares against the clock.
    samples_played: Arc<AtomicU64>,
    /// Pending drift correction in frames. Positive: playback is behind,
    /// frames are dropped on write. Negative: playback is ahead, frames are
    /// duplicated on write. Applied at most a few frames per packet so the
    /// correction itself stays inaudible (jump resync excepted, see
    /// `write_audio`).
    correction: Arc<AtomicI64>,
    /// Net frames adjusted so far: dropped − duplicated. Content position at
    /// the DAC = frames consumed + this adjustment; without it, skipping
    /// would never show up in the measured drift and the servo would skip
    /// forever.
    net_adjust: Arc<AtomicI64>,
    /// True while the DAC receives the exact negotiated samples (matching
    /// stream type, volume 100%, unmuted). Diagnostic, refreshed on
    /// configure.
    bit_perfect: Arc<AtomicU8>,
    /// Resolved cpal device + sample type support, keyed by the requested
    /// name. Device and config enumeration cost ~1-2 s on macOS: paying
    /// them on every configure() eats the PTS scheduling lead time.
    /// `prewarm()` fills this at startup.
    cached_device: Option<(Option<String>, cpal::Device, DeviceSampleSupport)>,
    #[cfg(feature = "flac")]
    flac_stream: Option<crate::flac_decoder::FlacStreamDecoder>,
}

impl CpalOutput {
    pub fn list_devices() -> Vec<String> {
        let host = cpal::default_host();
        host.output_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }

    pub fn default_device_name() -> Option<String> {
        let host = cpal::default_host();
        host.default_output_device().and_then(|d| d.name().ok())
    }

    /// Auto-detect the best audio output device, preferring USB DACs over built-in audio.
    /// Returns the device name if a USB DAC is found, or None to use the system default.
    pub fn auto_detect_usb_dac() -> Option<String> {
        let devices = Self::list_devices();
        if devices.is_empty() {
            return None;
        }

        // Look for a USB DAC first
        for name in &devices {
            if is_usb_dac(name) {
                info!(device = %name, "auto-detected USB DAC");
                return Some(name.clone());
            }
        }

        // No USB DAC found — return None so caller falls back to default
        warn!("no USB DAC detected, will use system default audio output");
        None
    }

    /// Get the current audio device name.
    pub fn current_device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    pub fn new() -> Self {
        Self {
            stream: None,
            producer: None,
            playing: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(1000)), // 100%
            muted: Arc::new(AtomicBool::new(false)),
            sample_rate: 0,
            channels: 0,
            format: AudioFormat::PcmS16le,
            ring_format: RingFormat::S16,
            device_name: None,
            samples_played: Arc::new(AtomicU64::new(0)),
            correction: Arc::new(AtomicI64::new(0)),
            net_adjust: Arc::new(AtomicI64::new(0)),
            bit_perfect: Arc::new(AtomicU8::new(0)),
            cached_device: None,
            #[cfg(feature = "flac")]
            flac_stream: None,
        }
    }

    /// Resolve the output device by name (or default) plus its sample type
    /// support, preferring the cache. On a cache miss the enumeration cost
    /// is paid and the result cached for subsequent streams.
    fn resolve_device(
        &mut self,
        device_name: Option<&str>,
    ) -> Result<(cpal::Device, DeviceSampleSupport), Box<dyn std::error::Error>> {
        if let Some((cached_key, device, support)) = &self.cached_device
            && cached_key.as_deref() == device_name
        {
            return Ok((device.clone(), *support));
        }

        let host = cpal::default_host();
        let device = if let Some(name) = device_name {
            let found = host.output_devices()?.find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&name.to_lowercase()))
                    .unwrap_or(false)
            });
            match found {
                Some(d) => {
                    info!(requested = name, found = d.name().unwrap_or_default(), "audio device matched by name");
                    d
                }
                None => {
                    warn!(requested = name, "audio device not found, falling back to default");
                    host.default_output_device()
                        .ok_or("no audio output device found")?
                }
            }
        } else {
            host.default_output_device()
                .ok_or("no audio output device found")?
        };

        // Probed here so the cost is paid once, not per stream.
        let mut support = DeviceSampleSupport::default();
        if let Ok(cfgs) = device.supported_output_configs() {
            for c in cfgs {
                match c.sample_format() {
                    cpal::SampleFormat::F32 => support.f32 = true,
                    cpal::SampleFormat::I16 => support.i16 = true,
                    cpal::SampleFormat::I32 => support.i32 = true,
                    _ => {}
                }
            }
        }

        self.cached_device = Some((
            device_name.map(|s| s.to_string()),
            device.clone(),
            support,
        ));
        Ok((device, support))
    }

    /// Pre-resolve and cache the output device so the first configure()
    /// does not pay the enumeration cost during stream setup.
    pub fn prewarm(&mut self, device_name: Option<&str>) {
        let started = std::time::Instant::now();
        match self.resolve_device(device_name) {
            Ok((d, support)) => info!(
                device = d.name().unwrap_or_default(),
                supports = ?support,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "audio device prewarmed"
            ),
            Err(e) => warn!(error = %e, "audio device prewarm failed"),
        }
    }

    pub fn configure(
        &mut self,
        format: AudioFormat,
        sample_rate: u32,
        channels: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.configure_with_device(format, sample_rate, channels, None)
    }

    pub fn configure_with_device(
        &mut self,
        format: AudioFormat,
        sample_rate: u32,
        channels: u8,
        device_name: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.stop();
        self.format = format;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.ring_format = RingFormat::of(format);

        #[cfg(feature = "flac")]
        {
            self.flac_stream = if format == AudioFormat::Flac {
                Some(crate::flac_decoder::FlacStreamDecoder::new())
            } else {
                None
            };
        }

        let configure_started = std::time::Instant::now();
        let (device, support) = self.resolve_device(device_name)?;

        let actual_device_name = device.name().unwrap_or_default();
        self.device_name = Some(actual_device_name.clone());
        let usb_hint = if is_usb_dac(&actual_device_name) { " (USB)" } else { "" };
        info!(
            device = %actual_device_name,
            sample_rate, channels, format = %format,
            "Tune Bridge using: {}{}", actual_device_name, usb_hint
        );

        let ring_format = self.ring_format;
        let bps = ring_format.bytes_per_sample();
        let ring_size = RING_BUFFER_FRAMES * channels as usize * bps;
        let config = StreamConfig {
            channels: channels as u16,
            sample_rate: SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let playing = self.playing.clone();
        let volume = self.volume.clone();
        let muted = self.muted.clone();
        self.samples_played.store(0, Ordering::Relaxed);
        self.correction.store(0, Ordering::Relaxed);
        self.net_adjust.store(0, Ordering::Relaxed);
        let samples_played = self.samples_played.clone();

        let rb = HeapRb::<u8>::new(ring_size);
        let (producer, mut consumer) = rb.split();

        // Pick the output sample type that preserves the negotiated samples:
        // matching integer type when the device offers it, f32 fallback
        // (power-of-two scaling: still exact for ≤ 24-bit content).
        enum OutKind {
            I16,
            I32,
            F32,
        }
        let out_kind = match ring_format {
            RingFormat::S16 if support.i16 => OutKind::I16,
            RingFormat::S16 | RingFormat::S24 | RingFormat::S24In32 | RingFormat::S32
                if support.i32 =>
            {
                OutKind::I32
            }
            RingFormat::F32 if support.f32 => OutKind::F32,
            _ if support.f32 => OutKind::F32,
            _ if support.i32 => OutKind::I32,
            _ => OutKind::I16,
        };
        let integer_out = !matches!(out_kind, OutKind::F32) || ring_format == RingFormat::F32;
        self.bit_perfect.store(integer_out as u8, Ordering::Relaxed);
        info!(
            out = match out_kind { OutKind::I16 => "i16", OutKind::I32 => "i32", OutKind::F32 => "f32" },
            ring = ?ring_format,
            bit_perfect_path = integer_out,
            "audio output path selected"
        );

        // Scratch buffer allocated once, outside the callback: a heap
        // allocation inside the real-time audio callback risks glitches.
        let mut scratch: Vec<u8> = vec![0u8; 16384 * bps];

        macro_rules! build_stream {
            ($sample:ty, $convert:expr) => {{
                let convert = $convert;
                device.build_output_stream(
                    &config,
                    move |output: &mut [$sample], _: &cpal::OutputCallbackInfo| {
                        if !playing.load(Ordering::Relaxed) || muted.load(Ordering::Relaxed) {
                            output.fill(Default::default());
                            return;
                        }
                        let vol = volume.load(Ordering::Relaxed);
                        let want_bytes = output.len() * bps;
                        if scratch.len() < want_bytes {
                            scratch.resize(want_bytes, 0);
                        }
                        let got = consumer.pop_slice(&mut scratch[..want_bytes]);
                        let samples = got / bps;
                        samples_played.fetch_add(samples as u64, Ordering::Relaxed);
                        for (i, out) in output.iter_mut().enumerate() {
                            *out = if i < samples {
                                convert(&scratch[i * bps..(i + 1) * bps], vol)
                            } else {
                                Default::default()
                            };
                        }
                    },
                    |err| error!(error = %err, "audio output error"),
                    None,
                )?
            }};
        }

        let stream = match out_kind {
            OutKind::I16 => build_stream!(i16, move |b: &[u8], vol: u32| -> i16 {
                // Ring is S16 on this path: exact passthrough at unity volume.
                let s = i16::from_le_bytes([b[0], b[1]]);
                if vol == 1000 {
                    s
                } else {
                    (s as i64 * vol as i64 / 1000) as i16
                }
            }),
            OutKind::I32 => build_stream!(i32, move |b: &[u8], vol: u32| -> i32 {
                let s = ring_format.to_i32(b);
                if vol == 1000 {
                    s
                } else {
                    (s as i64 * vol as i64 / 1000) as i32
                }
            }),
            OutKind::F32 => build_stream!(f32, move |b: &[u8], vol: u32| -> f32 {
                let s = ring_format.to_f32(b);
                if vol == 1000 { s } else { s * (vol as f32 / 1000.0) }
            }),
        };

        // Start the stream immediately: it outputs silence until the
        // `playing` flag is set. Host-side stream start (CoreAudio, WASAPI)
        // has tens of milliseconds of variable latency — paying it here, and
        // making play() a plain atomic flip, keeps PTS-scheduled starts
        // tight across a zone.
        stream.play().ok();
        self.stream = Some(stream);
        self.producer = Some(producer);

        info!(
            elapsed_ms = configure_started.elapsed().as_millis() as u64,
            "audio output configured"
        );
        Ok(())
    }

    pub fn play(&self) {
        if self.stream.is_some() {
            self.playing.store(true, Ordering::Relaxed);
            info!("audio output started");
        }
    }

    pub fn pause(&self) {
        // Keep the stream running (silence): resuming stays a cheap flip.
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        self.stream = None;
        self.producer = None;
    }

    pub fn flush(&mut self) {
        let fmt = self.format;
        let sr = self.sample_rate;
        let ch = self.channels;
        self.stop();
        let _ = self.configure(fmt, sr, ch);
        self.play();
    }

    pub fn set_volume(&self, level: u8) {
        let scaled = (level as u32 * 1000) / 100;
        self.volume.store(scaled.min(1000), Ordering::Relaxed);
    }

    pub fn set_mute(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// True when the negotiated samples reach the DAC bit-exact (matching
    /// output path; software volume at 100% and unmuted preserve this).
    pub fn bit_perfect_path(&self) -> bool {
        self.bit_perfect.load(Ordering::Relaxed) != 0
    }

    /// Write audio payload bytes (in the negotiated wire format) to the ring.
    /// Returns the number of frames enqueued.
    pub fn write_audio(&mut self, data: &[u8]) -> usize {
        let Some(producer) = self.producer.as_mut() else {
            return 0;
        };

        // Convert to ring bytes. Native PCM passes through untouched —
        // this is the bit-perfect path. FLAC and DSD decode to f32.
        let decoded: Vec<u8>;
        let mut bytes: &[u8] = match self.format {
            AudioFormat::Flac => {
                #[cfg(feature = "flac")]
                {
                    let samples = if let Some(ref mut stream) = self.flac_stream {
                        stream.feed(data)
                    } else {
                        crate::flac_decoder::decode_flac_to_f32(data).unwrap_or_default()
                    };
                    if samples.is_empty() {
                        return 0;
                    }
                    decoded = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                    &decoded
                }
                #[cfg(not(feature = "flac"))]
                {
                    warn!("FLAC data received but flac feature not enabled");
                    return 0;
                }
            }
            AudioFormat::DsdU8 | AudioFormat::DsdU16le | AudioFormat::DsdU32le => {
                let samples = dsd_to_f32(data, dsd_rate_multiplier(self.format));
                if samples.is_empty() {
                    return 0;
                }
                decoded = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                &decoded
            }
            _ => data,
        };

        let ch = self.channels.max(1) as usize;
        let frame_bytes = ch * self.ring_format.bytes_per_sample();

        // Drift correction, applied on the producer side (never in the RT
        // callback). Two regimes:
        // - Fine (crystal drift, a few ms): drop or duplicate at most
        //   MAX_CORRECTION_FRAMES_PER_WRITE frames per packet — each
        //   adjustment is inaudible.
        // - Jump resync (late start, > JUMP_THRESHOLD_FRAMES behind): drop
        //   the full deficit as content arrives, one perceptible jump. The
        //   alternative — spreading a large skip over many seconds — keeps
        //   the endpoint audibly out of sync with the rest of the zone.
        const MAX_CORRECTION_FRAMES_PER_WRITE: i64 = 2;
        const JUMP_THRESHOLD_FRAMES: i64 = 1200; // ~25 ms at 48 kHz
        let pending = self.correction.load(Ordering::Relaxed);
        let frames_in = (bytes.len() / frame_bytes) as i64;
        let mut duplicated: Vec<u8> = Vec::new();
        if pending > JUMP_THRESHOLD_FRAMES && frames_in > 0 {
            let drop_frames = pending.min(frames_in);
            self.correction.fetch_sub(drop_frames, Ordering::Relaxed);
            self.net_adjust.fetch_add(drop_frames, Ordering::Relaxed);
            if drop_frames == frames_in {
                return 0;
            }
            bytes = &bytes[drop_frames as usize * frame_bytes..];
        } else if pending > 0 && frames_in > 1 {
            let drop_frames = pending.min(MAX_CORRECTION_FRAMES_PER_WRITE).min(frames_in - 1);
            if drop_frames > 0 {
                bytes = &bytes[drop_frames as usize * frame_bytes..];
                self.correction.fetch_sub(drop_frames, Ordering::Relaxed);
                self.net_adjust.fetch_add(drop_frames, Ordering::Relaxed);
            }
        } else if pending < 0 && bytes.len() >= frame_bytes {
            let dup_frames = (-pending).min(MAX_CORRECTION_FRAMES_PER_WRITE) as usize;
            duplicated.reserve(dup_frames * frame_bytes + bytes.len());
            for _ in 0..dup_frames {
                duplicated.extend_from_slice(&bytes[..frame_bytes]);
            }
            duplicated.extend_from_slice(bytes);
            self.correction.fetch_add(dup_frames as i64, Ordering::Relaxed);
            self.net_adjust.fetch_sub(dup_frames as i64, Ordering::Relaxed);
            bytes = &duplicated;
        }

        // Only push complete frames to maintain channel alignment.
        // A partial push (odd number of samples for stereo) would permanently
        // swap L/R channels for all subsequent audio → distortion.
        let available = producer.vacant_len();
        let frames_to_push = (available / frame_bytes).min(bytes.len() / frame_bytes);
        producer.push_slice(&bytes[..frames_to_push * frame_bytes]);
        frames_to_push
    }

    /// Ring buffer fill level, in frames.
    pub fn buffer_level(&self) -> usize {
        let frame_bytes = self.channels.max(1) as usize * self.ring_format.bytes_per_sample();
        self.producer
            .as_ref()
            .map(|p| p.occupied_len() / frame_bytes)
            .unwrap_or(0)
    }

    /// Playback position in frames: what the audio callback has actually
    /// consumed since the last `configure()`. `None` if not configured.
    pub fn frames_played(&self) -> Option<u64> {
        if self.channels == 0 {
            return None;
        }
        Some(self.samples_played.load(Ordering::Relaxed) / self.channels as u64)
    }

    /// Content position in frames on the stream timeline: frames consumed
    /// plus the net frames skipped by drift correction. This is what the
    /// servo must compare against the clock — otherwise applied skips never
    /// show up in the measured drift.
    pub fn content_position(&self) -> Option<u64> {
        let played = self.frames_played()? as i64;
        let adjusted = played + self.net_adjust.load(Ordering::Relaxed);
        Some(adjusted.max(0) as u64)
    }

    /// Queue a drift correction. Positive `frames`: playback is behind, frames
    /// will be skipped. Negative: playback is ahead, frames will be duplicated.
    /// Replaces (not accumulates) the pending correction so a stale command
    /// can never pile up with a fresh one.
    pub fn set_correction(&self, frames: i64) {
        self.correction.store(frames, Ordering::Relaxed);
    }
}

impl Default for CpalOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the DSD rate multiplier for a DSD format (64, 128, or 256).
/// DSD_U8 = DSD64 (1 byte = 8 bits = 8 DSD samples), DSD_U16LE/U32LE imply
/// wider container but still DSD64 unless the sample_rate field says otherwise.
/// The actual multiplier is determined by the negotiated sample rate, but for
/// the purpose of decimation we use the container width to determine how many
/// DSD bits per container sample.
fn dsd_rate_multiplier(format: AudioFormat) -> u32 {
    match format {
        AudioFormat::DsdU8 => 64,
        AudioFormat::DsdU16le => 128,
        AudioFormat::DsdU32le => 256,
        _ => 64,
    }
}

/// Convert DSD bitstream data to PCM f32 samples.
///
/// DSD is a 1-bit format at high sample rates (2.8224 MHz for DSD64, i.e. 64x
/// 44.1 kHz). For software playback through a PCM DAC, we decimate the
/// bitstream down to 44.1 kHz by averaging blocks of DSD bits.
///
/// For DSD64: 64 bits (8 bytes) produce one PCM sample at 44.1 kHz.
/// For DSD128: 128 bits (16 bytes) produce one PCM sample at 44.1 kHz.
/// For DSD256: 256 bits (32 bytes) produce one PCM sample at 44.1 kHz.
///
/// Each DSD bit represents +1 or -1 (MSB-first within each byte). Averaging
/// the bit values over a block gives a crude but functional PCM value. A proper
/// implementation would use a windowed sinc FIR filter for better frequency
/// response, but this simple averaging provides adequate playback quality for a
/// first implementation.
fn dsd_to_f32(data: &[u8], rate_multiplier: u32) -> Vec<f32> {
    // Number of DSD bytes that produce one PCM sample at base rate (44.1 kHz).
    // DSD64: 64 bits / 8 = 8 bytes per sample
    // DSD128: 128 bits / 8 = 16 bytes per sample
    // DSD256: 256 bits / 8 = 32 bytes per sample
    let bytes_per_pcm_sample = (rate_multiplier as usize) / 8;
    if bytes_per_pcm_sample == 0 {
        return Vec::new();
    }

    data.chunks_exact(bytes_per_pcm_sample)
        .map(|block| {
            let total_bits = block.len() * 8;
            let ones: u32 = block.iter().map(|b| b.count_ones()).sum();
            // Map [0..total_bits] ones to [-1.0..+1.0]
            // 0 ones = -1.0, all ones = +1.0
            (2.0 * ones as f32 / total_bits as f32) - 1.0
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_format_s16_roundtrip_exact() {
        for v in [i16::MIN, -1, 0, 1, 12345, i16::MAX] {
            let b = v.to_le_bytes();
            assert_eq!(RingFormat::S16.to_i32(&b) >> 16, v as i32);
            // f32 path: /32768 then *32768 is exact (power of two)
            let f = RingFormat::S16.to_f32(&b);
            assert_eq!((f * 32768.0) as i32, v as i32);
        }
    }

    #[test]
    fn ring_format_s24_roundtrip_exact() {
        for v in [-8_388_608i32, -1, 0, 1, 4_194_304, 8_388_607] {
            let full = v.to_le_bytes();
            let b = [full[0], full[1], full[2]];
            assert_eq!(RingFormat::S24.to_i32(&b) >> 8, v);
            let f = RingFormat::S24.to_f32(&b);
            assert_eq!((f as f64 * 8_388_608.0) as i32, v);
        }
    }

    #[test]
    fn ring_format_s32_is_passthrough() {
        for v in [i32::MIN, -1, 0, 1, 123_456_789, i32::MAX] {
            let b = v.to_le_bytes();
            assert_eq!(RingFormat::S32.to_i32(&b), v);
        }
    }

    #[test]
    fn ring_format_f32_normalized_to_i32_exact_for_24bit() {
        // A 24-bit sample normalized by 2^23 must come back exact at 2^31.
        for v in [-8_388_608i32, -1, 0, 1, 8_388_607] {
            let f = v as f32 / 8_388_608.0;
            let b = f.to_le_bytes();
            assert_eq!(RingFormat::F32.to_i32(&b) >> 8, v);
        }
    }
}
