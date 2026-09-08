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
    volume_min: i32,
    volume_max: i32,
    mute_numid: Option<u32>,
}

impl AlsaMixer {
    pub fn new(card: u32) -> Self {
        let mut mixer = Self {
            card,
            volume_numid: None,
            volume_min: 0,
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
            self.volume_min = 0;
            self.volume_max = 100;
            self.mute_numid = Some(2);
            warn!(card = self.card, "amixer contents unavailable; using numid 1/2 fallback");
            return;
        };

        // Prefer a real Playback Volume control (SMSL USB, many USB DACs).
        if let Some((numid, min, max)) = find_playback_volume(&contents) {
            self.volume_numid = Some(numid);
            self.volume_min = min;
            self.volume_max = if max > min { max } else { min + 1 };
            info!(
                card = self.card,
                numid,
                min = self.volume_min,
                max = self.volume_max,
                "ALSA playback volume control detected"
            );
        } else {
            // ESS Sabre HAT convention in older tune-bridge setups.
            self.volume_numid = Some(1);
            self.volume_min = 0;
            self.volume_max = 100;
            warn!(
                card = self.card,
                "no Playback Volume control; falling back to numid=1"
            );
        }

        // Only bind mute when a named Playback Switch exists. Blind numid=2
        // after a successful discovery can poke an unrelated control on USB DACs.
        self.mute_numid = find_playback_switch(&contents);
    }

    pub fn set_volume(&self, level: u8) -> bool {
        let Some(numid) = self.volume_numid else {
            return false;
        };
        let raw = scale_level_to_raw(level, self.volume_min, self.volume_max);
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
        Some(scale_raw_to_level(raw, self.volume_min, self.volume_max))
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

/// Map protocol level 0–100 onto the control's `[min, max]` range (rounded).
fn scale_level_to_raw(level: u8, min: i32, max: i32) -> i32 {
    let level = i32::from(level.min(100));
    let span = max - min;
    min + (level * span + 50) / 100
}

/// Inverse of [`scale_level_to_raw`].
fn scale_raw_to_level(raw: i32, min: i32, max: i32) -> u8 {
    let span = max - min;
    if span == 0 {
        return 0;
    }
    let pct = ((raw - min) * 100 + span / 2) / span;
    pct.clamp(0, 100) as u8
}

fn find_playback_volume(contents: &str) -> Option<(u32, i32, i32)> {
    let mut best: Option<(u32, i32, i32)> = None;
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
        let min = parse_range_value(block, "min=").unwrap_or(0);
        let max = parse_range_value(block, "max=").unwrap_or(100);
        // Lowest numid wins when several * Playback Volume controls exist
        // (Master / PCM / Headphone…); good enough until a real card needs more.
        match best {
            Some((n, _, _)) if numid >= n => {}
            _ => best = Some((numid, min, max)),
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
        // Same lowest-numid heuristic as volume when several switches exist.
        match best {
            Some(n) if numid >= n => {}
            _ => best = Some(numid),
        }
    }
    best
}

fn parse_range_value(block: &str, key: &str) -> Option<i32> {
    for line in block.lines() {
        let trimmed = line.trim();
        // "; type=INTEGER,access=rw---R--,values=2,min=0,max=127,step=0"
        // Also handles negative mins: "min=-127,max=0"
        if let Some(idx) = trimmed.find(key) {
            let rest = &trimmed[idx + key.len()..];
            let num: String = rest
                .chars()
                .enumerate()
                .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
                .map(|(_, c)| c)
                .collect();
            if let Ok(v) = num.parse::<i32>() {
                return Some(v);
            }
        }
    }
    None
}

fn parse_int_value(output: &str) -> Option<i32> {
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
        assert_eq!(find_playback_volume(contents), Some((4, 0, 127)));
        assert_eq!(find_playback_switch(contents), Some(2));
    }

    #[test]
    fn detects_volume_with_nonzero_min() {
        let contents = r#"
numid=3,iface=MIXER,name='PCM Playback Volume'
  ; type=INTEGER,access=rw---R--,values=2,min=64,max=255,step=1
  : values=255,255
"#;
        assert_eq!(find_playback_volume(contents), Some((3, 64, 255)));
        assert_eq!(find_playback_switch(contents), None);
    }

    #[test]
    fn detects_volume_with_negative_min() {
        let contents = r#"
numid=5,iface=MIXER,name='Master Playback Volume'
  ; type=INTEGER,access=rw---R--,values=2,min=-127,max=0,step=1
  : values=0,0
"#;
        assert_eq!(find_playback_volume(contents), Some((5, -127, 0)));
    }

    #[test]
    fn scale_respects_min_max() {
        assert_eq!(scale_level_to_raw(0, 0, 127), 0);
        assert_eq!(scale_level_to_raw(100, 0, 127), 127);
        assert_eq!(scale_level_to_raw(50, 0, 127), 64);

        assert_eq!(scale_level_to_raw(0, 64, 255), 64);
        assert_eq!(scale_level_to_raw(100, 64, 255), 255);
        assert_eq!(scale_level_to_raw(0, -127, 0), -127);
        assert_eq!(scale_level_to_raw(100, -127, 0), 0);

        assert_eq!(scale_raw_to_level(0, 0, 127), 0);
        assert_eq!(scale_raw_to_level(127, 0, 127), 100);
        assert_eq!(scale_raw_to_level(64, 64, 255), 0);
        assert_eq!(scale_raw_to_level(255, 64, 255), 100);
        assert_eq!(scale_raw_to_level(-127, -127, 0), 0);
        assert_eq!(scale_raw_to_level(0, -127, 0), 100);
    }

    #[test]
    fn lowest_numid_wins_among_several_volumes() {
        let contents = r#"
numid=8,iface=MIXER,name='Headphone Playback Volume'
  ; type=INTEGER,access=rw---R--,values=2,min=0,max=100,step=1
  : values=100,100
numid=4,iface=MIXER,name='PCM Playback Volume'
  ; type=INTEGER,access=rw---R--,values=2,min=0,max=100,step=1
  : values=100,100
"#;
        assert_eq!(find_playback_volume(contents), Some((4, 0, 100)));
    }
}
