use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use oaat_core::format::AudioFormat;
use tracing::{error, info, warn};

pub struct AlsaDirectOutput {
    process: Option<Child>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
    bytes_written: u64,
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
                    .filter(|l| l.starts_with("card "))
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
        let cards: Vec<_> = devices.iter()
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

        let device = match device_name {
            Some(d) if d.starts_with("hw:") || d.starts_with("plughw:")
                || d.starts_with("default") || d.starts_with("sysdefault:") => d,
            _ => "default",
        };

        if format == AudioFormat::Flac {
            let alsa_fmt = match sample_rate {
                _ if channels == 2 => "S24_3LE",
                _ => "S24_3LE",
            };
            let cmd = format!(
                "ffmpeg -f flac -i pipe:0 -f s24le -ar {} -ac {} pipe:1 | aplay -D {} -f {} -r {} -c {} -t raw -q",
                sample_rate, channels, device, alsa_fmt, sample_rate, channels
            );
            let child = Command::new("sh")
                .args(["-c", &cmd])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;

            info!(
                device,
                format = "FLAC→s24le→aplay",
                sample_rate,
                channels,
                "ALSA direct output started (ffmpeg FLAC pipe aplay)"
            );

            self.process = Some(child);
        } else {
            let alsa_fmt = format_to_alsa(format)
                .ok_or_else(|| format!("unsupported format for ALSA direct: {format}"))?;

            let child = Command::new("aplay")
                .args([
                    "-D", device,
                    "-f", alsa_fmt,
                    "-r", &sample_rate.to_string(),
                    "-c", &channels.to_string(),
                    "-t", "raw",
                    "-q",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;

            info!(
                device,
                format = alsa_fmt,
                sample_rate,
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

        if child.try_wait().ok().flatten().is_some() {
            warn!("aplay process exited unexpectedly");
            self.process = None;
            return 0;
        }

        let Some(ref mut stdin) = child.stdin else {
            return 0;
        };

        let vol = self.volume.load(Ordering::Relaxed) as f32 / 1000.0;

        let result = if (vol - 1.0).abs() < 0.001 {
            stdin.write_all(data)
        } else {
            let scaled = apply_volume(self.format, data, vol);
            stdin.write_all(&scaled)
        };

        match result {
            Ok(()) => {
                self.bytes_written += data.len() as u64;
                let bpf = bytes_per_frame(self.format, self.channels);
                if bpf > 0 { data.len() / bpf } else { 0 }
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
}

impl Default for AlsaDirectOutput {
    fn default() -> Self {
        Self::new()
    }
}

fn format_to_alsa(format: AudioFormat) -> Option<&'static str> {
    match format {
        AudioFormat::PcmS16le => Some("S16_LE"),
        AudioFormat::PcmS24le => Some("S24_3LE"),
        AudioFormat::PcmS24le4 => Some("S24_LE"),
        AudioFormat::PcmS32le => Some("S32_LE"),
        AudioFormat::PcmF32le => Some("FLOAT_LE"),
        _ => None,
    }
}

fn bytes_per_frame(format: AudioFormat, channels: u8) -> usize {
    let bps = match format {
        AudioFormat::PcmS16le => 2,
        AudioFormat::PcmS24le => 3,
        AudioFormat::PcmS24le4 | AudioFormat::PcmS32le | AudioFormat::PcmF32le => 4,
        _ => 2,
    };
    bps * channels.max(1) as usize
}

fn apply_volume(format: AudioFormat, data: &[u8], vol: f32) -> Vec<u8> {
    match format {
        AudioFormat::PcmS16le => {
            let mut out = data.to_vec();
            for chunk in out.chunks_exact_mut(2) {
                let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                let scaled = ((s as f32) * vol) as i16;
                chunk.copy_from_slice(&scaled.to_le_bytes());
            }
            out
        }
        AudioFormat::PcmS32le | AudioFormat::PcmS24le4 | AudioFormat::PcmF32le => {
            let mut out = data.to_vec();
            for chunk in out.chunks_exact_mut(4) {
                let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let scaled = ((s as f64) * vol as f64) as i32;
                chunk.copy_from_slice(&scaled.to_le_bytes());
            }
            out
        }
        AudioFormat::PcmS24le => {
            let mut out = data.to_vec();
            for chunk in out.chunks_exact_mut(3) {
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
