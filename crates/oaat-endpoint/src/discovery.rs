use oaat_core::{DEFAULT_CONTROL_PORT, PROTOCOL_VERSION, SERVICE_TYPE};
use oaat_core::capability::Capabilities;

pub struct EndpointAnnouncement {
    pub instance_name: String,
    pub port: u16,
    pub endpoint_id: String,
    pub capabilities: Capabilities,
    pub channels_max: u8,
    pub volume_type: Option<String>,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub firmware: Option<String>,
}

impl EndpointAnnouncement {
    pub fn txt_records(&self) -> Vec<(String, String)> {
        let mut records = vec![
            ("v".into(), PROTOCOL_VERSION.to_string()),
            ("id".into(), self.endpoint_id.clone()),
            ("name".into(), self.instance_name.clone()),
            ("caps".into(), self.capabilities.to_string()),
            ("ch".into(), self.channels_max.to_string()),
        ];
        if let Some(ref vol) = self.volume_type {
            records.push(("vol".into(), vol.clone()));
        }
        if let Some(ref model) = self.model {
            records.push(("model".into(), model.clone()));
        }
        if let Some(ref vendor) = self.vendor {
            records.push(("vendor".into(), vendor.clone()));
        }
        if let Some(ref fw) = self.firmware {
            records.push(("fw".into(), fw.clone()));
        }
        records
    }

    pub fn service_type() -> &'static str {
        SERVICE_TYPE
    }

    pub fn default_port() -> u16 {
        DEFAULT_CONTROL_PORT
    }
}
