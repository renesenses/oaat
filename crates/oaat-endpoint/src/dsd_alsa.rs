//! Native ALSA words and DoP from Tune Bridge wire formats.
//! Tune sends DSF bytes as DSD_U8 in LSB-first order today; preserve the
//! existing endpoint conversion. Word-aligned LE formats are already MSB-first.
//! No audio device is used by this module or its tests.
use oaat_core::format::AudioFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    U32Be,
    U32Le,
    U16Be,
    U16Le,
    U8,
    Dop32,
    Dop24,
}
impl Mode {
    pub const ALL: [Self; 7] = [
        Self::U32Be,
        Self::U32Le,
        Self::U16Be,
        Self::U16Le,
        Self::U8,
        Self::Dop32,
        Self::Dop24,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::U32Be => "DSD_U32_BE",
            Self::U32Le => "DSD_U32_LE",
            Self::U16Be => "DSD_U16_BE",
            Self::U16Le => "DSD_U16_LE",
            Self::U8 => "DSD_U8",
            Self::Dop32 => "S32_LE",
            Self::Dop24 => "S24_LE",
        }
    }
    pub fn width(self) -> usize {
        match self {
            Self::U32Be | Self::U32Le => 4,
            Self::U8 => 1,
            _ => 2,
        }
    }
    pub fn rate(self, bit_rate: u32) -> u32 {
        bit_rate / (8 * self.width() as u32)
    }
    pub fn is_dop(self) -> bool {
        matches!(self, Self::Dop32 | Self::Dop24)
    }
}

/// Input order is preference order: native before DoP. Never infer support
/// for a missing rate from a higher one.
pub(crate) fn select(modes: &[(Mode, Vec<u32>)], bits: u32) -> Option<Mode> {
    modes
        .iter()
        .find(|(m, rates)| {
            bits.is_multiple_of(8 * m.width() as u32) && rates.contains(&m.rate(bits))
        })
        .map(|(m, _)| *m)
}
pub(crate) fn maximum(modes: &[(Mode, Vec<u32>)]) -> Option<u16> {
    [64, 128, 256, 512]
        .into_iter()
        .take_while(|m| select(modes, *m as u32 * 44_100).is_some())
        .last()
}
pub(crate) fn bit_rate(format: AudioFormat, rate: u32) -> Option<u32> {
    rate.checked_mul(match format {
        AudioFormat::DsdU8 => 1,
        AudioFormat::DsdU16le => 2,
        AudioFormat::DsdU32le => 32,
        _ => return None,
    })
}

pub(crate) struct Encoder {
    mode: Mode,
    channels: usize,
    wire_width: usize,
    wire_tail: Vec<u8>,
    byte_tail: Vec<u8>,
    marker: u8,
}
impl Encoder {
    pub fn new(mode: Mode, format: AudioFormat, channels: u8) -> Self {
        Self {
            mode,
            channels: channels as usize,
            wire_width: match format {
                AudioFormat::DsdU16le => 2,
                AudioFormat::DsdU32le => 4,
                _ => 1,
            },
            wire_tail: Vec::new(),
            byte_tail: Vec::new(),
            marker: 0x05,
        }
    }
    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        if self.channels == 0 {
            return Vec::new();
        }
        self.wire_tail.extend_from_slice(data);
        let frame = self.wire_width * self.channels;
        let whole = self.wire_tail.len() / frame * frame;
        // Canonical byte-interleaved, MSB-first chronological stream. Word
        // formats are little endian. Tune currently sends U8 in DSF bit order,
        // unlike the RFC wording: mirror once, as the existing bridge does.
        for f in self.wire_tail[..whole].chunks_exact(frame) {
            for b in (0..self.wire_width).rev() {
                for c in 0..self.channels {
                    let byte = f[c * self.wire_width + b];
                    self.byte_tail.push(if self.wire_width == 1 {
                        byte.reverse_bits()
                    } else {
                        byte
                    });
                }
            }
        }
        self.wire_tail.drain(..whole);
        let width = self.mode.width();
        let frame = width * self.channels;
        let whole = self.byte_tail.len() / frame * frame;
        let mut out = Vec::new();
        for f in self.byte_tail[..whole].chunks_exact(frame) {
            for c in 0..self.channels {
                if self.mode.is_dop() {
                    let first = f[c];
                    let second = f[c + self.channels];
                    if self.mode == Mode::Dop32 {
                        out.push(0);
                    }
                    out.extend_from_slice(&[second, first, self.marker]);
                    if self.mode == Mode::Dop24 {
                        out.push(if self.marker & 0x80 != 0 { 0xff } else { 0 });
                    }
                } else if matches!(self.mode, Mode::U32Le | Mode::U16Le) {
                    for b in (0..width).rev() {
                        out.push(f[c + b * self.channels]);
                    }
                } else {
                    for b in 0..width {
                        out.push(f[c + b * self.channels]);
                    }
                }
            }
            self.marker ^= 0xff;
        }
        self.byte_tail.drain(..whole);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_native_formats_and_dop_are_selected_at_their_exact_rates() {
        for mode in Mode::ALL {
            let modes = vec![(mode, vec![mode.rate(2_822_400), mode.rate(5_644_800)])];
            assert_eq!(select(&modes, 2_822_400), Some(mode));
            assert_eq!(maximum(&modes), Some(128));
            assert_eq!(select(&modes, 11_289_600), None);
        }
        assert_eq!(maximum(&[(Mode::U32Le, vec![176_400])]), None);
        assert_eq!(
            select(
                &[(Mode::U16Le, vec![176_400]), (Mode::Dop32, vec![176_400])],
                2_822_400
            ),
            Some(Mode::U16Le)
        );
    }
    #[test]
    fn native_words_keep_channels_time_and_bits() {
        let wire: [u8; 8] = [0x01, 0x80, 0x02, 0x40, 0x04, 0x20, 0x08, 0x10];
        for (mode, expected) in [
            (
                Mode::U32Be,
                vec![0x80, 0x40, 0x20, 0x10, 0x01, 0x02, 0x04, 0x08],
            ),
            (
                Mode::U32Le,
                vec![0x10, 0x20, 0x40, 0x80, 0x08, 0x04, 0x02, 0x01],
            ),
            (
                Mode::U16Be,
                vec![0x80, 0x40, 0x01, 0x02, 0x20, 0x10, 0x04, 0x08],
            ),
            (
                Mode::U16Le,
                vec![0x40, 0x80, 0x02, 0x01, 0x10, 0x20, 0x08, 0x04],
            ),
            (Mode::U8, wire.iter().map(|b| b.reverse_bits()).collect()),
        ] {
            let mut encoder = Encoder::new(mode, AudioFormat::DsdU8, 2);
            let out: Vec<_> = wire.iter().flat_map(|b| encoder.feed(&[*b])).collect();
            assert_eq!(out, expected, "{mode:?}");
        }
    }
    #[test]
    fn dop_markers_and_payload_survive_single_byte_packets() {
        let input = [0x01, 0x80, 0x02, 0x40, 0x04, 0x20, 0x08, 0x10];
        let mut enc = Encoder::new(Mode::Dop32, AudioFormat::DsdU8, 2);
        let out: Vec<_> = input.iter().flat_map(|b| enc.feed(&[*b])).collect();
        assert_eq!(
            out,
            [
                0, 0x40, 0x80, 0x05, 0, 0x02, 0x01, 0x05, 0, 0x10, 0x20, 0xfa, 0, 0x08, 0x04, 0xfa
            ]
        );
        let mut enc = Encoder::new(Mode::Dop24, AudioFormat::DsdU8, 2);
        assert_eq!(
            enc.feed(&input),
            [
                0x40, 0x80, 0x05, 0, 0x02, 0x01, 0x05, 0, 0x10, 0x20, 0xfa, 0xff, 0x08, 0x04, 0xfa,
                0xff
            ]
        );
    }
    #[test]
    fn word_wire_formats_produce_the_same_dop_stream() {
        let u8s = [0x01, 0x80, 0x02, 0x40, 0x04, 0x20, 0x08, 0x10];
        let expected = Encoder::new(Mode::Dop32, AudioFormat::DsdU8, 2).feed(&u8s);
        for (format, bytes) in [
            (
                AudioFormat::DsdU16le,
                vec![0x40, 0x80, 0x02, 0x01, 0x10, 0x20, 0x08, 0x04],
            ),
            (
                AudioFormat::DsdU32le,
                vec![0x10, 0x20, 0x40, 0x80, 0x08, 0x04, 0x02, 0x01],
            ),
        ] {
            let mut enc = Encoder::new(Mode::Dop32, format, 2);
            let out: Vec<_> = bytes.iter().flat_map(|b| enc.feed(&[*b])).collect();
            assert_eq!(out, expected);
        }
    }
}
