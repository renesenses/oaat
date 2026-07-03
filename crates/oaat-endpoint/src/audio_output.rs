use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

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

pub struct CpalOutput {
    stream: Option<cpal::Stream>,
    producer: Option<ringbuf::HeapProd<f32>>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>, // 0-1000 (0.0-1.0 * 1000)
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
    device_name: Option<String>,
    /// Samples actually consumed from the ring by the audio callback.
    /// Divided by `channels`, this is the playback position in frames —
    /// the reference the drift servo compares against the clock.
    samples_played: Arc<AtomicU64>,
    /// Pending drift correction in frames. Positive: playback is behind,
    /// frames are dropped on write. Negative: playback is ahead, frames are
    /// duplicated on write. Applied at most a few frames per packet so the
    /// correction itself stays inaudible (bulk resync excepted, see
    /// `write_audio`).
    correction: Arc<AtomicI64>,
    /// Net frames adjusted so far: dropped − duplicated. Content position at
    /// the DAC = frames consumed + this adjustment; without it, skipping
    /// would never show up in the measured drift and the servo would skip
    /// forever.
    net_adjust: Arc<AtomicI64>,
    /// Resolved cpal device + f32 support, keyed by the requested name.
    /// Device and config enumeration cost ~1-2 s on macOS: paying them on
    /// every configure() eats the PTS scheduling lead time. `prewarm()`
    /// fills this at startup.
    cached_device: Option<(Option<String>, cpal::Device, bool)>,
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
            device_name: None,
            samples_played: Arc::new(AtomicU64::new(0)),
            correction: Arc::new(AtomicI64::new(0)),
            net_adjust: Arc::new(AtomicI64::new(0)),
            cached_device: None,
            #[cfg(feature = "flac")]
            flac_stream: None,
        }
    }

    /// Resolve the output device by name (or default) plus its f32 support,
    /// preferring the cache. On a cache miss the enumeration cost is paid
    /// and the result cached for subsequent streams.
    fn resolve_device(
        &mut self,
        device_name: Option<&str>,
    ) -> Result<(cpal::Device, bool), Box<dyn std::error::Error>> {
        if let Some((cached_key, device, supports_f32)) = &self.cached_device
            && cached_key.as_deref() == device_name
        {
            return Ok((device.clone(), *supports_f32));
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

        // f32 preferred, i16/i32 fallback (I2S DACs, see configure). Probed
        // here so the cost is paid once, not per stream.
        let supports_f32 = device
            .supported_output_configs()
            .map(|cfgs| {
                cfgs.into_iter()
                    .any(|c| c.sample_format() == cpal::SampleFormat::F32)
            })
            .unwrap_or(false);

        self.cached_device = Some((
            device_name.map(|s| s.to_string()),
            device.clone(),
            supports_f32,
        ));
        Ok((device, supports_f32))
    }

    /// Pre-resolve and cache the output device so the first configure()
    /// does not pay the enumeration cost during stream setup.
    pub fn prewarm(&mut self, device_name: Option<&str>) {
        let started = std::time::Instant::now();
        match self.resolve_device(device_name) {
            Ok((d, supports_f32)) => info!(
                device = d.name().unwrap_or_default(),
                supports_f32,
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

        #[cfg(feature = "flac")]
        {
            self.flac_stream = if format == AudioFormat::Flac {
                Some(crate::flac_decoder::FlacStreamDecoder::new())
            } else {
                None
            };
        }

        let configure_started = std::time::Instant::now();
        let (device, supports_f32) = self.resolve_device(device_name)?;

        let actual_device_name = device.name().unwrap_or_default();
        self.device_name = Some(actual_device_name.clone());
        let usb_hint = if is_usb_dac(&actual_device_name) { " (USB)" } else { "" };
        info!(
            device = %actual_device_name,
            sample_rate, channels, format = %format,
            "Tune Bridge using: {}{}", actual_device_name, usb_hint
        );

        let ring_size = RING_BUFFER_FRAMES * channels as usize;
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
        let samples_played_i32 = self.samples_played.clone();

        // Prefer f32, fallback to i32 (most compatible with I2S DACs).
        // Many I2S DACs (ESS 9038 via hifiberry overlay) accept S32_LE in
        // ALSA but only output sound with S16_LE. The probe result comes
        // from resolve_device's cache.
        let rb = HeapRb::<f32>::new(ring_size);
        let (producer, mut consumer) = rb.split();

        let stream = if supports_f32 {
            info!("opening audio output (f32)");
            device.build_output_stream(
                &config,
                move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !playing.load(Ordering::Relaxed) || muted.load(Ordering::Relaxed) {
                        output.fill(0.0);
                        return;
                    }
                    let vol = volume.load(Ordering::Relaxed) as f32 / 1000.0;
                    let read = consumer.pop_slice(output);
                    samples_played.fetch_add(read as u64, Ordering::Relaxed);
                    for sample in &mut output[..read] {
                        *sample *= vol;
                    }
                    output[read..].fill(0.0);
                },
                |err| error!(error = %err, "audio output error"),
                None,
            )?
        } else {
            info!("opening audio output (i32/S32_LE)");
            // Scratch buffer allocated once, outside the callback: a heap
            // allocation inside the real-time audio callback risks glitches.
            // Grows only if the host ever delivers a larger buffer.
            let mut tmp: Vec<f32> = vec![0.0f32; 8192];
            device.build_output_stream(
                &config,
                move |output: &mut [i32], _: &cpal::OutputCallbackInfo| {
                    if !playing.load(Ordering::Relaxed) || muted.load(Ordering::Relaxed) {
                        output.fill(0);
                        return;
                    }
                    let vol = volume.load(Ordering::Relaxed) as f32 / 1000.0;
                    if tmp.len() < output.len() {
                        tmp.resize(output.len(), 0.0);
                    }
                    let read = consumer.pop_slice(&mut tmp[..output.len()]);
                    samples_played_i32.fetch_add(read as u64, Ordering::Relaxed);
                    for (i, sample) in output.iter_mut().enumerate() {
                        if i < read {
                            let s = (tmp[i] * vol).clamp(-1.0, 1.0);
                            *sample = (s * (i32::MAX - 256) as f32) as i32;
                        } else {
                            *sample = 0;
                        }
                    }
                },
                |err| error!(error = %err, "audio output error"),
                None,
            )?
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

    pub fn write_audio(&mut self, data: &[u8]) -> usize {
        let Some(producer) = self.producer.as_mut() else {
            return 0;
        };

        let mut samples = if self.format == AudioFormat::Flac {
            #[cfg(feature = "flac")]
            {
                if let Some(ref mut stream) = self.flac_stream {
                    let s = stream.feed(data);
                    if s.is_empty() { return 0; }
                    s
                } else {
                    match crate::flac_decoder::decode_flac_to_f32(data) {
                        Ok(s) => s,
                        Err(_) => return 0,
                    }
                }
            }
            #[cfg(not(feature = "flac"))]
            {
                warn!("FLAC data received but flac feature not enabled");
                return 0;
            }
        } else {
            convert_to_f32(self.format, data)
        };

        let ch = self.channels.max(1) as usize;

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
        let frames_in = (samples.len() / ch) as i64;
        if pending > JUMP_THRESHOLD_FRAMES && frames_in > 0 {
            let drop_frames = pending.min(frames_in);
            self.correction.fetch_sub(drop_frames, Ordering::Relaxed);
            self.net_adjust.fetch_add(drop_frames, Ordering::Relaxed);
            if drop_frames == frames_in {
                return 0;
            }
            samples.drain(..drop_frames as usize * ch);
        } else if pending > 0 && samples.len() > ch {
            let drop_frames = pending
                .min(MAX_CORRECTION_FRAMES_PER_WRITE)
                .min(frames_in - 1);
            if drop_frames > 0 {
                samples.drain(..drop_frames as usize * ch);
                self.correction.fetch_sub(drop_frames, Ordering::Relaxed);
                self.net_adjust.fetch_add(drop_frames, Ordering::Relaxed);
            }
        } else if pending < 0 && samples.len() >= ch {
            let dup_frames = (-pending).min(MAX_CORRECTION_FRAMES_PER_WRITE);
            for _ in 0..dup_frames {
                let first_frame: Vec<f32> = samples[..ch].to_vec();
                samples.splice(0..0, first_frame);
            }
            self.correction.fetch_add(dup_frames, Ordering::Relaxed);
            self.net_adjust.fetch_sub(dup_frames, Ordering::Relaxed);
        }

        // Only push complete frames to maintain channel alignment.
        // A partial push (odd number of samples for stereo) would permanently
        // swap L/R channels for all subsequent audio → distortion.
        let available = producer.vacant_len();
        let frames_to_push = (available / ch).min(samples.len() / ch);
        let samples_to_push = frames_to_push * ch;
        producer.push_slice(&samples[..samples_to_push]);
        frames_to_push
    }

    pub fn buffer_level(&self) -> usize {
        self.producer
            .as_ref()
            .map(|p| p.occupied_len())
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

fn convert_to_f32(format: AudioFormat, data: &[u8]) -> Vec<f32> {
    match format {
        AudioFormat::PcmS16le => data
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / i16::MAX as f32)
            .collect(),
        AudioFormat::PcmS24le => data
            .chunks_exact(3)
            .map(|b| {
                let sign = if b[2] & 0x80 != 0 { 0xFF } else { 0 };
                let val = i32::from_le_bytes([b[0], b[1], b[2], sign]);
                val as f32 / 8_388_607.0
            })
            .collect(),
        AudioFormat::PcmS24le4 | AudioFormat::PcmS32le => data
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / i32::MAX as f32)
            .collect(),
        AudioFormat::PcmF32le => data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        AudioFormat::DsdU8 | AudioFormat::DsdU16le | AudioFormat::DsdU32le => {
            dsd_to_f32(data, dsd_rate_multiplier(format))
        }
        _ => {
            warn!(format = %format, "unsupported format, silence");
            vec![0.0f32; data.len() / 2]
        }
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
