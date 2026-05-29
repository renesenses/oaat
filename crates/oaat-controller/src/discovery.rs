use oaat_core::{CTRL_SERVICE_TYPE, PROTOCOL_VERSION, SERVICE_TYPE};

pub struct ControllerAnnouncement {
    pub controller_id: String,
    pub controller_name: String,
    pub port: u16,
    pub zone_count: u32,
}

impl ControllerAnnouncement {
    pub fn txt_records(&self) -> Vec<(String, String)> {
        vec![
            ("v".into(), PROTOCOL_VERSION.to_string()),
            ("id".into(), self.controller_id.clone()),
            ("name".into(), self.controller_name.clone()),
            ("zones".into(), self.zone_count.to_string()),
        ]
    }

    pub fn service_type() -> &'static str {
        CTRL_SERVICE_TYPE
    }

    pub fn browse_service_type() -> &'static str {
        SERVICE_TYPE
    }
}
