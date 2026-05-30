use std::process::Command;
use tracing::{info, warn};

pub struct AlsaMixer {
    card: u32,
}

impl AlsaMixer {
    pub fn new(card: u32) -> Self {
        Self { card }
    }

    pub fn set_volume(&self, level: u8) -> bool {
        let level = level.min(100);
        self.amixer_cset("numid=1", &level.to_string())
    }

    pub fn set_mute(&self, muted: bool) -> bool {
        let val = if muted { "off" } else { "on" };
        self.amixer_cset("numid=2", val)
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
                    if n <= 6 { n } else { warn!(filter = other, "invalid FIR filter index"); return false; }
                } else {
                    warn!(filter = other, "unknown FIR filter name");
                    return false;
                }
            }
        };
        self.amixer_cset("numid=3", &idx.to_string())
    }

    pub fn get_volume(&self) -> Option<u8> {
        let output = self.amixer_cget("numid=1")?;
        parse_int_value(&output).map(|v| v.min(100) as u8)
    }

    pub fn get_mute(&self) -> Option<bool> {
        let output = self.amixer_cget("numid=2")?;
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

        if let Some(filter) = fir_filter {
            if self.set_fir_filter(filter) {
                info!(filter, "FIR filter set");
            }
        }

        if let Some(vol) = self.get_volume() {
            info!(volume = vol, "DAC hardware volume");
        }
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

fn parse_int_value(output: &str) -> Option<u32> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(": values=") {
            return trimmed
                .strip_prefix(": values=")
                .and_then(|v| v.parse().ok());
        }
    }
    None
}
