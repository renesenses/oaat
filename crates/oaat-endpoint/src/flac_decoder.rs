use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_FLAC, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::{debug, warn};

/// Decode a complete FLAC buffer (header + frames) into interleaved f32 PCM.
pub fn decode_flac_to_f32(flac_data: &[u8]) -> Result<Vec<f32>, String> {
    let cursor = Cursor::new(flac_data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("flac");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("FLAC probe failed: {e}"))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec == CODEC_TYPE_FLAC)
        .ok_or("no FLAC track found")?;

    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("FLAC decoder init failed: {e}"))?;

    let mut all_samples = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                debug!(error = %e, "FLAC packet read ended");
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let duration = decoded.capacity();
                let mut sample_buf = SampleBuffer::<f32>::new(duration as u64, spec);
                sample_buf.copy_interleaved_ref(decoded);
                all_samples.extend_from_slice(sample_buf.samples());
            }
            Err(e) => {
                warn!(error = %e, "FLAC decode error, skipping packet");
            }
        }
    }

    Ok(all_samples)
}

/// Stateful FLAC stream decoder — accumulates data across packets and decodes
/// as complete FLAC frames become available.
pub struct FlacStreamDecoder {
    buf: Vec<u8>,
    initialized: bool,
    format: Option<Box<dyn FormatReader>>,
    decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>,
    track_id: u32,
}

impl FlacStreamDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(64 * 1024),
            initialized: false,
            format: None,
            decoder: None,
            track_id: 0,
        }
    }

    /// Feed raw FLAC data and get decoded f32 PCM samples back.
    /// The first call must include the fLaC header + STREAMINFO.
    pub fn feed(&mut self, data: &[u8]) -> Vec<f32> {
        self.buf.extend_from_slice(data);

        if !self.initialized {
            if self.buf.len() < 42 || &self.buf[..4] != b"fLaC" {
                return Vec::new();
            }
            match self.try_init() {
                Ok(()) => self.initialized = true,
                Err(_) => return Vec::new(),
            }
        }

        self.drain_frames()
    }

    /// Reset for a new stream.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.initialized = false;
        self.format = None;
        self.decoder = None;
        self.track_id = 0;
    }

    fn try_init(&mut self) -> Result<(), String> {
        let cursor = Cursor::new(self.buf.clone());
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

        let mut hint = Hint::new();
        hint.with_extension("flac");

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("FLAC init failed: {e}"))?;

        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec == CODEC_TYPE_FLAC)
            .ok_or("no FLAC track")?;

        let track_id = track.id;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("FLAC decoder init: {e}"))?;

        self.track_id = track_id;
        self.format = Some(format);
        self.decoder = Some(decoder);
        Ok(())
    }

    fn drain_frames(&mut self) -> Vec<f32> {
        let Some(format) = self.format.as_mut() else {
            return Vec::new();
        };
        let Some(decoder) = self.decoder.as_mut() else {
            return Vec::new();
        };

        let mut samples = Vec::new();
        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != self.track_id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let dur = decoded.capacity();
                    let mut sb = SampleBuffer::<f32>::new(dur as u64, spec);
                    sb.copy_interleaved_ref(decoded);
                    samples.extend_from_slice(sb.samples());
                }
                Err(e) => {
                    debug!(error = %e, "FLAC stream decode skip");
                }
            }
        }
        samples
    }
}

impl Default for FlacStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_returns_error() {
        let result = decode_flac_to_f32(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn garbage_data_returns_error() {
        let result = decode_flac_to_f32(&[0xFF; 100]);
        assert!(result.is_err());
    }

    #[test]
    fn stream_decoder_needs_header() {
        let mut dec = FlacStreamDecoder::new();
        let result = dec.feed(&[0xFF; 100]);
        assert!(result.is_empty());
        assert!(!dec.initialized);
    }
}
