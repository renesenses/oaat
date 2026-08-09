use std::process::Command;
use tracing::{info, warn};

/// ALSA mixer control for endpoint hardware volume.
///
/// Supports both:
/// - ESS / Audiophonics-style cards that historically used fixed `numid=1/2`
/// - USB DACs like SMSL that expose `* Playback Volume` (often 0..127)
///
/// Controls are resolved by name first; numid fallbacks remain for ESS HATs.
pub struct AlsaMixer {
    card: u32,
    volume_numid: Option<u32>,
    volume_max: u32,
    mute_numid: Option<u32>,
}

impl AlsaMixer {
    pub fn new(card: u32) -> Self {
        let mut mixer = Self {
            card,
            volume_numid: None,
            volume_max: 100,
            mute_numid: None,
        };
        mixer.discover_controls();
        mixer
    }

    fn discover_controls(&mut self) {
        let Some(contents) = self.amixer_contents() else {
            // Legacy ESS fallback when discovery fails.
            self.volume_numid = Some(1);
            self.volume_max = 100;
            self.mute_numid = Some(2);
            warn!(card = self.card, "amixer contents unavailable; using numid 1/2 fallback");
            return;
        };

        // Prefer a real Playback Volume control (SMSL USB, many USB DACs).
        if let Some((numid, max)) = find_playback_volume(&contents) {
            self.volume_numid = Some(numid);
            self.volume_max = max.max(1);
            info!(
                card = self.card,
                numid,
                max = self.volume_max,
                "ALSA playback volume control detected"
            );
        } else {
            // ESS Sabre HAT convention in older tune-bridge setups.
            self.volume_numid = Some(1);
            self.volume_max = 100;
            warn!(
                card = self.card,
                "no Playback Volume control; falling back to numid=1"
            );
        }

        if let Some(numid) = find_playback_switch(&contents) {
            self.mute_numid = Some(numid);
        } else {
            self.mute_numid = Some(2);
        }
    }

    pub fn set_volume(&self, level: u8) -> bool {
        let level = level.min(100) as u32;
        let Some(numid) = self.volume_numid else {
            return false;
        };
        let raw = (level * self.volume_max + 50) / 100; // round
        self.amixer_cset(&format!("numid={numid}"), &raw.to_string())
    }

    pub fn set_mute(&self, muted: bool) -> bool {
        let Some(numid) = self.mute_numid else {
            return false;
        };
        let val = if muted { "off" } else { "on" };
        self.amixer_cset(&format!("numid={numid}"), val)
    }

    pub fn set_fir_filter(&self, filter_name: &str) -> bool {
        let idx = match filter_name {
            "brick wall" => 0,
            "corrected minimum phase fast" => 1,
            "minimum phase slow" => 2,
            "minimum phase fast" => 3,
            "linear phase slow" => 4,
            "linear phase fast" => 5,
            "apodizing fast" => 6,
            other => {
                if let Ok(n) = other.parse::<u32>() {
                    if n <= 6 {
                        n
                    } else {
                        warn!(filter = other, "invalid FIR filter index");
                        return false;
                    }
                } else {
                    warn!(filter = other, "unknown FIR filter name");
                    return false;
                }
            }
        };
        // ESS-only control; ignore failures on USB DACs without FIR.
        self.amixer_cset("numid=3", &idx.to_string())
    }

    pub fn get_volume(&self) -> Option<u8> {
        let numid = self.volume_numid?;
        let output = self.amixer_cget(&format!("numid={numid}"))?;
        let raw = parse_int_value(&output)?;
        let pct = (raw * 100 + self.volume_max / 2) / self.volume_max;
        Some(pct.min(100) as u8)
    }

    pub fn get_mute(&self) -> Option<bool> {
        let numid = self.mute_numid?;
        let output = self.amixer_cget(&format!("numid={numid}"))?;
        if output.contains("values=off") {
            Some(true)
        } else if output.contains("values=on") {
            Some(false)
        } else {
            None
        }
    }

    pub fn init(&self, fir_filter: Option<&str>) {
        self.set_mute(false);
        info!(card = self.card, "DAC unmuted");

        if let Some(filter) = fir_filter
            && self.set_fir_filter(filter)
        {
            info!(filter, "FIR filter set");
        }

        if let Some(vol) = self.get_volume() {
            info!(volume = vol, "DAC hardware volume");
        }
    }

    fn amixer_contents(&self) -> Option<String> {
        Command::new("amixer")
            .args(["-c", &self.card.to_string(), "contents"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn amixer_cset(&self, control: &str, value: &str) -> bool {
        match Command::new("amixer")
            .args(["-c", &self.card.to_string(), "cset", control, value])
            .output()
        {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                warn!(
                    control,
                    value,
                    stderr = String::from_utf8_lossy(&out.stderr).as_ref(),
                    "amixer cset failed"
                );
                false
            }
            Err(e) => {
                warn!(error = %e, "amixer not found");
                false
            }
        }
    }

    fn amixer_cget(&self, control: &str) -> Option<String> {
        Command::new("amixer")
            .args(["-c", &self.card.to_string(), "cget", control])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

fn find_playback_volume(contents: &str) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    for block in contents.split("numid=") {
        let Some(first_line) = block.lines().next() else {
            continue;
        };
        // "4,iface=MIXER,name='SMSL USB AUDIO  Playback Volume'"
        let Some(numid_str) = first_line.split(',').next() else {
            continue;
        };
        let Ok(numid) = numid_str.parse::<u32>() else {
            continue;
        };
        let lower = block.to_ascii_lowercase();
        if !lower.contains("playback volume") {
            continue;
        }
        if !lower.contains("iface=mixer") {
            continue;
        }
        let max = parse_max_value(block).unwrap_or(100);
        // Prefer the first stereo/main control (lowest numid wins if several).
        match best {
            Some((n, _)) if numid >= n => {}
            _ => best = Some((numid, max)),
        }
    }
    best
}

fn find_playback_switch(contents: &str) -> Option<u32> {
    let mut best: Option<u32> = None;
    for block in contents.split("numid=") {
        let Some(first_line) = block.lines().next() else {
            continue;
        };
        let Some(numid_str) = first_line.split(',').next() else {
            continue;
        };
        let Ok(numid) = numid_str.parse::<u32>() else {
            continue;
        };
        let lower = block.to_ascii_lowercase();
        if !lower.contains("playback switch") || !lower.contains("iface=mixer") {
            continue;
        }
        match best {
            Some(n) if numid >= n => {}
            _ => best = Some(numid),
        }
    }
    best
}

fn parse_max_value(block: &str) -> Option<u32> {
    for line in block.lines() {
        let trimmed = line.trim();
        // "; type=INTEGER,access=rw---R--,values=2,min=0,max=127,step=0"
        if let Some(idx) = trimmed.find("max=") {
            let rest = &trimmed[idx + 4..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = num.parse::<u32>() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_int_value(output: &str) -> Option<u32> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(": values=") {
            let raw = trimmed.strip_prefix(": values=")?;
            // "127,127" → first channel
            let first = raw.split(',').next()?.trim();
            return first.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_smsl_playback_volume() {
        let contents = r#"
numid=1,iface=PCM,name='Playback Channel Map'
  ; type=INTEGER,access=r--v-R--,values=2,min=0,max=36,step=0
  : values=0,0
numid=2,iface=MIXER,name='SMSL USB AUDIO  Playback Switch'
  ; type=BOOLEAN,access=rw------,values=2
  : values=on,on
numid=4,iface=MIXER,name='SMSL USB AUDIO  Playback Volume'
  ; type=INTEGER,access=rw---R--,values=2,min=0,max=127,step=0
  : values=127,127
"#;
        assert_eq!(find_playback_volume(contents), Some((4, 127)));
        assert_eq!(find_playback_switch(contents), Some(2));
    }
}
