use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_FLAC, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::{debug, warn};

/// Decode a FLAC frame (or concatenation of frames) into interleaved f32 PCM samples.
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
}
