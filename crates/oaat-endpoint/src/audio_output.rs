use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use tracing::{error, info, warn};

use oaat_core::format::AudioFormat;

const RING_BUFFER_FRAMES: usize = 48000;

pub struct CpalOutput {
    stream: Option<cpal::Stream>,
    producer: Option<ringbuf::HeapProd<f32>>,
    playing: Arc<AtomicBool>,
    volume: Arc<AtomicU32>, // 0-1000 (0.0-1.0 * 1000)
    muted: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u8,
    format: AudioFormat,
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

        info!(
            device = device.name().unwrap_or_default(),
            sample_rate, channels, format = %format,
            "configuring audio output"
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

        // Check supported formats: prefer f32, fallback to i16 (most compatible with I2S DACs).
        // Many I2S DACs (ESS 9038 via hifiberry overlay) accept S32_LE in ALSA but
        // only output sound with S16_LE. Use i16 as the safe fallback.
        let supports_f32 = device
            .supported_output_configs()
            .map(|cfgs| {
                cfgs.into_iter()
                    .any(|c| c.sample_format() == cpal::SampleFormat::F32)
            })
            .unwrap_or(false);

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
                    for sample in &mut output[..read] {
                        *sample *= vol;
                    }
                    output[read..].fill(0.0);
                },
                |err| error!(error = %err, "audio output error"),
                None,
            )?
        } else {
            info!("opening audio output (i16/S16_LE)");
            device.build_output_stream(
                &config,
                move |output: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    if !playing.load(Ordering::Relaxed) || muted.load(Ordering::Relaxed) {
                        output.fill(0);
                        return;
                    }
                    let vol = volume.load(Ordering::Relaxed) as f32 / 1000.0;
                    let mut tmp = vec![0.0f32; output.len()];
                    let read = consumer.pop_slice(&mut tmp);
                    for (i, sample) in output.iter_mut().enumerate() {
                        if i < read {
                            let s = (tmp[i] * vol).clamp(-1.0, 1.0);
                            *sample = (s * i16::MAX as f32) as i16;
                        } else {
                            *sample = 0;
                        }
                    }
                },
                |err| error!(error = %err, "audio output error"),
                None,
            )?
        };

        self.stream = Some(stream);
        self.producer = Some(producer);

        Ok(())
    }

    pub fn play(&self) {
        if let Some(ref stream) = self.stream {
            stream.play().ok();
            self.playing.store(true, Ordering::Relaxed);
            info!("audio output started");
        }
    }

    pub fn pause(&self) {
        if let Some(ref stream) = self.stream {
            stream.pause().ok();
            self.playing.store(false, Ordering::Relaxed);
        }
    }

    pub fn stop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        self.stream = None;
        self.producer = None;
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

        let samples = if self.format == AudioFormat::Flac {
            #[cfg(feature = "flac")]
            {
                match crate::flac_decoder::decode_flac_to_f32(data) {
                    Ok(s) => s,
                    Err(_) => return 0,
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

        producer.push_slice(&samples) / self.channels.max(1) as usize
    }

    pub fn buffer_level(&self) -> usize {
        self.producer
            .as_ref()
            .map(|p| p.occupied_len())
            .unwrap_or(0)
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
