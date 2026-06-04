use std::path::Path;

use serde::Deserialize;
use tracing::info;

/// TOML configuration for the OAAT endpoint daemon.
///
/// Example config (endpoint.toml):
/// ```toml
/// [endpoint]
/// name = "Living Room DAC"
/// port = 9740
/// # audio_device = "default"
///
/// [capabilities]
/// pcm_max_rate = 192000
/// pcm_max_bits = 24
/// channels_max = 2
/// # dsd = false
/// # flac = false
///
/// [logging]
/// level = "info"
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EndpointFileConfig {
    pub endpoint: EndpointSection,
    pub capabilities: CapabilitiesSection,
    pub dac: DacSection,
    pub logging: LoggingSection,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct EndpointSection {
    pub name: String,
    pub port: u16,
    pub audio_device: Option<String>,
    /// Enable TLS 1.3 on the control channel.
    pub tls: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct CapabilitiesSection {
    pub pcm_max_rate: u32,
    pub pcm_max_bits: u8,
    pub channels_max: u8,
    pub dsd: bool,
    pub flac: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LoggingSection {
    pub level: String,
}

impl Default for EndpointSection {
    fn default() -> Self {
        Self {
            name: "OAAT Endpoint".into(),
            port: 9740,
            audio_device: None,
            tls: false,
        }
    }
}

impl Default for CapabilitiesSection {
    fn default() -> Self {
        Self {
            pcm_max_rate: 192000,
            pcm_max_bits: 24,
            channels_max: 2,
            dsd: false,
            flac: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DacSection {
    /// Use ALSA hardware volume control instead of software volume.
    pub hardware_volume: bool,
    /// ALSA card number for mixer commands.
    pub card: u32,
    /// FIR filter type (ESS 9038 DACs).
    /// Options: "brick wall", "corrected minimum phase fast", "minimum phase slow",
    ///          "minimum phase fast", "linear phase slow", "linear phase fast", "apodizing fast"
    pub fir_filter: Option<String>,
}


impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

const DEFAULT_CONFIG_PATHS: &[&str] = &[
    "/etc/tune-bridge/config.toml",
    "/etc/oaat/endpoint.toml",
];

impl EndpointFileConfig {
    /// Load config from an explicit path, the default path, or built-in defaults.
    ///
    /// Priority:
    /// 1. `--config <path>` CLI flag (error if file missing)
    /// 2. `/etc/oaat/endpoint.toml` if it exists
    /// 3. Built-in defaults
    pub fn load(explicit_path: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(path) = explicit_path {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read config {path}: {e}"))?;
            let config: Self =
                toml::from_str(&contents).map_err(|e| format!("invalid config {path}: {e}"))?;
            info!(path, "loaded config");
            return Ok(config);
        }

        for default_path_str in DEFAULT_CONFIG_PATHS {
            let default_path = Path::new(default_path_str);
            if default_path.exists() {
                let contents = std::fs::read_to_string(default_path)
                    .map_err(|e| format!("cannot read config {}: {e}", default_path_str))?;
                let config: Self = toml::from_str(&contents)
                    .map_err(|e| format!("invalid config {}: {e}", default_path_str))?;
                info!(path = default_path_str, "loaded config");
                return Ok(config);
            }
        }

        info!("no config file found, using built-in defaults");
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = EndpointFileConfig::default();
        assert_eq!(cfg.endpoint.name, "OAAT Endpoint");
        assert_eq!(cfg.endpoint.port, 9740);
        assert_eq!(cfg.capabilities.pcm_max_rate, 192000);
        assert_eq!(cfg.capabilities.pcm_max_bits, 24);
        assert_eq!(cfg.capabilities.channels_max, 2);
        assert_eq!(cfg.logging.level, "info");
    }

    #[test]
    fn parse_minimal_toml() {
        let toml_str = r#"
[endpoint]
name = "My DAC"
port = 9750
"#;
        let cfg: EndpointFileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.endpoint.name, "My DAC");
        assert_eq!(cfg.endpoint.port, 9750);
        // defaults for rest
        assert_eq!(cfg.capabilities.pcm_max_rate, 192000);
    }

    #[test]
    fn parse_full_toml() {
        let toml_str = r#"
[endpoint]
name = "Living Room DAC"
port = 9740
audio_device = "hw:1,0"

[capabilities]
pcm_max_rate = 384000
pcm_max_bits = 32
channels_max = 8
dsd = true
flac = true

[logging]
level = "debug"
"#;
        let cfg: EndpointFileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.endpoint.name, "Living Room DAC");
        assert_eq!(cfg.endpoint.audio_device.as_deref(), Some("hw:1,0"));
        assert_eq!(cfg.capabilities.pcm_max_rate, 384000);
        assert_eq!(cfg.capabilities.pcm_max_bits, 32);
        assert_eq!(cfg.capabilities.channels_max, 8);
        assert!(cfg.capabilities.dsd);
        assert!(cfg.capabilities.flac);
        assert_eq!(cfg.logging.level, "debug");
    }

    #[test]
    fn fallback_to_defaults_when_no_file() {
        let cfg = EndpointFileConfig::load(None).unwrap();
        assert_eq!(cfg.endpoint.name, "OAAT Endpoint");
    }
}
