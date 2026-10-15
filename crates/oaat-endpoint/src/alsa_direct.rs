//! Direct ALSA output for Linux endpoints.
//!
//! Everything happens in-process (#44, #45): FLAC is decoded with Symphonia
//! (`FlacStreamDecoder`), and PCM/DSD frames are written to the ALSA PCM
//! with the `alsa` crate. No `ffmpeg`, no `aplay`, no subprocess.
//!
//! A dedicated writer thread owns the ALSA device. `write_audio()` converts
//! the payload and hands it over through a bounded channel: when the channel
//! is full it blocks, which gives the same clean backpressure as the former
//! `aplay` stdin pipe (#7). The thread:
//! - recovers an underrun (`EPIPE`) with `snd_pcm_prepare` and a suspend
//!   (`ESTRPIPE`) with `snd_pcm_resume`, then rewrites the pending frames;
//! - reopens the device with capped exponential backoff after a fatal error
//!   (DAC unplugged), dropping audio while the device is absent instead of
//!   blocking the caller (#33). The FLAC decoder lives on the caller's side,
//!   so a reopen needs no stream header: decoding simply goes on (#45);
//! - retries a device another program holds (`EBUSY`) every `BUSY_RETRY`,
//!   also before any audio arrives, for `BUSY_WAIT`: on a host that hands
//!   the DAC from one player to another, the other one is about to let go;
//! - closes the device after `IDLE_CLOSE` without audio, so an endpoint
//!   whose stream ended does not keep the DAC open, and reopens it on the
//!   next audio (#44, #12).
//!
//! `stop()` is intentional: like closing the `aplay` pipe did, it plays out
//! what is already queued, then closes the device. The thread is joined, so
//! the device is released when `stop()` returns, and nothing reopens it
//! before the next `configure()`. `flush()` (seek) discards instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use oaat_core::format::AudioFormat;
use tracing::{info, warn};

/// ALSA ring-buffer size requested, in microseconds. 0.5s was too small to
/// absorb network jitter + clock drift on a continuous stream, so the device
/// XRUN'd repeatedly — audible as frequent micro-dropouts on BOTH live radio
/// (FIP) and steady FLAC (Qobuz), independent of source. 2s gives enough
/// slack to ride out jitter without a dropout. ALSA clamps to the hardware
/// maximum, which some drivers cap in BYTES: the request that yields 2s at
/// S16/48k grants only 1s at S32/44.1k (#7).
const ALSA_BUFFER_TIME_US: u32 = 2_000_000;

/// Requested ALSA period. With a byte-capped driver the buffer is granted as
/// a whole number of periods, so coarse periods forfeit up to one period of
/// capacity (observed on the I-Sabre ES9038Q2M: 2 × 500ms granted where the
/// cap allowed ~1.1s, #7). Finer periods claim the cap almost exactly.
const ALSA_PERIOD_TIME_US: u32 = 125_000;

/// Payloads queued between `write_audio()` and the writer thread. With
/// typical payloads of a few KiB this is about 1 MiB, the userspace slack
/// the enlarged `aplay` pipe used to give (#7): bursts are absorbed with
/// blocking backpressure instead of overflowing the UDP socket.
const WRITE_QUEUE_PAYLOADS: usize = 256;

/// Without audio for this long, the device is drained and closed (#44); the
/// next payload reopens it. Covers a stream that ends without a `stop`, and
/// a long pause.
const IDLE_CLOSE: Duration = Duration::from_secs(10);

/// First reopen delay after a fatal device error (#33).
const RESPAWN_BACKOFF_MIN: Duration = Duration::from_millis(250);
/// Reopen delay cap while the device keeps failing (e.g. DAC unplugged).
const RESPAWN_BACKOFF_MAX: Duration = Duration::from_secs(8);
/// A device that played at least this long was healthy: the next failure
/// starts the backoff over instead of continuing to double it.
const RESPAWN_STABLE_RUN: Duration = Duration::from_secs(5);

/// Reopen delay while another program holds the device (`EBUSY`). On a host
/// that switches players, the other one is about to let go: retry often.
const BUSY_RETRY: Duration = Duration::from_millis(100);
/// How long a busy device is retried without audio coming in: covers the
/// wait between `configure()` and the first audio, while the endpoint holds
/// its format accept back until the device is open. After that it is only
/// retried as audio arrives, so an endpoint nobody streams to doesn't grab
/// the DAC the moment its owner lets go.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// What the writer thread opens: device name, ALSA sample format (as ALSA
/// names it), rate and channel count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceParams {
    pub device: String,
    pub format: &'static str,
    pub rate: u32,
    pub channels: u8,
}

impl DeviceParams {
    fn frame_bytes(&self) -> usize {
        let sample = if self.format == "S16_LE" { 2 } else { 4 };
        sample * self.channels.max(1) as usize
    }
}

/// A write or recovery failure of the device.
#[derive(Debug)]
pub(crate) enum DeviceError {
    /// Underrun (`EPIPE`): recoverable with `prepare`.
    Xrun,
    /// The system suspended the device (`ESTRPIPE`): recoverable.
    Suspended,
    /// Anything else (device unplugged, I/O error): close and reopen.
    Fatal(String),
}

/// The ALSA PCM as the writer thread sees it. Dropping it closes the device.
/// Tests substitute a fake: ALSA is not available in CI.
pub(crate) trait PcmDevice: Send {
    /// Write interleaved frames (whole frames only). Returns the number of
    /// frames written, which may be fewer than given.
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError>;
    /// Recover from `Xrun` or `Suspended` so writing can go on.
    fn recover(&mut self, err: &DeviceError) -> Result<(), DeviceError>;
    /// Play out what is buffered (idle close).
    fn drain(&mut self);
    /// Discard what is buffered (intentional stop).
    fn discard(&mut self);
}

/// Why the device could not be opened.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// Another program holds the device (`EBUSY`).
    Busy(String),
    /// Anything else (device absent, parameters not supported).
    Failed(String),
}

impl From<String> for OpenError {
    fn from(e: String) -> Self {
        OpenError::Failed(e)
    }
}

pub(crate) type DeviceOpener =
    Arc<dyn Fn(&DeviceParams) -> Result<Box<dyn PcmDevice>, OpenError> + Send + Sync>;

/// State shared between the output and its writer thread.
#[derive(Default)]
struct Shared {
    /// Underruns during active playback (cumulative across streams).
    underruns: AtomicU64,
    /// Incremented by `pause()`. A payload that follows a pause may hit the
    /// xrun the pause caused: that one is not a playback incident.
    pause_gen: AtomicU64,
    /// The device is currently open.
    open: AtomicBool,
}

struct Writer {
    tx: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

pub struct AlsaDirectOutput {
    writer: Option<Writer>,
    opener: DeviceOpener,
    shared: Arc<Shared>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
    device_name: Option<String>,
    idle_close: Duration,
    #[cfg(feature = "flac")]
    flac_stream: Option<crate::flac_decoder::FlacStreamDecoder>,
}

impl AlsaDirectOutput {
    /// ALSA playback devices, one line per card/device, in the format of
    /// `aplay -l` (`card 1: X [Y], device 0: Z [W]`).
    pub fn list_devices() -> Vec<String> {
        let mut out = Vec::new();
        for card in alsa::card::Iter::new().flatten() {
            let Ok(ctl) = alsa::Ctl::from_card(&card, false) else {
                continue;
            };
            let Ok(info) = ctl.card_info() else { continue };
            let card_id = info.get_id().unwrap_or("").to_string();
            let card_name = info.get_name().unwrap_or("").to_string();
            for dev in alsa::ctl::DeviceIter::new(&ctl) {
                let Ok(pi) = ctl.pcm_info(dev as u32, 0, alsa::Direction::Playback) else {
                    continue;
                };
                out.push(format!(
                    "card {}: {} [{}], device {}: {} [{}]",
                    card.get_index(),
                    card_id,
                    card_name,
                    dev,
                    pi.get_id().unwrap_or(""),
                    pi.get_name().unwrap_or("")
                ));
            }
        }
        out
    }

    pub fn default_device_name() -> Option<String> {
        Some("default".to_string())
    }

    pub fn auto_detect_usb_dac() -> Option<String> {
        let devices = Self::list_devices();
        // On ALSA, prefer device with USB/DAC in name
        for d in &devices {
            let lower = d.to_lowercase();
            if lower.contains("usb") || lower.contains("dac") {
                return Some(d.clone());
            }
        }
        // Fallback: use sysdefault:CARD=X if only one card exists (likely USB DAC)
        let cards: Vec<_> = devices
            .iter()
            .filter(|d| d.starts_with("sysdefault:CARD="))
            .collect();
        if cards.len() == 1 {
            return Some(cards[0].clone());
        }
        // Last resort: first non-builtin sysdefault
        for d in &devices {
            if d.starts_with("sysdefault:CARD=") {
                let lower = d.to_lowercase();
                if !lower.contains("hdmi") && !lower.contains("builtin") {
                    return Some(d.clone());
                }
            }
        }
        None
    }

    pub fn current_device_name(&self) -> Option<&str> {
        Some("alsa-direct")
    }

    pub fn new() -> Self {
        Self::with_opener(Arc::new(open_alsa_device))
    }

    pub(crate) fn with_opener(opener: DeviceOpener) -> Self {
        Self {
            writer: None,
            opener,
            shared: Arc::new(Shared::default()),
            playing: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(1000)),
            muted: Arc::new(AtomicBool::new(false)),
            sample_rate: 0,
            channels: 0,
            format: AudioFormat::PcmS16le,
            device_name: None,
            idle_close: IDLE_CLOSE,
            #[cfg(feature = "flac")]
            flac_stream: None,
        }
    }

    /// ALSA underruns during active playback since this output was created
    /// (cumulative across streams and device reopens). Resumes after a
    /// pause or an idle period are not counted (#12).
    pub fn underrun_count(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// True while the ALSA device is open.
    pub fn device_open(&self) -> bool {
        self.shared.open.load(Ordering::Relaxed)
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
        self.device_name = device_name.map(|s| s.to_string());

        let mut device = match device_name {
            Some(d)
                if d.starts_with("hw:")
                    || d.starts_with("plughw:")
                    || d.starts_with("default")
                    || d.starts_with("sysdefault:") =>
            {
                d.to_string()
            }
            _ => "default".to_string(),
        };

        // Native DSD requires a raw hw: device — plug/sysdefault cannot open
        // SPECIAL DSD_U32_BE. Map sysdefault:CARD=X → hw:CARD=X,DEV=0.
        if format.is_dsd() {
            device = dsd_hw_device(&device);
        }

        let (alsa_fmt, alsa_rate) = if format == AudioFormat::Flac {
            #[cfg(not(feature = "flac"))]
            return Err("FLAC stream but the flac feature is not enabled".into());
            #[cfg(feature = "flac")]
            {
                self.flac_stream = Some(crate::flac_decoder::FlacStreamDecoder::new());
                (flac_alsa_format(channels), sample_rate)
            }
        } else {
            #[cfg(feature = "flac")]
            {
                self.flac_stream = None;
            }
            alsa_format_and_rate(format, sample_rate)
                .ok_or_else(|| format!("unsupported format for ALSA direct: {format}"))?
        };

        let params = DeviceParams {
            device,
            format: alsa_fmt,
            rate: alsa_rate,
            channels,
        };
        info!(
            device = %params.device,
            format = alsa_fmt,
            sample_rate = alsa_rate,
            wire_format = %format,
            wire_sample_rate = sample_rate,
            channels,
            "ALSA direct output configured (in-process, no subprocess)"
        );
        self.writer = Some(spawn_writer(
            params,
            self.opener.clone(),
            self.shared.clone(),
            self.idle_close,
        ));
        self.playing.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn play(&self) {
        self.playing.store(true, Ordering::Relaxed);
        info!("audio output started (ALSA direct)");
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Relaxed);
        self.shared.pause_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// Intentional stop: play out what is queued (as closing the `aplay`
    /// pipe did, so the end of a track is not cut), then release the device.
    /// When this returns the ALSA device is closed. Nothing is reopened until
    /// the next `configure()`.
    pub fn stop(&mut self) {
        self.shutdown(false);
    }

    /// Give up the device now (endpoint_release): unlike `stop()`, queued
    /// audio is dropped, so another source can open the DAC right away.
    /// When this returns the ALSA device is closed.
    pub fn release(&mut self) {
        self.shutdown(true);
    }

    /// True between `play()` and `pause()`/`stop()`/`release()`.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Stop the writer thread; `discard` drops queued audio instead of
    /// playing it out.
    fn shutdown(&mut self, discard: bool) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(w) = self.writer.take() {
            w.stop.store(discard, Ordering::Relaxed);
            drop(w.tx);
            if w.handle.join().is_err() {
                warn!("ALSA writer thread panicked");
            }
        }
        #[cfg(feature = "flac")]
        if let Some(dec) = self.flac_stream.as_mut() {
            dec.reset();
        }
    }

    pub fn flush(&mut self) {
        let fmt = self.format;
        let sr = self.sample_rate;
        let ch = self.channels;
        let dev = self.device_name.clone();
        // A seek: what is queued belongs to the old position.
        self.shutdown(true);
        if let Err(e) = self.configure_with_device(fmt, sr, ch, dev.as_deref()) {
            warn!(error = %e, "flush: reconfigure failed");
        }
        self.play();
    }

    pub fn set_volume(&self, level: u8) {
        let scaled = (level as u32 * 1000) / 100;
        self.volume.store(scaled.min(1000), Ordering::Relaxed);
    }

    pub fn set_mute(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Convert the payload to the device format and queue it for the writer
    /// thread. Blocks while the queue is full (backpressure). Returns the
    /// number of frames queued.
    pub fn write_audio(&mut self, data: &[u8]) -> usize {
        if !self.playing.load(Ordering::Relaxed) || self.muted.load(Ordering::Relaxed) {
            return 0;
        }
        if self.writer.is_none() {
            return 0;
        }
        let vol = self.volume.load(Ordering::Relaxed) as f32 / 1000.0;
        let unity = (vol - 1.0).abs() < 0.001;

        let (bytes, frames) = if self.format == AudioFormat::Flac {
            #[cfg(feature = "flac")]
            {
                let Some(dec) = self.flac_stream.as_mut() else {
                    return 0;
                };
                let samples = dec.feed(data);
                if samples.is_empty() {
                    return 0;
                }
                let frames = samples.len() / self.channels.max(1) as usize;
                (flac_samples_to_device(&samples, self.channels, vol), frames)
            }
            #[cfg(not(feature = "flac"))]
            {
                return 0;
            }
        } else {
            let frames = data
                .len()
                .checked_div(bytes_per_frame(self.format, self.channels))
                .unwrap_or(0);
            let bytes = match self.format {
                // DSD is bit-perfect: ignore software volume (DAC volume only).
                AudioFormat::DsdU8 => pack_dsd_u8_to_u32_be(data, self.channels),
                AudioFormat::DsdU32le => dsd_u32le_to_be(data),
                AudioFormat::PcmS24le if unity => pad_s24_to_s32(data),
                AudioFormat::PcmS24le => pad_s24_to_s32(&apply_volume(self.format, data, vol)),
                _ if unity => data.to_vec(),
                _ => apply_volume(self.format, data, vol),
            };
            (bytes, frames)
        };
        if bytes.is_empty() {
            return 0;
        }

        let Some(w) = self.writer.as_ref() else {
            return 0;
        };
        match w.tx.try_send(bytes) {
            Ok(()) => frames,
            Err(TrySendError::Full(bytes)) => match w.tx.send(bytes) {
                Ok(()) => frames,
                Err(_) => 0,
            },
            Err(TrySendError::Disconnected(_)) => 0,
        }
    }

    pub fn buffer_level(&self) -> usize {
        0
    }

    /// Playback position is not tracked on this output.
    /// Drift correction is unavailable on this output (see CpalOutput).
    pub fn frames_played(&self) -> Option<u64> {
        None
    }

    /// See `frames_played`: no position tracking on this output.
    pub fn content_position(&self) -> Option<u64> {
        None
    }

    /// No-op: the device is opened per stream, there is nothing to prewarm.
    pub fn prewarm(&mut self, _device_name: Option<&str>) {}

    /// Raw wire bytes reach ALSA in the matching sample format: bit-perfect
    /// by construction (software volume aside; FLAC ≤ 24-bit is decoded
    /// losslessly and written as S32_LE).
    pub fn bit_perfect_path(&self) -> bool {
        true
    }

    /// No-op: no sample-accurate insertion point on this output.
    pub fn set_correction(&self, _frames: i64) {}
}

impl Default for AlsaDirectOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AlsaDirectOutput {
    fn drop(&mut self) {
        self.shutdown(true);
    }
}

fn spawn_writer(
    params: DeviceParams,
    opener: DeviceOpener,
    shared: Arc<Shared>,
    idle_close: Duration,
) -> Writer {
    let (tx, rx) = std::sync::mpsc::sync_channel(WRITE_QUEUE_PAYLOADS);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let handle = std::thread::Builder::new()
        .name("oaat-alsa-writer".into())
        .spawn(move || writer_loop(rx, params, opener, shared, stop_t, idle_close))
        .expect("spawn ALSA writer thread");
    Writer { tx, stop, handle }
}

/// Device state machine of the writer thread.
struct DeviceSlot {
    params: DeviceParams,
    opener: DeviceOpener,
    shared: Arc<Shared>,
    dev: Option<Box<dyn PcmDevice>>,
    opened_at: Option<Instant>,
    retry_at: Option<Instant>,
    backoff: Duration,
    /// Since when the device has been busy (`EBUSY`), while it is.
    busy_since: Option<Instant>,
}

impl DeviceSlot {
    /// Open the device if it is closed and no backoff is pending. Returns
    /// whether the device is open.
    fn ensure_open(&mut self) -> bool {
        if self.dev.is_some() {
            return true;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        match (self.opener)(&self.params) {
            Ok(dev) => {
                if let Some(since) = self.busy_since.take() {
                    info!(
                        device = %self.params.device,
                        waited_ms = since.elapsed().as_millis() as u64,
                        "ALSA device no longer busy, opened"
                    );
                } else if self.retry_at.is_some() {
                    info!(device = %self.params.device, "ALSA device reopened after a failure");
                }
                self.dev = Some(dev);
                self.opened_at = Some(Instant::now());
                self.retry_at = None;
                self.shared.open.store(true, Ordering::Relaxed);
                true
            }
            Err(OpenError::Busy(e)) => {
                if self.busy_since.is_none() {
                    self.busy_since = Some(Instant::now());
                    warn!(
                        device = %self.params.device,
                        error = %e,
                        retry_in_ms = BUSY_RETRY.as_millis() as u64,
                        "ALSA device busy, retrying"
                    );
                }
                self.retry_at = Some(Instant::now() + BUSY_RETRY);
                false
            }
            Err(OpenError::Failed(e)) => {
                self.busy_since = None;
                self.schedule_retry();
                warn!(
                    device = %self.params.device,
                    error = %e,
                    retry_in_ms = self.backoff.as_millis() as u64,
                    "ALSA device open failed"
                );
                false
            }
        }
    }

    /// When to retry a busy device although no audio came in, if it should.
    fn busy_retry_at(&self) -> Option<Instant> {
        if self.dev.is_none() && self.busy_since.is_some_and(|t| t.elapsed() < BUSY_WAIT) {
            self.retry_at
        } else {
            None
        }
    }

    fn schedule_retry(&mut self) {
        let stable = self
            .opened_at
            .is_some_and(|t| t.elapsed() >= RESPAWN_STABLE_RUN);
        self.backoff = if stable || self.backoff.is_zero() {
            RESPAWN_BACKOFF_MIN
        } else {
            (self.backoff * 2).min(RESPAWN_BACKOFF_MAX)
        };
        self.opened_at = None;
        self.retry_at = Some(Instant::now() + self.backoff);
    }

    /// Close after a fatal error; a later payload reopens after the backoff.
    fn fail(&mut self, error: &str) {
        self.dev = None;
        self.shared.open.store(false, Ordering::Relaxed);
        self.schedule_retry();
        warn!(
            device = %self.params.device,
            error,
            retry_in_ms = self.backoff.as_millis() as u64,
            "ALSA device lost, will reopen"
        );
    }

    fn close(&mut self, drain: bool) {
        if let Some(mut dev) = self.dev.take() {
            if drain {
                dev.drain();
            } else {
                dev.discard();
            }
            drop(dev);
            self.shared.open.store(false, Ordering::Relaxed);
        }
        self.opened_at = None;
    }

    /// Write one payload, recovering underruns in place.
    /// `active`: audio was flowing before this payload (no pause, no idle
    /// period), so an underrun here is a real playback incident.
    fn write_all(&mut self, data: &[u8], active: bool) {
        let frame = self.params.frame_bytes();
        let data = &data[..data.len() - data.len() % frame];
        let mut off = 0;
        let mut recoveries = 0;
        while off < data.len() {
            let Some(dev) = self.dev.as_mut() else { return };
            match dev.write(&data[off..]) {
                Ok(0) => return,
                Ok(frames) => off += frames * frame,
                Err(DeviceError::Fatal(e)) => return self.fail(&e),
                Err(e) => {
                    recoveries += 1;
                    if recoveries > 3 {
                        return self.fail("underrun recovery keeps failing");
                    }
                    match e {
                        DeviceError::Xrun if active => {
                            let total = self.shared.underruns.fetch_add(1, Ordering::Relaxed) + 1;
                            warn!(total, "ALSA underrun, recovered");
                        }
                        _ => info!(?e, "ALSA resume after pause or suspend, not counted as underrun"),
                    }
                    if let Err(e) = dev.recover(&e) {
                        return self.fail(&format!("recovery failed: {e:?}"));
                    }
                }
            }
        }
    }
}

fn writer_loop(
    rx: Receiver<Vec<u8>>,
    params: DeviceParams,
    opener: DeviceOpener,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    idle_close: Duration,
) {
    let mut slot = DeviceSlot {
        params,
        opener,
        shared,
        dev: None,
        opened_at: None,
        retry_at: None,
        backoff: Duration::ZERO,
        busy_since: None,
    };
    // Open right away: the first audio should not pay for it.
    slot.ensure_open();
    let mut last_write = Instant::now();
    let mut pause_gen = slot.shared.pause_gen.load(Ordering::Relaxed);
    loop {
        let busy_retry_at = slot.busy_retry_at();
        let wait = busy_retry_at.map_or(idle_close, |at| {
            at.saturating_duration_since(Instant::now())
        });
        let data = match rx.recv_timeout(wait) {
            Ok(d) => d,
            Err(RecvTimeoutError::Timeout) if busy_retry_at.is_some() => {
                slot.ensure_open();
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {
                if slot.dev.is_some() {
                    info!(idle_ms = idle_close.as_millis() as u64, "no audio, ALSA device closed until the next stream");
                    slot.close(true);
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if stop.load(Ordering::Relaxed) {
            break; // flush/drop: the rest of the queue is discarded
        }
        if !slot.ensure_open() {
            // Device absent: drop the audio rather than block the caller.
            continue;
        }
        let gen_now = slot.shared.pause_gen.load(Ordering::Relaxed);
        let active = gen_now == pause_gen && last_write.elapsed() < idle_close;
        pause_gen = gen_now;
        slot.write_all(&data, active);
        last_write = Instant::now();
    }
    let discard = stop.load(Ordering::Relaxed);
    slot.close(!discard);
}

/// The real device: an ALSA PCM opened with the `alsa` crate.
struct AlsaPcm {
    pcm: alsa::PCM,
}

fn alsa_errno(e: &alsa::Error) -> i32 {
    e.errno().abs()
}

impl PcmDevice for AlsaPcm {
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        let mut interrupted = 0;
        loop {
            match self.pcm.io_bytes().writei(data) {
                Ok(n) => return Ok(n),
                Err(e) => match alsa_errno(&e) {
                    libc::EPIPE => return Err(DeviceError::Xrun),
                    libc::ESTRPIPE => return Err(DeviceError::Suspended),
                    libc::EAGAIN | libc::EINTR if interrupted < 100 => interrupted += 1,
                    _ => return Err(DeviceError::Fatal(e.to_string())),
                },
            }
        }
    }

    fn recover(&mut self, err: &DeviceError) -> Result<(), DeviceError> {
        if matches!(err, DeviceError::Suspended) {
            // Wait for the driver to finish resuming, then prepare if needed.
            for _ in 0..50 {
                match self.pcm.resume() {
                    Ok(()) => return Ok(()),
                    Err(e) if alsa_errno(&e) == libc::EAGAIN => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
        }
        self.pcm
            .prepare()
            .map_err(|e| DeviceError::Fatal(e.to_string()))
    }

    fn drain(&mut self) {
        let _ = self.pcm.drain();
    }

    fn discard(&mut self) {
        let _ = self.pcm.drop();
    }
}

fn alsa_format(name: &str) -> Option<alsa::pcm::Format> {
    use alsa::pcm::Format;
    Some(match name {
        "S16_LE" => Format::S16LE,
        "S24_LE" => Format::S24LE,
        "S32_LE" => Format::S32LE,
        "FLOAT_LE" => Format::FloatLE,
        "DSD_U32_BE" => Format::DSDU32BE,
        _ => return None,
    })
}

/// Open and configure the ALSA PCM: interleaved access, exact format and
/// channels, nearest rate, ~2 s buffer in 125 ms periods, start once the
/// buffer is full (what `aplay` did).
fn open_alsa_device(p: &DeviceParams) -> Result<Box<dyn PcmDevice>, OpenError> {
    use alsa::pcm::{Access, HwParams};
    use alsa::{Direction, PCM, ValueOr};

    let fmt = alsa_format(p.format).ok_or_else(|| format!("unknown ALSA format {}", p.format))?;
    let err = |what: &str, e: alsa::Error| format!("{}: {what}: {e}", p.device);
    let pcm = PCM::new(&p.device, Direction::Playback, false).map_err(|e| {
        if alsa_errno(&e) == libc::EBUSY {
            OpenError::Busy(err("open", e))
        } else {
            OpenError::Failed(err("open", e))
        }
    })?;
    {
        let hwp = HwParams::any(&pcm).map_err(|e| err("hw params", e))?;
        hwp.set_access(Access::RWInterleaved)
            .map_err(|e| err("access", e))?;
        hwp.set_format(fmt).map_err(|e| err(p.format, e))?;
        hwp.set_channels(p.channels as u32)
            .map_err(|e| err("channels", e))?;
        hwp.set_rate(p.rate, ValueOr::Nearest)
            .map_err(|e| err("rate", e))?;
        hwp.set_period_time_near(ALSA_PERIOD_TIME_US, ValueOr::Nearest)
            .map_err(|e| err("period time", e))?;
        hwp.set_buffer_time_near(ALSA_BUFFER_TIME_US, ValueOr::Nearest)
            .map_err(|e| err("buffer time", e))?;
        pcm.hw_params(&hwp).map_err(|e| err("apply hw params", e))?;
    }
    let (rate, buffer, period) = {
        let hwp = pcm.hw_params_current().map_err(|e| err("hw params", e))?;
        (
            hwp.get_rate().unwrap_or(0),
            hwp.get_buffer_size().unwrap_or(0),
            hwp.get_period_size().unwrap_or(0),
        )
    };
    {
        let swp = pcm.sw_params_current().map_err(|e| err("sw params", e))?;
        swp.set_start_threshold(buffer.max(1))
            .map_err(|e| err("start threshold", e))?;
        swp.set_avail_min(period.max(1))
            .map_err(|e| err("avail min", e))?;
        pcm.sw_params(&swp).map_err(|e| err("apply sw params", e))?;
    }
    if rate != p.rate {
        warn!(requested = p.rate, granted = rate, "ALSA rate is not exact");
    }
    // What `aplay -v` used to print: exposes a driver that clamps the
    // requested buffer (#7).
    info!(
        device = %p.device,
        format = p.format,
        rate,
        channels = p.channels,
        buffer_frames = buffer,
        period_frames = period,
        buffer_ms = if rate > 0 { buffer as u64 * 1000 / rate as u64 } else { 0 },
        "ALSA device opened"
    );
    Ok(Box::new(AlsaPcm { pcm }))
}

/// Device format for decoded FLAC. Same choice as the former ffmpeg pipe:
/// S32_LE up to stereo (lossless for ≤ 24-bit sources), S16_LE beyond.
#[cfg(feature = "flac")]
fn flac_alsa_format(channels: u8) -> &'static str {
    if channels <= 2 { "S32_LE" } else { "S16_LE" }
}

/// Normalized f32 samples from the FLAC decoder → device bytes, with
/// software volume. The decoder scales by powers of two, so at unity volume
/// a ≤ 24-bit source comes out bit-exact in S32_LE.
#[cfg(feature = "flac")]
fn flac_samples_to_device(samples: &[f32], channels: u8, vol: f32) -> Vec<u8> {
    let unity = (vol - 1.0).abs() < 0.001;
    let s32 = flac_alsa_format(channels) == "S32_LE";
    let mut out = Vec::with_capacity(samples.len() * if s32 { 4 } else { 2 });
    for &s in samples {
        let v = (if unity { s } else { s * vol }) as f64;
        if s32 {
            let i = (v * 2_147_483_648.0).clamp(i32::MIN as f64, i32::MAX as f64) as i32;
            out.extend_from_slice(&i.to_le_bytes());
        } else {
            let i = (v * 32_768.0).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            out.extend_from_slice(&i.to_le_bytes());
        }
    }
    out
}

/// Expand packed 24-bit little-endian samples (3 bytes) into S32_LE (4 bytes),
/// left-justified so the 24 significant bits occupy the high bytes of the 32-bit
/// word (equivalent to `value << 8`). This produces full-scale S32_LE, which
/// every ALSA hardware device accepts — unlike raw S24_LE, which DACs such as
/// the I-Sabre ES9038Q2M reject. Bit-perfect: the low byte is zero-filled and
/// the sign bit is carried naturally by the original MSB (chunk[2]).
fn pad_s24_to_s32(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 3 * 4);
    for chunk in data.as_chunks::<3>().0 {
        out.extend_from_slice(&[0x00, chunk[0], chunk[1], chunk[2]]);
    }
    out
}

/// Map a soft device name to a raw hw: device suitable for native DSD.
/// `sysdefault:CARD=AUDIO` → `hw:CARD=AUDIO,DEV=0`. Already-`hw:` names pass through.
fn dsd_hw_device(device: &str) -> String {
    if device.starts_with("hw:") {
        return device.to_string();
    }
    if let Some(rest) = device.strip_prefix("sysdefault:CARD=") {
        let card = rest.split(',').next().unwrap_or(rest).trim();
        return format!("hw:CARD={card},DEV=0");
    }
    if let Some(rest) = device.strip_prefix("plughw:") {
        return format!("hw:{rest}");
    }
    // Last resort: keep as-is (may fail; the writer logs the open error).
    device.to_string()
}

/// ALSA format name + rate for the device.
/// DsdU8 arrives with the DSD *bit* rate (e.g. 2_822_400); ALSA DSD_U32 uses rate/32.
fn alsa_format_and_rate(format: AudioFormat, sample_rate: u32) -> Option<(&'static str, u32)> {
    match format {
        AudioFormat::PcmS16le => Some(("S16_LE", sample_rate)),
        AudioFormat::PcmS24le => Some(("S32_LE", sample_rate)),
        AudioFormat::PcmS24le4 => Some(("S24_LE", sample_rate)),
        AudioFormat::PcmS32le => Some(("S32_LE", sample_rate)),
        AudioFormat::PcmF32le => Some(("FLOAT_LE", sample_rate)),
        // Native DSD: USB DACs (XMOS etc.) expose SPECIAL DSD_U32_BE.
        AudioFormat::DsdU8 => Some(("DSD_U32_BE", sample_rate / 32)),
        AudioFormat::DsdU16le => Some(("DSD_U32_BE", sample_rate / 16)),
        AudioFormat::DsdU32le => Some(("DSD_U32_BE", sample_rate)),
        _ => None,
    }
}

#[cfg(test)]
fn format_to_alsa(format: AudioFormat) -> Option<&'static str> {
    alsa_format_and_rate(format, 44100).map(|(f, _)| f)
}

/// Pack byte-interleaved DSD_U8 (LSB-first, DSF order) into ALSA DSD_U32_BE.
///
/// Input layout: `L0 R0 L1 R1 L2 R2 L3 R3 ...` (one byte = 8 DSD bits).
/// Output layout: 4-byte big-endian words per channel, bits MSB-first in time
/// (each input byte is bit-reversed; XMOS `bitrev=0` expects app-side order).
fn pack_dsd_u8_to_u32_be(data: &[u8], channels: u8) -> Vec<u8> {
    let ch = channels.max(1) as usize;
    let frame = 4 * ch; // 4 bytes/channel → one DSD_U32 frame
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(frame) {
        for c in 0..ch {
            let b0 = chunk[c].reverse_bits();
            let b1 = chunk[c + ch].reverse_bits();
            let b2 = chunk[c + 2 * ch].reverse_bits();
            let b3 = chunk[c + 3 * ch].reverse_bits();
            out.extend_from_slice(&[b0, b1, b2, b3]);
        }
    }
    out
}

/// Byte-swap DSD_U32LE words into DSD_U32_BE.
fn dsd_u32le_to_be(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<4>().0 {
        out.extend_from_slice(&[chunk[3], chunk[2], chunk[1], chunk[0]]);
    }
    out
}

fn bytes_per_frame(format: AudioFormat, channels: u8) -> usize {
    let bps = match format {
        AudioFormat::PcmS16le => 2,
        AudioFormat::PcmS24le => 3,
        AudioFormat::PcmS24le4 | AudioFormat::PcmS32le | AudioFormat::PcmF32le => 4,
        AudioFormat::DsdU8 => 1,
        AudioFormat::DsdU16le => 2,
        AudioFormat::DsdU32le => 4,
        _ => 2,
    };
    bps * channels.max(1) as usize
}

fn apply_volume(format: AudioFormat, data: &[u8], vol: f32) -> Vec<u8> {
    match format {
        AudioFormat::PcmS16le => {
            let mut out = data.to_vec();
            for chunk in out.as_chunks_mut::<2>().0 {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                let scaled = ((s as f32) * vol) as i16;
                chunk.copy_from_slice(&scaled.to_le_bytes());
            }
            out
        }
        AudioFormat::PcmF32le => {
            let mut out = data.to_vec();
            for chunk in out.as_chunks_mut::<4>().0 {
                let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                chunk.copy_from_slice(&(s * vol).to_le_bytes());
            }
            out
        }
        AudioFormat::PcmS32le | AudioFormat::PcmS24le4 => {
            let mut out = data.to_vec();
            for chunk in out.as_chunks_mut::<4>().0 {
                let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let scaled = ((s as f64) * vol as f64) as i32;
                chunk.copy_from_slice(&scaled.to_le_bytes());
            }
            out
        }
        AudioFormat::PcmS24le => {
            let mut out = data.to_vec();
            for chunk in out.as_chunks_mut::<3>().0 {
                let sign = if chunk[2] & 0x80 != 0 { 0xFFu8 } else { 0 };
                let val = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], sign]);
                let scaled = ((val as f64) * vol as f64) as i32;
                let bytes = scaled.to_le_bytes();
                chunk[0] = bytes[0];
                chunk[1] = bytes[1];
                chunk[2] = bytes[2];
            }
            out
        }
        _ => data.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::thread::sleep;

    /// What the fake device does on its next write.
    #[derive(Clone, Copy, Debug)]
    enum Next {
        Xrun,
        Fatal,
    }

    #[derive(Default)]
    struct FakeState {
        written: Vec<u8>,
        /// Byte offset in `written` at each successful open.
        open_marks: Vec<usize>,
        open_attempts: usize,
        fail_opens: usize, // usize::MAX: always fail
        busy_opens: usize, // opens that find the device busy, usize::MAX: always
        closes: usize,
        drains: usize,
        discards: usize,
        recovers: usize,
        script: VecDeque<(usize, Next)>, // (write index, action)
        writes: usize,
        params: Option<DeviceParams>,
    }

    /// A stand-in for the ALSA PCM: records frames, plays scripted errors,
    /// counts opens and closes (Drop).
    struct FakeDevice {
        st: Arc<Mutex<FakeState>>,
        frame: usize,
    }

    impl PcmDevice for FakeDevice {
        fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
            let mut st = self.st.lock().unwrap();
            let idx = st.writes;
            st.writes += 1;
            if st.script.front().is_some_and(|(i, _)| *i == idx) {
                let (_, next) = st.script.pop_front().unwrap();
                return Err(match next {
                    Next::Xrun => DeviceError::Xrun,
                    Next::Fatal => DeviceError::Fatal("No such device".into()),
                });
            }
            st.written.extend_from_slice(data);
            Ok(data.len() / self.frame)
        }
        fn recover(&mut self, _err: &DeviceError) -> Result<(), DeviceError> {
            self.st.lock().unwrap().recovers += 1;
            Ok(())
        }
        fn drain(&mut self) {
            self.st.lock().unwrap().drains += 1;
        }
        fn discard(&mut self) {
            self.st.lock().unwrap().discards += 1;
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.st.lock().unwrap().closes += 1;
        }
    }

    fn fake() -> (Arc<Mutex<FakeState>>, DeviceOpener) {
        let st = Arc::new(Mutex::new(FakeState::default()));
        let st2 = st.clone();
        let opener: DeviceOpener = Arc::new(move |p: &DeviceParams| {
            let mut s = st2.lock().unwrap();
            s.open_attempts += 1;
            s.params = Some(p.clone());
            if s.busy_opens > 0 {
                if s.busy_opens != usize::MAX {
                    s.busy_opens -= 1;
                }
                return Err(OpenError::Busy("Device or resource busy".into()));
            }
            if s.fail_opens > 0 {
                s.fail_opens = s.fail_opens.saturating_sub(if s.fail_opens == usize::MAX { 0 } else { 1 });
                return Err(OpenError::Failed("No such device".into()));
            }
            let mark = s.written.len();
            s.open_marks.push(mark);
            Ok(Box::new(FakeDevice {
                st: st2.clone(),
                frame: p.frame_bytes(),
            }) as Box<dyn PcmDevice>)
        });
        (st, opener)
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            sleep(Duration::from_millis(5));
        }
    }

    /// Expected device bytes for a FLAC fixture: one-shot decode, then the
    /// same f32 → S32_LE conversion.
    #[cfg(feature = "flac")]
    fn flac_fixture(frames: usize) -> (Vec<u8>, Vec<u8>) {
        let (flac, _, _) = crate::flac_decoder::tests::encode_flac(frames, 256);
        let pcm = crate::flac_decoder::decode_flac_to_f32(&flac).unwrap();
        (flac, flac_samples_to_device(&pcm, 2, 1.0))
    }

    /// #45 / #44: a FLAC stream is decoded in-process and reaches the
    /// device bit-exact; `stop()` plays out what is queued and releases the
    /// device before returning. No subprocess is involved.
    #[cfg(feature = "flac")]
    #[test]
    fn flac_is_decoded_in_process_and_stop_releases_the_device() {
        let (st, opener) = fake();
        let (flac, expected) = flac_fixture(50);
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure_with_device(AudioFormat::Flac, 44_100, 2, Some("hw:1,0"))
            .unwrap();
        out.play();
        let mut frames = 0;
        for chunk in flac.chunks(1000) {
            frames += out.write_audio(chunk);
        }
        assert_eq!(frames, 50 * 256);
        out.stop();
        assert!(!out.device_open(), "device still open after stop()");
        let st = st.lock().unwrap();
        let p = st.params.clone().unwrap();
        assert_eq!((p.device.as_str(), p.format, p.rate, p.channels), ("hw:1,0", "S32_LE", 44_100, 2));
        assert_eq!(st.written.len(), expected.len(), "every decoded frame reaches the device");
        assert!(st.written == expected, "decoded PCM must be bit-exact");
        assert_eq!(st.open_marks.len(), 1);
        assert_eq!(st.closes, 1, "stop() must close the device");
        assert_eq!(st.drains, 1, "stop() plays out the queue (end of track not cut)");
    }

    /// #45: the device is lost mid-stream (DAC unplugged). It is reopened
    /// with backoff and FLAC decoding goes on: no stream header is needed
    /// again, and the end of the stream reaches the device.
    #[cfg(feature = "flac")]
    #[test]
    fn flac_playback_resumes_after_device_loss_mid_stream() {
        let (st, opener) = fake();
        st.lock().unwrap().script.push_back((20, Next::Fatal));
        let (flac, expected) = flac_fixture(400);
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::Flac, 44_100, 2).unwrap();
        out.play();
        for chunk in flac.chunks(1000) {
            out.write_audio(chunk);
            sleep(Duration::from_millis(2));
        }
        out.stop();
        let st = st.lock().unwrap();
        assert_eq!(st.open_marks.len(), 2, "one reopen after the loss");
        let after = &st.written[st.open_marks[1]..];
        assert!(!after.is_empty(), "audio must resume after the reopen");
        let tail = &expected[expected.len() - 4096..];
        assert!(st.written.ends_with(tail), "decoding went on to the end of the stream");
        assert_eq!(out.underrun_count(), 0);
    }

    /// An underrun is recovered in place with `prepare`: the device is not
    /// reopened, no frame is lost, and it is counted once.
    #[test]
    fn xrun_is_recovered_without_losing_frames() {
        let (st, opener) = fake();
        st.lock().unwrap().script.push_back((3, Next::Xrun));
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        let mut sent = Vec::new();
        for i in 0..10u8 {
            let chunk = vec![i; 480 * 4];
            sent.extend_from_slice(&chunk);
            assert_eq!(out.write_audio(&chunk), 480);
        }
        out.stop();
        let st = st.lock().unwrap();
        assert!(st.written == sent, "S16 passes bit-exact, nothing lost on xrun");
        assert_eq!(st.recovers, 1);
        assert_eq!(st.open_marks.len(), 1, "an xrun does not reopen the device");
        assert_eq!(out.underrun_count(), 1);
    }

    /// The xrun caused by a pause is not a playback incident (#12).
    #[test]
    fn xrun_after_pause_is_not_counted() {
        let (st, opener) = fake();
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        let chunk = vec![0u8; 480 * 4];
        out.write_audio(&chunk);
        wait_until("first write", || st.lock().unwrap().writes == 1);
        out.pause();
        st.lock().unwrap().script.push_back((1, Next::Xrun));
        out.play();
        out.write_audio(&chunk);
        out.stop();
        assert_eq!(st.lock().unwrap().recovers, 1);
        assert_eq!(out.underrun_count(), 0);
    }

    /// #44: a stream that ends without `stop` does not keep the DAC open:
    /// after the idle delay the device is drained and closed, and the next
    /// audio reopens it.
    #[test]
    fn idle_device_is_closed_and_reopened_on_next_audio() {
        let (st, opener) = fake();
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.idle_close = Duration::from_millis(100);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        let chunk = vec![0u8; 480 * 4];
        out.write_audio(&chunk);
        wait_until("idle close", || st.lock().unwrap().closes == 1);
        assert!(!out.device_open());
        assert_eq!(st.lock().unwrap().drains, 1);
        out.write_audio(&chunk);
        wait_until("reopen", || st.lock().unwrap().open_marks.len() == 2);
        out.stop();
        assert_eq!(st.lock().unwrap().closes, 2);
        assert_eq!(out.underrun_count(), 0);
    }

    /// #33: after an intentional stop nothing reopens the device, even if
    /// `play()` is called again.
    #[test]
    fn no_reopen_after_intentional_stop() {
        let (st, opener) = fake();
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        out.stop();
        out.play();
        assert_eq!(out.write_audio(&[0u8; 1920]), 0);
        sleep(Duration::from_millis(300));
        let st = st.lock().unwrap();
        assert_eq!(st.open_attempts, 1);
        assert_eq!(st.closes, 1);
    }

    /// #33: a device that keeps failing (DAC gone) is retried with capped
    /// backoff (250 ms, 500 ms, 1 s, 2 s…), not once per write, and writes
    /// never block meanwhile.
    #[test]
    fn persistent_open_failure_is_retried_with_backoff() {
        let (st, opener) = fake();
        st.lock().unwrap().fail_opens = usize::MAX;
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        let chunk = vec![0u8; 480 * 4];
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            out.write_audio(&chunk);
            sleep(Duration::from_millis(10));
        }
        let n = st.lock().unwrap().open_attempts;
        assert!((3..=6).contains(&n), "bounded retries, got {n} attempts in ~3 s");
        // The device comes back: playback resumes by itself.
        st.lock().unwrap().fail_opens = 0;
        let end = Instant::now() + Duration::from_secs(5);
        while st.lock().unwrap().open_marks.is_empty() && Instant::now() < end {
            out.write_audio(&chunk);
            sleep(Duration::from_millis(10));
        }
        out.write_audio(&chunk);
        out.stop();
        let st = st.lock().unwrap();
        assert_eq!(st.open_marks.len(), 1, "device reopened once it is back");
        assert!(!st.written.is_empty());
    }

    /// A device another player still holds is retried every `BUSY_RETRY`,
    /// without waiting for audio, and opened as soon as it is free: the
    /// endpoint waits for it before accepting the format.
    #[test]
    fn busy_device_is_opened_once_free_without_audio() {
        let (st, opener) = fake();
        st.lock().unwrap().busy_opens = 5;
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        assert!(!out.device_open());
        let t0 = Instant::now();
        wait_until("open once free", || out.device_open());
        let waited = t0.elapsed();
        assert!(
            waited < Duration::from_secs(2),
            "opened {waited:?} after the device came free"
        );
        assert_eq!(st.lock().unwrap().open_attempts, 6);
        out.stop();
    }

    /// Without audio, a busy device is only retried for `BUSY_WAIT`: an
    /// endpoint nobody streams to must not take the DAC whenever its owner
    /// lets go.
    #[test]
    fn busy_device_is_not_retried_forever_without_audio() {
        let (st, opener) = fake();
        st.lock().unwrap().busy_opens = usize::MAX;
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        sleep(BUSY_WAIT + Duration::from_millis(300));
        let n = st.lock().unwrap().open_attempts;
        assert!(n >= 40, "retried while waiting, got {n} attempts");
        sleep(Duration::from_millis(500));
        assert_eq!(
            st.lock().unwrap().open_attempts,
            n,
            "no retries after BUSY_WAIT without audio"
        );
        // audio comes in: retried again
        st.lock().unwrap().busy_opens = 0;
        out.play();
        wait_until("open with audio", || {
            out.write_audio(&[0u8; 1920]);
            out.device_open()
        });
        out.stop();
    }

    /// `flush()` (seek) discards what is queued instead of playing it out.
    #[test]
    fn flush_discards_and_reopens() {
        let (st, opener) = fake();
        let mut out = AlsaDirectOutput::with_opener(opener);
        out.configure(AudioFormat::PcmS16le, 48_000, 2).unwrap();
        out.play();
        out.flush();
        wait_until("reopen after flush", || st.lock().unwrap().open_marks.len() == 2);
        out.stop();
        let st = st.lock().unwrap();
        assert_eq!(st.discards, 1);
        assert_eq!(st.closes, 2);
    }

    // ------------------------------------------------------------------
    // Le chemin audio touche par le passage de `chunks_exact` a `as_chunks`
    // (lint `chunks_exact_to_as_chunks`, clippy 1.98). Les deux constructions
    // ecartent le reste de division de la meme facon — mais ni `apply_volume`
    // ni `dsd_u32le_to_be` n'avaient le moindre test, et un octet decale sur
    // ce chemin s'entend. Ces tests fixent le comportement AVANT/APRES.
    // ------------------------------------------------------------------

    /// Le volume s'applique echantillon par echantillon, en 16 bits.
    #[test]
    fn le_volume_divise_un_echantillon_s16() {
        // 1000 (0x03E8) en petit-boutiste, a moitie volume -> 500 (0x01F4).
        let out = apply_volume(AudioFormat::PcmS16le, &[0xE8, 0x03], 0.5);
        assert_eq!(out, vec![0xF4, 0x01]);
    }

    /// Idem en 32 bits.
    #[test]
    fn le_volume_divise_un_echantillon_s32() {
        let out = apply_volume(AudioFormat::PcmS32le, &[0xE8, 0x03, 0x00, 0x00], 0.5);
        assert_eq!(out, vec![0xF4, 0x01, 0x00, 0x00]);
    }

    /// Le S24 empaquete tient sur TROIS octets : le signe est reconstitue a la
    /// main avant la multiplication, sinon un echantillon negatif devient un
    /// enorme positif — et ca ne s'entend pas qu'un peu.
    #[test]
    fn le_volume_conserve_le_signe_en_s24_empaquete() {
        // +1000 -> +500
        let positif = apply_volume(AudioFormat::PcmS24le, &[0xE8, 0x03, 0x00], 0.5);
        assert_eq!(positif, vec![0xF4, 0x01, 0x00]);
        // -1000 (0xFFFFFC18) -> -500 (0xFFFFFE0C)
        let negatif = apply_volume(AudioFormat::PcmS24le, &[0x18, 0xFC, 0xFF], 0.5);
        assert_eq!(negatif, vec![0x0C, 0xFE, 0xFF]);
    }

    /// LA propriete que le changement de construction aurait pu casser : un
    /// reste de division n'est PAS traite, et il est rendu tel quel.
    #[test]
    fn le_volume_laisse_un_reste_partiel_intact() {
        // Trois octets en S16 : un echantillon complet, puis un octet orphelin.
        let out = apply_volume(AudioFormat::PcmS16le, &[0xE8, 0x03, 0x77], 0.5);
        assert_eq!(
            out,
            vec![0xF4, 0x01, 0x77],
            "l'octet orphelin passe tel quel"
        );
    }

    /// Le boutisme des mots DSD_U32 est inverse, mot par mot.
    #[test]
    fn les_mots_dsd_u32_sont_inverses_un_a_un() {
        let out = dsd_u32le_to_be(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out, vec![4, 3, 2, 1, 8, 7, 6, 5]);
    }

    /// Meme propriete de reste, cote DSD : un mot incomplet est ECARTE, pas
    /// rendu a moitie inverse.
    #[test]
    fn un_mot_dsd_incomplet_est_ecarte() {
        let out = dsd_u32le_to_be(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            out,
            vec![4, 3, 2, 1],
            "les deux octets en trop disparaissent"
        );
    }

    #[test]
    fn s24_maps_to_s32le_for_hardware_compat() {
        // Packed S24 is expanded to S32_LE before writing, so the device must be
        // opened as S32_LE (accepted by S24_LE-incapable DACs like the ES9038Q2M).
        assert_eq!(format_to_alsa(AudioFormat::PcmS24le), Some("S32_LE"));
        assert_eq!(format_to_alsa(AudioFormat::PcmS32le), Some("S32_LE"));
        assert_eq!(format_to_alsa(AudioFormat::PcmS16le), Some("S16_LE"));
    }

    #[test]
    fn pad_s24_is_left_justified_full_scale() {
        // Positive sample 0x7F1234 (LE bytes 34 12 7F) -> S32 0x7F123400.
        let out = pad_s24_to_s32(&[0x34, 0x12, 0x7F]);
        assert_eq!(out, vec![0x00, 0x34, 0x12, 0x7F]);
        assert_eq!(
            i32::from_le_bytes([out[0], out[1], out[2], out[3]]),
            0x7F123400
        );

        // Most-negative sample 0x800000 -> i32::MIN (sign preserved, full scale).
        let out = pad_s24_to_s32(&[0x00, 0x00, 0x80]);
        assert_eq!(
            i32::from_le_bytes([out[0], out[1], out[2], out[3]]),
            i32::MIN
        );

        // Zero stays zero; length grows 3 -> 4 bytes per sample.
        let out = pad_s24_to_s32(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(out, vec![0u8; 8]);
    }

    #[test]
    fn s24_value_is_source_shifted_left_8() {
        // The S32 output equals the sign-extended 24-bit source value << 8:
        // guarantees bit-perfect magnitude at full scale.
        for &(b, expect_v24) in &[
            ([0x01u8, 0x00, 0x00], 1i32),
            ([0xFF, 0xFF, 0xFF], -1i32),
            ([0x00, 0x00, 0x40], 0x400000i32),
        ] {
            let out = pad_s24_to_s32(&b);
            let got = i32::from_le_bytes([out[0], out[1], out[2], out[3]]);
            assert_eq!(got, expect_v24 << 8, "src {b:?}");
        }
    }

    #[test]
    fn dsd_u8_maps_to_u32_be_at_rate_div_32() {
        assert_eq!(
            alsa_format_and_rate(AudioFormat::DsdU8, 2_822_400),
            Some(("DSD_U32_BE", 88_200))
        );
        assert_eq!(
            alsa_format_and_rate(AudioFormat::DsdU8, 5_644_800),
            Some(("DSD_U32_BE", 176_400))
        );
    }

    #[test]
    fn sysdefault_maps_to_hw_for_dsd() {
        assert_eq!(
            dsd_hw_device("sysdefault:CARD=AUDIO"),
            "hw:CARD=AUDIO,DEV=0"
        );
        assert_eq!(dsd_hw_device("hw:3,0"), "hw:3,0");
    }

    #[test]
    fn le_volume_f32_multiplie_en_float() {
        let sample: f32 = 0.8;
        let input = sample.to_le_bytes();
        let out = apply_volume(AudioFormat::PcmF32le, &input, 0.5);
        let got = f32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert!(
            (got - 0.4).abs() < 1e-6,
            "F32 volume: expected ~0.4, got {got}"
        );
    }

    #[test]
    fn le_volume_f32_negatif() {
        let sample: f32 = -0.6;
        let input = sample.to_le_bytes();
        let out = apply_volume(AudioFormat::PcmF32le, &input, 0.5);
        let got = f32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert!(
            (got - (-0.3)).abs() < 1e-6,
            "F32 negative volume: expected ~-0.3, got {got}"
        );
    }

    #[test]
    fn f32_maps_to_float_le() {
        assert_eq!(format_to_alsa(AudioFormat::PcmF32le), Some("FLOAT_LE"));
    }

    #[test]
    fn pack_dsd_u8_stereo_bitrevs_and_groups() {
        // 4 interleaved L/R byte pairs → one DSD_U32 frame per channel.
        // Input: L0 R0 L1 R1 L2 R2 L3 R3
        let input = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let out = pack_dsd_u8_to_u32_be(&input, 2);
        assert_eq!(out.len(), 8);
        // Left word: rev(L0) rev(L1) rev(L2) rev(L3)
        assert_eq!(out[0], 0x01u8.reverse_bits());
        assert_eq!(out[1], 0x03u8.reverse_bits());
        assert_eq!(out[2], 0x05u8.reverse_bits());
        assert_eq!(out[3], 0x07u8.reverse_bits());
        // Right word
        assert_eq!(out[4], 0x02u8.reverse_bits());
        assert_eq!(out[5], 0x04u8.reverse_bits());
        assert_eq!(out[6], 0x06u8.reverse_bits());
        assert_eq!(out[7], 0x08u8.reverse_bits());
    }
}
