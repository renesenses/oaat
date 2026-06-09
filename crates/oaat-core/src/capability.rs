use crate::error::OaatError;
use serde::{Deserialize, Serialize};

/// Parsed capability string from mDNS TXT records.
/// Format: `pcm:<max_rate_khz>/<max_bits>[,dsd:<max_multiplier>][,flac][,opus]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub pcm_max_rate_khz: u32,
    pub pcm_max_bits: u8,
    pub dsd_max_multiplier: Option<u16>,
    pub flac: bool,
    pub opus: bool,
    pub truehd: bool,
    pub eac3: bool,
}

impl Capabilities {
    pub fn parse(s: &str) -> Result<Self, OaatError> {
        let mut pcm_max_rate_khz = 0u32;
        let mut pcm_max_bits = 0u8;
        let mut dsd_max_multiplier = None;
        let mut flac = false;
        let mut opus = false;
        let mut truehd = false;
        let mut eac3 = false;

        for part in s.split(',') {
            let part = part.trim();
            if let Some(pcm) = part.strip_prefix("pcm:") {
                let (rate_s, bits_s) = pcm
                    .split_once('/')
                    .ok_or_else(|| OaatError::InvalidCapabilityString(s.to_owned()))?;
                pcm_max_rate_khz = rate_s
                    .parse()
                    .map_err(|_| OaatError::InvalidCapabilityString(s.to_owned()))?;
                pcm_max_bits = bits_s
                    .parse()
                    .map_err(|_| OaatError::InvalidCapabilityString(s.to_owned()))?;
            } else if let Some(dsd) = part.strip_prefix("dsd:") {
                dsd_max_multiplier = Some(
                    dsd.parse()
                        .map_err(|_| OaatError::InvalidCapabilityString(s.to_owned()))?,
                );
            } else if part == "flac" {
                flac = true;
            } else if part == "opus" {
                opus = true;
            } else if part == "truehd" {
                truehd = true;
            } else if part == "eac3" {
                eac3 = true;
            }
        }

        if pcm_max_rate_khz == 0 {
            return Err(OaatError::InvalidCapabilityString(s.to_owned()));
        }

        Ok(Self {
            pcm_max_rate_khz,
            pcm_max_bits,
            dsd_max_multiplier,
            flac,
            opus,
            truehd,
            eac3,
        })
    }

    pub fn pcm_max_rate_hz(&self) -> u32 {
        self.pcm_max_rate_khz * 1000
    }
}

impl std::fmt::Display for Capabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pcm:{}/{}", self.pcm_max_rate_khz, self.pcm_max_bits)?;
        if let Some(dsd) = self.dsd_max_multiplier {
            write!(f, ",dsd:{dsd}")?;
        }
        if self.flac {
            write!(f, ",flac")?;
        }
        if self.opus {
            write!(f, ",opus")?;
        }
        if self.truehd {
            write!(f, ",truehd")?;
        }
        if self.eac3 {
            write!(f, ",eac3")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pcm_only() {
        let caps = Capabilities::parse("pcm:192/24").unwrap();
        assert_eq!(caps.pcm_max_rate_khz, 192);
        assert_eq!(caps.pcm_max_bits, 24);
        assert_eq!(caps.dsd_max_multiplier, None);
        assert!(!caps.flac);
    }

    #[test]
    fn parse_full() {
        let caps = Capabilities::parse("pcm:768/32,dsd:256,flac,opus").unwrap();
        assert_eq!(caps.pcm_max_rate_khz, 768);
        assert_eq!(caps.pcm_max_bits, 32);
        assert_eq!(caps.dsd_max_multiplier, Some(256));
        assert!(caps.flac);
        assert!(caps.opus);
    }

    #[test]
    fn roundtrip() {
        let original = "pcm:768/32,dsd:256,flac";
        let caps = Capabilities::parse(original).unwrap();
        assert_eq!(caps.to_string(), original);
    }

    #[test]
    fn invalid() {
        assert!(Capabilities::parse("dsd:256").is_err());
        assert!(Capabilities::parse("garbage").is_err());
    }
}
