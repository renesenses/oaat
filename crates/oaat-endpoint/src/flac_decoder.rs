use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_FLAC, CodecParameters, DecoderOptions};
use symphonia::core::formats::{FormatOptions, Packet};
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
///
/// `buf` only ever holds *unread* input: the stream header until it is
/// complete, then at most one partial frame (plus the chunk being fed).
/// Complete frames are cut out of it and handed to the codec one by one, so
/// memory stays bounded over an arbitrarily long stream and frames fed after
/// initialization are decoded too (#32).
pub struct FlacStreamDecoder {
    buf: Vec<u8>,
    initialized: bool,
    decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>,
    track_id: u32,
}

/// No legal FLAC frame comes near this size: a candidate frame still open
/// after this many bytes is corrupt, not slow — resync past it.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

impl FlacStreamDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(64 * 1024),
            initialized: false,
            decoder: None,
            track_id: 0,
        }
    }

    /// Feed raw FLAC data and get decoded f32 PCM samples back.
    /// The first call must include the fLaC header + STREAMINFO.
    pub fn feed(&mut self, data: &[u8]) -> Vec<f32> {
        self.buf.extend_from_slice(data);

        if !self.initialized {
            if self.buf.len() < 4 {
                return Vec::new();
            }
            if &self.buf[..4] != b"fLaC" {
                // Joined mid-stream: nothing is decodable without STREAMINFO,
                // and keeping the bytes would only grow the buffer.
                self.buf.clear();
                return Vec::new();
            }
            let Some(header_len) = stream_header_len(&self.buf) else {
                return Vec::new(); // metadata not complete yet
            };
            match self.try_init(header_len) {
                Ok(()) => {
                    self.buf.drain(..header_len);
                    self.initialized = true;
                }
                Err(e) => {
                    warn!(error = %e, "FLAC stream header rejected");
                    self.buf.clear();
                    return Vec::new();
                }
            }
        }

        self.drain_frames()
    }

    /// Reset for a new stream.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.initialized = false;
        self.decoder = None;
        self.track_id = 0;
    }

    /// Build the codec from the STREAMINFO block of the stream header. The
    /// symphonia demuxer is not used here: its init insists on reading up to
    /// the first audio frame, and it owns its input, which is what made the
    /// old decoder blind to anything fed after initialization (#32).
    fn try_init(&mut self, header_len: usize) -> Result<(), String> {
        let info = stream_info(&self.buf[..header_len]).ok_or("no STREAMINFO block")?;
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_FLAC)
            .with_extra_data(info.to_vec().into_boxed_slice());
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| format!("FLAC decoder init: {e}"))?;
        self.track_id = 0;
        self.decoder = Some(decoder);
        Ok(())
    }

    /// Decode every complete frame in `buf`, then drop the consumed bytes,
    /// keeping only a trailing partial frame (or partial sync) for next time.
    fn drain_frames(&mut self) -> Vec<f32> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Vec::new();
        };

        let buf = &self.buf;
        let mut samples = Vec::new();
        let mut pos = 0;
        while pos < buf.len() {
            let Some(start) =
                (pos..buf.len()).find(|&i| !matches!(frame_header(&buf[i..]), Header::Invalid))
            else {
                pos = buf.len();
                break;
            };
            pos = start;
            let Header::Valid(header_len) = frame_header(&buf[start..]) else {
                break; // header itself still incomplete
            };
            match frame_end(buf, start, header_len) {
                Some(end) => {
                    let packet = Packet::new_from_slice(self.track_id, 0, 0, &buf[start..end]);
                    match decoder.decode(&packet) {
                        Ok(decoded) => {
                            let spec = *decoded.spec();
                            let dur = decoded.capacity();
                            let mut sb = SampleBuffer::<f32>::new(dur as u64, spec);
                            sb.copy_interleaved_ref(decoded);
                            samples.extend_from_slice(sb.samples());
                        }
                        // A CRC-16 match at the very end of the buffer can be
                        // a split frame that matched by chance (1 in 65536):
                        // a truncated frame does not decode, so wait for more.
                        Err(_) if end == buf.len() => break,
                        Err(e) => debug!(error = %e, "FLAC stream decode skip"),
                    }
                    pos = end;
                }
                None if buf.len() - start > MAX_FRAME_BYTES => {
                    debug!("FLAC: no frame end within bound, resyncing");
                    pos = start + 1;
                }
                None => break, // frame not complete yet
            }
        }
        self.buf.drain(..pos);
        samples
    }
}

/// Length of the stream header (`fLaC` + all metadata blocks) once the last
/// metadata block is fully buffered; `None` while incomplete.
fn stream_header_len(buf: &[u8]) -> Option<usize> {
    let mut pos = 4;
    loop {
        let block = buf.get(pos..pos + 4)?;
        let len = u32::from_be_bytes([0, block[1], block[2], block[3]]) as usize;
        let end = pos + 4 + len;
        if buf.len() < end {
            return None;
        }
        if block[0] & 0x80 != 0 {
            return Some(end);
        }
        pos = end;
    }
}

/// Body of the STREAMINFO metadata block (type 0, 34 bytes) in a complete
/// stream header.
fn stream_info(header: &[u8]) -> Option<&[u8]> {
    let mut pos = 4;
    while let Some(block) = header.get(pos..pos + 4) {
        let len = u32::from_be_bytes([0, block[1], block[2], block[3]]) as usize;
        let body = header.get(pos + 4..pos + 4 + len)?;
        if block[0] & 0x7F == 0 && len == 34 {
            return Some(body);
        }
        if block[0] & 0x80 != 0 {
            return None;
        }
        pos += 4 + len;
    }
    None
}

enum Header {
    /// A frame header with a matching CRC-8, of this many bytes.
    Valid(usize),
    /// Could still become a valid header: more bytes needed.
    Incomplete,
    Invalid,
}

/// Parse a FLAC frame header at the start of `b` (RFC 9639 §9.1).
fn frame_header(b: &[u8]) -> Header {
    if b.is_empty() {
        return Header::Incomplete;
    }
    if b[0] != 0xFF {
        return Header::Invalid;
    }
    if b.len() < 2 {
        return Header::Incomplete;
    }
    if b[1] & 0xFE != 0xF8 {
        return Header::Invalid;
    }
    if b.len() < 5 {
        return Header::Incomplete;
    }
    let block_code = b[2] >> 4;
    let rate_code = b[2] & 0x0F;
    let channels = b[3] >> 4;
    let depth = (b[3] >> 1) & 0x07;
    if block_code == 0 || rate_code == 0x0F || channels > 10 || depth == 3 || b[3] & 1 != 0 {
        return Header::Invalid;
    }
    let coded_len = match b[4] {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        0xF8..=0xFB => 5,
        0xFC..=0xFD => 6,
        0xFE => 7,
        _ => return Header::Invalid,
    };
    let extra = match block_code {
        6 => 1,
        7 => 2,
        _ => 0,
    } + match rate_code {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    let crc_at = 4 + coded_len + extra;
    for (i, &c) in b.iter().enumerate().take(crc_at.min(b.len())).skip(5) {
        if i < 4 + coded_len && c & 0xC0 != 0x80 {
            return Header::Invalid;
        }
    }
    if b.len() <= crc_at {
        return Header::Incomplete;
    }
    if crc8(&b[..crc_at]) == b[crc_at] {
        Header::Valid(crc_at + 1)
    } else {
        Header::Invalid
    }
}

/// End (exclusive) of the frame starting at `start`: the first position where
/// the running CRC-16 matches the 2 trailing bytes and the bytes after it
/// are either the end of the buffer or the beginning of another header.
fn frame_end(buf: &[u8], start: usize, header_len: usize) -> Option<usize> {
    let mut crc = 0u16;
    for (i, &byte) in buf.iter().enumerate().skip(start) {
        let end = i + 2;
        if end > buf.len() {
            break;
        }
        if i > start + header_len
            && crc == u16::from_be_bytes([buf[i], buf[i + 1]])
            && (end == buf.len() || !matches!(frame_header(&buf[end..]), Header::Invalid))
        {
            return Some(end);
        }
        crc = crc16_update(crc, byte);
    }
    None
}

/// CRC-8, polynomial x^8 + x^2 + x + 1, initial 0 (FLAC frame header).
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-16, polynomial x^16 + x^15 + x^2 + 1, initial 0 (FLAC frame footer).
fn crc16_update(crc: u16, byte: u8) -> u16 {
    let mut crc = crc ^ ((byte as u16) << 8);
    for _ in 0..8 {
        crc = if crc & 0x8000 != 0 {
            (crc << 1) ^ 0x8005
        } else {
            crc << 1
        };
    }
    crc
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

    /// Minimal FLAC encoder for tests: 16-bit stereo, VERBATIM subframes,
    /// fixed block size. Returns (stream bytes, header length, frame lengths).
    fn encode_flac(frames: usize, block: usize) -> (Vec<u8>, usize, Vec<usize>) {
        fn crc8(d: &[u8]) -> u8 {
            let mut c = 0u8;
            for &b in d {
                c ^= b;
                for _ in 0..8 {
                    c = if c & 0x80 != 0 {
                        (c << 1) ^ 0x07
                    } else {
                        c << 1
                    };
                }
            }
            c
        }
        fn crc16(d: &[u8]) -> u16 {
            let mut c = 0u16;
            for &b in d {
                c ^= (b as u16) << 8;
                for _ in 0..8 {
                    c = if c & 0x8000 != 0 {
                        (c << 1) ^ 0x8005
                    } else {
                        c << 1
                    };
                }
            }
            c
        }
        let rate: u64 = 44_100;
        let total = (frames * block) as u64;
        let mut out = b"fLaC".to_vec();
        out.extend_from_slice(&[0x80, 0, 0, 34]); // last block, STREAMINFO, 34 bytes
        out.extend_from_slice(&(block as u16).to_be_bytes());
        out.extend_from_slice(&(block as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]); // min/max frame size unknown
        let packed: u64 = (rate << 44) | (1 << 41) | (15 << 36) | total;
        out.extend_from_slice(&packed.to_be_bytes());
        out.extend_from_slice(&[0; 16]); // MD5 unset
        let header_len = out.len();
        let mut lens = Vec::new();
        let mut seed: u32 = 0x1234_5678;
        for f in 0..frames {
            let start = out.len();
            out.extend_from_slice(&[0xFF, 0xF8, 0x79, 0x18]); // 16-bit block size, 44.1 kHz, 2 ch, 16 bit
            if f < 0x80 {
                out.push(f as u8);
            } else {
                out.push(0xC0 | (f >> 6) as u8);
                out.push(0x80 | (f & 0x3F) as u8);
            }
            out.extend_from_slice(&((block - 1) as u16).to_be_bytes());
            let c = crc8(&out[start..]);
            out.push(c);
            for _ch in 0..2 {
                out.push(0x02); // VERBATIM, no wasted bits
                for _ in 0..block {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    out.extend_from_slice(&((seed >> 16) as i16).to_be_bytes());
                }
            }
            let c = crc16(&out[start..]);
            out.extend_from_slice(&c.to_be_bytes());
            lens.push(out.len() - start);
        }
        (out, header_len, lens)
    }

    /// #32: frames fed after initialization must be decoded, and the input
    /// buffer must not keep everything ever fed.
    #[test]
    fn stream_decoder_decodes_all_frames_with_bounded_buffer() {
        let (flac, _, lens) = encode_flac(200, 256);
        let one_shot = decode_flac_to_f32(&flac).unwrap();
        assert_eq!(one_shot.len(), 200 * 256 * 2, "fixture must be valid FLAC");

        let mut dec = FlacStreamDecoder::new();
        let mut out = Vec::new();
        let mut max_buf = 0;
        // 1000-byte chunks: frames (~2 KB) are split across feeds.
        for chunk in flac.chunks(1000) {
            out.extend(dec.feed(chunk));
            max_buf = max_buf.max(dec.buf.len());
        }
        assert!(dec.initialized);
        assert_eq!(out.len(), one_shot.len(), "every frame decoded");
        assert_eq!(out, one_shot, "same PCM as one-shot decoding");
        let frame = *lens.iter().max().unwrap();
        assert!(
            max_buf <= frame + 1000,
            "unread input stays bounded: {max_buf} bytes retained"
        );
    }

    /// Frame-aligned input (the usual wire case) is decoded without waiting
    /// for the next frame.
    #[test]
    fn stream_decoder_frame_aligned_feeds_decode_immediately() {
        let (flac, header_len, lens) = encode_flac(5, 256);
        let mut dec = FlacStreamDecoder::new();
        assert!(dec.feed(&flac[..header_len]).is_empty());
        assert!(dec.initialized);
        let mut pos = header_len;
        for len in lens {
            assert_eq!(dec.feed(&flac[pos..pos + len]).len(), 256 * 2);
            assert!(dec.buf.is_empty());
            pos += len;
        }
    }

    /// Byte-by-byte feeding: every split point inside headers and frames.
    #[test]
    fn stream_decoder_handles_any_split() {
        let (flac, _, _) = encode_flac(4, 64);
        let one_shot = decode_flac_to_f32(&flac).unwrap();
        let mut dec = FlacStreamDecoder::new();
        let mut out = Vec::new();
        for b in &flac {
            out.extend(dec.feed(std::slice::from_ref(b)));
        }
        assert_eq!(out, one_shot);
    }

    #[test]
    fn stream_decoder_reset_starts_a_new_stream() {
        let (flac, _, _) = encode_flac(3, 128);
        let mut dec = FlacStreamDecoder::new();
        let first = dec.feed(&flac[..flac.len() / 2]);
        dec.reset();
        assert!(!dec.initialized);
        assert!(dec.buf.is_empty());
        let mut out = dec.feed(&flac);
        out.extend(dec.feed(&[]));
        assert_eq!(out.len(), 3 * 128 * 2);
        assert!(first.len() < out.len());
    }

    #[test]
    fn stream_decoder_needs_header() {
        let mut dec = FlacStreamDecoder::new();
        let result = dec.feed(&[0xFF; 100]);
        assert!(result.is_empty());
        assert!(!dec.initialized);
    }
}
