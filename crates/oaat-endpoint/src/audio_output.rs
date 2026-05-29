use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};
use ringbuf::{HeapRb, traits::{Consumer, Observer, Producer, Split}};
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
        self.stop();
        self.format = format;
        self.sample_rate = sample_rate;
        self.channels = channels;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device found")?;

        info!(
            device = device.name().unwrap_or_default(),
            sample_rate, channels, format = %format,
            "configuring audio output"
        );

        let ring_size = RING_BUFFER_FRAMES * channels as usize;
        let rb = HeapRb::<f32>::new(ring_size);
        let (producer, mut consumer) = rb.split();

        let config = StreamConfig {
            channels: channels as u16,
            sample_rate: SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let playing = self.playing.clone();
        let volume = self.volume.clone();
        let muted = self.muted.clone();

        let stream = device.build_output_stream(
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
            move |err| {
                error!(error = %err, "audio output error");
            },
            None,
        )?;

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
        _ => {
            warn!(format = %format, "unsupported format, silence");
            vec![0.0f32; data.len() / 2]
        }
    }
}
