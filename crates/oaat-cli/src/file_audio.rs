//! File input for the demonstration controller. No resampling or DSD-to-PCM.
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use oaat_core::format::AudioFormat;

#[derive(Clone, Debug)]
pub struct Spec {
    pub format: AudioFormat,
    /// PCM frames/sec, or DSD bits/sec per channel for DsdU8.
    pub rate: u32,
    pub channels: u8,
    pub bits: u8,
    pub dsd_multiplier: Option<u16>,
    /// PCM frames or DSD bytes per channel.
    pub total_frames: u64,
}
impl Spec {
    pub fn frame_bytes(&self) -> usize {
        (self.channels as usize) * if self.format == AudioFormat::DsdU8 { 1 } else { self.bits as usize / 8 }
    }
    pub fn elapsed_ns(&self, frames: u64) -> u64 {
        let numerator = if self.format == AudioFormat::DsdU8 { 8u128 } else { 1u128 };
        ((frames as u128 * numerator * 1_000_000_000u128) / self.rate as u128) as u64
    }
    pub fn duration_s(&self) -> f64 {
        self.elapsed_ns(self.total_frames) as f64 / 1e9
    }
}

pub struct AudioFile {
    spec: Spec,
    source: Source,
}
enum Source {
    Wav { file: File, remaining: u64 },
    Dsf { file: File, remaining: u64, block_size: usize, reverse_bits: bool, pending: Vec<u8>, cursor: usize },
    Flac { samples: Vec<u8>, cursor: usize },
}
fn invalid(msg: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, msg.to_string()) }
fn u16le(b: &[u8]) -> u16 { u16::from_le_bytes(b.try_into().unwrap()) }
fn u32le(b: &[u8]) -> u32 { u32::from_le_bytes(b.try_into().unwrap()) }
fn u64le(b: &[u8]) -> u64 { u64::from_le_bytes(b.try_into().unwrap()) }

impl AudioFile {
    pub fn spec(&self) -> &Spec { &self.spec }
    pub fn open(path: &str) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 12];
        file.read_exact(&mut magic)?;
        file.seek(SeekFrom::Start(0))?;
        if &magic[..4] == b"DSD " { Self::open_dsf(file) }
        else if &magic[..4] == b"RIFF" && &magic[8..12] == b"WAVE" { Self::open_wav(file) }
        else if &magic[..4] == b"fLaC" { Self::open_flac(path) }
        else { Err(invalid("expected RIFF/WAVE, FLAC or DSF")) }
    }
    fn open_wav(mut file: File) -> io::Result<Self> {
        file.seek(SeekFrom::Start(12))?;
        let mut fmt = None;
        let mut data = None;
        loop {
            let mut h = [0u8; 8];
            if file.read_exact(&mut h).is_err() { break; }
            let len = u32le(&h[4..8]) as u64;
            let at = file.stream_position()?;
            if &h[..4] == b"fmt " {
                if len < 16 || len > 4096 { return Err(invalid("invalid WAV fmt chunk")); }
                let mut buf = vec![0; len as usize]; file.read_exact(&mut buf)?;
                let tag = u16le(&buf[0..2]);
                if tag != 1 { return Err(invalid("only integer PCM WAV is supported")); }
                let channels = u16le(&buf[2..4]);
                let rate = u32le(&buf[4..8]);
                let bits = u16le(&buf[14..16]);
                fmt = Some((channels, rate, bits));
            } else if &h[..4] == b"data" { data = Some((at,len)); break; }
            file.seek(SeekFrom::Start(at + len + len % 2))?;
        }
        let (channels, rate, bits) = fmt.ok_or_else(|| invalid("missing WAV fmt"))?;
        let (at,len) = data.ok_or_else(|| invalid("missing WAV data"))?;
        if channels == 0 || channels > 2 || rate == 0 { return Err(invalid("unsupported WAV channel count/rate")); }
        let format = match bits { 16 => AudioFormat::PcmS16le, 24 => AudioFormat::PcmS24le,
            32 => AudioFormat::PcmS32le, _ => return Err(invalid("unsupported WAV bit depth")) };
        let frame_bytes = channels as u64 * (bits as u64 / 8);
        if len % frame_bytes != 0 { return Err(invalid("truncated WAV PCM frames")); }
        file.seek(SeekFrom::Start(at))?;
        Ok(Self { spec: Spec { format, rate, channels: channels as u8, bits: bits as u8,
            dsd_multiplier: None, total_frames: len / frame_bytes }, source: Source::Wav { file, remaining: len } })
    }
    fn open_dsf(mut file: File) -> io::Result<Self> {
        let mut head = [0u8; 28]; file.read_exact(&mut head)?;
        if u64le(&head[4..12]) != 28 { return Err(invalid("invalid DSF header")); }
        let mut fmt = None;
        let mut data = None;
        loop {
            let mut h = [0u8; 12];
            if file.read_exact(&mut h).is_err() { break; }
            let size = u64le(&h[4..12]);
            if size < 12 { return Err(invalid("invalid DSF chunk size")); }
            let pos = file.stream_position()?;
            if &h[..4] == b"fmt " {
                if size < 52 || size > 4096 { return Err(invalid("invalid DSF fmt")); }
                let mut b = vec![0; (size - 12) as usize]; file.read_exact(&mut b)?;
                let channels = u32le(&b[12..16]);
                let rate = u32le(&b[16..20]);
                let bits = u32le(&b[20..24]);
                let samples = u64le(&b[24..32]);
                let block_size = u32le(&b[32..36]);
                fmt = Some((channels, rate, bits, samples, block_size));
            } else if &h[..4] == b"data" { data = Some((pos,size-12)); break; }
            file.seek(SeekFrom::Start(pos + size - 12))?;
        }
        let (channels, rate, bits, samples, block_size) = fmt.ok_or_else(|| invalid("missing DSF fmt"))?;
        let (pos, len) = data.ok_or_else(|| invalid("missing DSF data"))?;
        if channels == 0 || channels > 2 || ![1, 8].contains(&bits) || block_size == 0 || block_size > 65536 {
            return Err(invalid("unsupported DSF channel/bit-order/block size (expected 1=MSB or 8=LSB)"));
        }
        // Accept both 44.1 kHz and 48 kHz DSD families. The multiplier
        // identifies DSD64/128/256/512; Spec::rate keeps the actual bit rate.
        let multiplier = [44_100u32, 48_000]
            .into_iter()
            .find_map(|base| {
                (rate % base == 0 && [64, 128, 256, 512].contains(&(rate / base)))
                    .then_some((rate / base) as u16)
            })
            .ok_or_else(|| invalid("unsupported DSF bit rate"))?;
        if samples % 8 != 0 { return Err(invalid("DSF sample count not byte-aligned")); }
        let total = samples / 8;
        let blocks = total.div_ceil(block_size as u64);
        if len < blocks * block_size as u64 * channels as u64 {
            return Err(invalid("truncated DSF channel blocks"));
        }
        // DSD_U32_BE requires groups of four bytes per channel.
        if total % 4 != 0 { return Err(invalid("DSF sample count not DSD_U32 aligned")); }
        file.seek(SeekFrom::Start(pos))?;
        // DSF "bits per sample" field: 1 = LSB-first (every real DSF file; ffmpeg
        // calls it dsd_lsbf_planar), 8 = MSB-first. OAAT DsdU8 on the wire is
        // LSB-first, the order DSF already stores, and the endpoint reverses each
        // byte once for ALSA (MSB-first). So only an MSB-first file (8) is
        // reversed here; reversing a normal file (1) as well would invert the
        // bit order twice and play it backwards bit-wise (noise).
        Ok(Self { spec: Spec { format: AudioFormat::DsdU8, rate, channels: channels as u8,
            bits: 1, dsd_multiplier: Some(multiplier), total_frames: total },
            source: Source::Dsf { file, remaining: total, block_size: block_size as usize,
                reverse_bits: bits == 8, pending: Vec::new(), cursor: 0 } })
    }
    fn open_flac(path: &str) -> io::Result<Self> {
        let mut decoder = claxon::FlacReader::open(Path::new(path))
            .map_err(|e| invalid(&format!("FLAC: {e}")))?;
        let info = decoder.streaminfo();
        let (channels, rate, bits) = (info.channels, info.sample_rate, info.bits_per_sample);
        if channels == 0 || channels > 2 || rate == 0 || ![16,24,32].contains(&bits) {
            return Err(invalid("FLAC supports mono/stereo 16/24/32 bit only"));
        }
        let bytes = (bits / 8) as usize;
        let mut pcm = Vec::new();
        for sample in decoder.samples() {
            let v = sample.map_err(|e| invalid(&format!("FLAC decode: {e}")))?;
            for b in v.to_le_bytes().iter().take(bytes) { pcm.push(*b); }
        }
        let frame_bytes = channels as usize * bytes;
        if pcm.len() % frame_bytes != 0 { return Err(invalid("FLAC incomplete frame")); }
        let total_frames = (pcm.len() / frame_bytes) as u64;
        let format = match bits { 16 => AudioFormat::PcmS16le, 24 => AudioFormat::PcmS24le,
            _ => AudioFormat::PcmS32le };
        Ok(Self { spec: Spec { format, rate, channels: channels as u8, bits: bits as u8,
            dsd_multiplier: None, total_frames }, source: Source::Flac { samples: pcm, cursor: 0 } })
    }
    pub fn next_packet(&mut self, max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
        let frame_bytes = self.spec.frame_bytes();
        let max_frames = (max_bytes / frame_bytes).max(1);
        match &mut self.source {
            Source::Wav { file, remaining } => {
                if *remaining == 0 { return Ok(None); }
                let n = (*remaining as usize).min(max_frames * frame_bytes);
                let mut b = vec![0; n]; file.read_exact(&mut b)?; *remaining -= n as u64;
                Ok(Some(b))
            }
            Source::Flac { samples, cursor } => {
                if *cursor == samples.len() { return Ok(None); }
                let end = (*cursor + max_frames * frame_bytes).min(samples.len());
                let b = samples[*cursor..end].to_vec(); *cursor = end;
                Ok(Some(b))
            }
            Source::Dsf { file, remaining, block_size, reverse_bits, pending, cursor } => {
                if *remaining == 0 { return Ok(None); }
                if *cursor == pending.len() {
                    let mut blocks = Vec::new();
                    for _ in 0..self.spec.channels {
                        let mut b = vec![0u8; *block_size]; file.read_exact(&mut b)?;
                        blocks.push(b);
                    }
                    let useful = (*remaining as usize).min(*block_size);
                    pending.clear();
                    for i in 0..useful {
                        for b in &blocks {
                            let byte = b[i];
                            pending.push(if *reverse_bits { byte.reverse_bits() } else { byte });
                        }
                    }
                    *remaining -= useful as u64;
                    *cursor = 0;
                }
                let n = (max_frames * frame_bytes).min(pending.len() - *cursor);
                let n = n - n % (4 * self.spec.channels as usize);
                if n == 0 { return Err(invalid("DSF packet not aligned to 32 DSD bits")); }
                let b = pending[*cursor..*cursor+n].to_vec(); *cursor += n;
                Ok(Some(b))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dsd64_timing_and_packet_alignment() {
        let s = Spec { format: AudioFormat::DsdU8, rate: 2_822_400, channels: 2,
            bits: 1, dsd_multiplier: Some(64), total_frames: 352800 };
        assert_eq!(s.frame_bytes(), 2);
        assert_eq!(s.elapsed_ns(352800), 1_000_000_000);
    }
    // Build a minimal stereo DSF with one 4-byte channel block. Values are
    // deliberately asymmetric so incorrect bit reversal is observable.
    fn dsf_fixture(rate: u32, bit_order: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"DSD ");
        out.extend_from_slice(&28u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&52u64.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // format version
        out.extend_from_slice(&0u32.to_le_bytes()); // format id
        out.extend_from_slice(&2u32.to_le_bytes()); // channel type
        out.extend_from_slice(&2u32.to_le_bytes()); // channels
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&bit_order.to_le_bytes());
        out.extend_from_slice(&32u64.to_le_bytes()); // 32 DSD bits/channel
        out.extend_from_slice(&4u32.to_le_bytes()); // block size
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
        out.extend_from_slice(b"data");
        out.extend_from_slice(&20u64.to_le_bytes()); // header + 8 bytes
        out.extend_from_slice(&[0x01, 0x02, 0x04, 0x08]); // left
        out.extend_from_slice(&[0x10, 0x20, 0x40, 0x80]); // right
        out
    }

    #[test]
    fn dsf_both_rate_families_and_bit_orders() {
        for (rate, multiplier) in [
            (2_822_400, 64), (3_072_000, 64),
            (5_644_800, 128), (6_144_000, 128),
            (11_289_600, 256), (12_288_000, 256),
            (22_579_200, 512), (24_576_000, 512),
        ] {
            for bit_order in [1u32, 8] {
                let path = std::env::temp_dir().join(format!(
                    "oaat-dsf-{}-{}-{}-{}.dsf", std::process::id(),
                    rate, bit_order, line!()
                ));
                std::fs::write(&path, dsf_fixture(rate, bit_order)).unwrap();
                let result = (|| -> io::Result<(Spec, Vec<u8>)> {
                    let mut audio = AudioFile::open(path.to_str().unwrap())?;
                    let spec = audio.spec().clone();
                    let bytes = audio.next_packet(1024)?.unwrap();
                    assert!(audio.next_packet(1024)?.is_none());
                    Ok((spec, bytes))
                })();
                std::fs::remove_file(&path).unwrap();
                let (spec, bytes) = result.unwrap();
                assert_eq!(spec.rate, rate);
                assert_eq!(spec.dsd_multiplier, Some(multiplier));
                assert_eq!(spec.total_frames, 4);
                let expected = [
                    0x01u8, 0x10, 0x02, 0x20, 0x04, 0x40, 0x08, 0x80,
                ];
                let expected: Vec<u8> = expected.iter().map(|b| {
                    if bit_order == 8 { b.reverse_bits() } else { *b }
                }).collect();
                assert_eq!(bytes, expected);
            }
        }
    }

    #[test]
    fn pcm_timing() {
        let s = Spec { format: AudioFormat::PcmS24le, rate: 192000, channels: 2,
            bits: 24, dsd_multiplier: None, total_frames: 192000 };
        assert_eq!(s.frame_bytes(), 6);
        assert_eq!(s.elapsed_ns(192000), 1_000_000_000);
    }
}
