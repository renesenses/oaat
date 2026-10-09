use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use oaat_core::format::AudioFormat;
use tracing::{error, info, warn};

/// aplay ALSA ring-buffer size, in microseconds. 0.5s was too small to absorb
/// network jitter + clock drift on a continuous stream, so the device XRUN'd
/// repeatedly — audible as frequent micro-dropouts on BOTH live radio (FIP) and
/// steady FLAC (Qobuz), independent of source. 2s gives aplay enough slack to
/// ride out jitter without a dropout; the stall watchdog still recovers a real
/// hang. ALSA clamps to the hardware maximum, which some drivers cap in BYTES:
/// the request that yields 2s at S16/48k grants only 1s at S32/44.1k (#7).
const APLAY_BUFFER_TIME_US: &str = "2000000";

/// Requested ALSA period. aplay's default is buffer/4 (500ms here); with a
/// byte-capped driver the buffer is granted as a whole number of periods, so
/// coarse periods forfeit up to one period of capacity (observed on the
/// I-Sabre ES9038Q2M: 2 × 500ms granted where the cap allowed ~1.1s, #7).
/// Finer periods claim the cap almost exactly.
const APLAY_PERIOD_TIME_US: &str = "125000";

/// Target capacity of the stdin pipe feeding aplay (Linux F_SETPIPE_SZ).
/// The kernel default (64 KiB ≈ 0.18s at S32/44.1k stereo) barely decouples
/// us from the device buffer; 1 MiB ≈ 3s of userspace slack absorbs sender
/// bursts with clean blocking backpressure instead of dropping UDP audio (#7).
#[cfg(target_os = "linux")]
const APLAY_STDIN_PIPE_BYTES: libc::c_int = 1 << 20;

pub struct AlsaDirectOutput {
    process: Option<Child>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
    bytes_written: u64,
    device_name: Option<String>,
    underruns: Arc<AtomicU64>,
}

impl AlsaDirectOutput {
    pub fn list_devices() -> Vec<String> {
        Command::new("aplay")
            .args(["-l"])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| l.starts_with("card ") || l.starts_with("carte "))
                    .map(|l| l.to_string())
                    .collect()
            })
            .unwrap_or_default()
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
        Self {
            process: None,
            playing: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(AtomicU32::new(1000)),
            muted: Arc::new(AtomicBool::new(false)),
            sample_rate: 0,
            channels: 0,
            format: AudioFormat::PcmS16le,
            bytes_written: 0,
            device_name: None,
            underruns: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Total ALSA under/overruns reported by aplay since this output was
    /// created (cumulative across respawns; PCM path only — the FLAC path
    /// inherits stderr straight to the service log).
    pub fn underrun_count(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
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

        if format == AudioFormat::Flac {
            // Stream FLAC through ffmpeg → raw PCM → aplay.
            // Each write_audio() call writes FLAC data to ffmpeg's stdin;
            // ffmpeg handles the streaming decode (needs FLAC headers only once).
            let bits = if channels <= 2 { "s32le" } else { "s16le" };
            let alsa_fmt = if bits == "s32le" { "S32_LE" } else { "S16_LE" };
            let cmd = format!(
                "ffmpeg -hide_banner -loglevel warning -err_detect ignore_err -f flac -i /dev/stdin -f {bits} -ar {sample_rate} -ac {channels} - | aplay -D {device} -f {alsa_fmt} -r {sample_rate} -c {channels} -t raw -v --buffer-time {APLAY_BUFFER_TIME_US} --period-time {APLAY_PERIOD_TIME_US}"
            );
            let child = Command::new("sh")
                .args(["-c", &cmd])
                .env("LC_ALL", "C")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?;
            if let Some(stdin) = child.stdin.as_ref() {
                enlarge_pipe(stdin);
            }

            info!(
                device,
                format = %format!("FLAC→ffmpeg→{bits}→aplay"),
                sample_rate,
                channels,
                "ALSA direct output started (ffmpeg FLAC pipe)"
            );

            self.process = Some(child);
        } else {
            let (alsa_fmt, alsa_rate) = alsa_format_and_rate(format, sample_rate)
                .ok_or_else(|| format!("unsupported format for ALSA direct: {format}"))?;

            let mut child = Command::new("aplay")
                .args([
                    "-D",
                    &device,
                    "-f",
                    alsa_fmt,
                    "-r",
                    &alsa_rate.to_string(),
                    "-c",
                    &channels.to_string(),
                    "-t",
                    "raw",
                    "-v",
                    "--buffer-time",
                    APLAY_BUFFER_TIME_US,
                    "--period-time",
                    APLAY_PERIOD_TIME_US,
                ])
                .env("LC_ALL", "C")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(stdin) = child.stdin.as_ref() {
                enlarge_pipe(stdin);
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_stderr_logger(stderr, Arc::clone(&self.underruns));
            }

            info!(
                device = %device,
                format = alsa_fmt,
                sample_rate = alsa_rate,
                wire_sample_rate = sample_rate,
                channels,
                "ALSA direct output started (aplay pipe)"
            );

            self.process = Some(child);
        }

        self.bytes_written = 0;
        self.playing.store(false, Ordering::Relaxed);

        Ok(())
    }

    pub fn play(&self) {
        self.playing.store(true, Ordering::Relaxed);
        info!("audio output started (ALSA direct)");
    }

    pub fn pause(&self) {
        self.playing.store(false, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(mut child) = self.process.take() {
            drop(child.stdin.take());
            let _ = child.wait();
        }
        self.bytes_written = 0;
    }

    pub fn flush(&mut self) {
        let fmt = self.format;
        let sr = self.sample_rate;
        let ch = self.channels;
        let dev = self.device_name.clone();
        self.stop();
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

    pub fn write_audio(&mut self, data: &[u8]) -> usize {
        if !self.playing.load(Ordering::Relaxed) || self.muted.load(Ordering::Relaxed) {
            return 0;
        }

        let Some(ref mut child) = self.process else {
            return 0;
        };

        if let Some(status) = child.try_wait().ok().flatten() {
            // stderr is streamed live by spawn_stderr_logger — the cause is
            // already in the log as `aplay: …` lines.
            warn!(exit_code = %status, "audio output process exited unexpectedly");
            self.process = None;
            return 0;
        }

        let Some(ref mut stdin) = child.stdin else {
            return 0;
        };

        let vol = self.volume.load(Ordering::Relaxed) as f32 / 1000.0;

        // For FLAC: write raw FLAC data to ffmpeg pipe (ffmpeg handles decode)
        if self.format == AudioFormat::Flac {
            match stdin.write_all(data) {
                Ok(()) => {
                    self.bytes_written += data.len() as u64;
                    return data.len() / 4;
                }
                Err(e) => {
                    error!(error = %e, "ALSA FLAC pipe write failed");
                    return 0;
                }
            }
        }

        let scaled;
        let write_data;
        let to_write = if self.format == AudioFormat::DsdU8 {
            // DSD is bit-perfect: ignore software volume (DAC volume only).
            write_data = pack_dsd_u8_to_u32_be(data, self.channels);
            &write_data
        } else if self.format == AudioFormat::DsdU32le {
            write_data = dsd_u32le_to_be(data);
            &write_data
        } else if self.format == AudioFormat::PcmS24le {
            if (vol - 1.0).abs() < 0.001 {
                write_data = pad_s24_to_s32(data);
            } else {
                scaled = apply_volume(self.format, data, vol);
                write_data = pad_s24_to_s32(&scaled);
            }
            &write_data
        } else if (vol - 1.0).abs() < 0.001 {
            data
        } else {
            write_data = apply_volume(self.format, data, vol);
            &write_data
        };
        let result = stdin.write_all(to_write);

        match result {
            Ok(()) => {
                self.bytes_written += data.len() as u64;
                let bpf = bytes_per_frame(self.format, self.channels);
                data.len().checked_div(bpf).unwrap_or(0)
            }
            Err(e) => {
                error!(error = %e, "ALSA direct write failed");
                0
            }
        }
    }

    pub fn buffer_level(&self) -> usize {
        0
    }

    /// Playback position is not observable through the aplay pipe.
    /// Drift correction is unavailable on this output (see CpalOutput).
    pub fn frames_played(&self) -> Option<u64> {
        None
    }

    /// See `frames_played`: no position tracking on this output.
    pub fn content_position(&self) -> Option<u64> {
        None
    }

    /// No-op: aplay is spawned per stream, there is nothing to prewarm.
    pub fn prewarm(&mut self, _device_name: Option<&str>) {}

    /// Raw wire bytes are piped to aplay with the matching ALSA format:
    /// bit-perfect by construction (software volume aside).
    pub fn bit_perfect_path(&self) -> bool {
        true
    }

    /// No-op: the aplay pipe offers no sample-accurate insertion point.
    pub fn set_correction(&self, _frames: i64) {}
}

impl Default for AlsaDirectOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// Grow the pipe feeding the audio process so sender bursts are absorbed in
/// userspace instead of overflowing the UDP socket once the device buffer is
/// full. Best effort: a refusal (unprivileged cap lowered below 1 MiB) keeps
/// the kernel default and only costs slack.
#[cfg(target_os = "linux")]
fn enlarge_pipe(stdin: &ChildStdin) {
    use std::os::fd::AsRawFd;
    let granted = unsafe {
        libc::fcntl(
            stdin.as_raw_fd(),
            libc::F_SETPIPE_SZ,
            APLAY_STDIN_PIPE_BYTES,
        )
    };
    if granted < 0 {
        warn!(
            error = %std::io::Error::last_os_error(),
            "aplay stdin pipe resize refused — keeping kernel default"
        );
    } else {
        info!(bytes = granted, "aplay stdin pipe enlarged");
    }
}

#[cfg(not(target_os = "linux"))]
fn enlarge_pipe(_stdin: &ChildStdin) {}

/// Stream aplay's stderr into the endpoint log. Under/overruns previously
/// vanished (`-q` recovers XRUNs silently): audible dropouts, zero trace (#8).
/// `-v` also makes aplay dump the hw params actually granted, exposing a
/// driver that clamps the requested buffer (#7).
fn spawn_stderr_logger(stderr: std::process::ChildStderr, underruns: Arc<AtomicU64>) {
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if is_xrun_line(line) {
                let total = underruns.fetch_add(1, Ordering::Relaxed) + 1;
                warn!(total, "aplay: {line}");
            } else {
                info!("aplay: {line}");
            }
        }
    });
}

/// aplay (LC_ALL=C) reports buffer trouble as `underrun!!! (at least … ms
/// long)` / `overrun!!!`; snd_pcm_recover-style messages say "xrun".
fn is_xrun_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("underrun") || l.contains("overrun") || l.contains("xrun")
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
    // Last resort: keep as-is (may fail; caller logs the aplay error).
    device.to_string()
}

/// ALSA format string + rate for aplay.
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

/// Byte-swap DSD_U32LE words into DSD_U32_BE for aplay.
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
        // Packed S24 is expanded to S32_LE before writing, so aplay must be
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
    fn xrun_lines_are_detected_others_ignored() {
        assert!(is_xrun_line("underrun!!! (at least 34.202 ms long)"));
        assert!(is_xrun_line("overrun!!!"));
        assert!(is_xrun_line("Suspicious buffer position: xrun detected"));
        assert!(!is_xrun_line(
            "Playing raw data 'stdin' : Signed 32 bit Little Endian, Rate 44100 Hz, Stereo"
        ));
        assert!(!is_xrun_line("buffer_size  : 44100"));
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
