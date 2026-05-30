use mdns_sd::{ServiceDaemon, ServiceInfo};
use oaat_core::capability::Capabilities;
use oaat_core::{DEFAULT_CONTROL_PORT, PROTOCOL_VERSION, SERVICE_TYPE};
use std::net::Ipv4Addr;
use tracing::{info, warn};

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

fn detect_local_ip() -> Ipv4Addr {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok();
    if let Some(sock) = sock {
        if sock.connect("8.8.8.8:53").is_ok() {
            if let Ok(addr) = sock.local_addr() {
                if let std::net::IpAddr::V4(v4) = addr.ip() {
                    return v4;
                }
            }
        }
    }
    warn!("could not detect local IP, falling back to 0.0.0.0");
    Ipv4Addr::UNSPECIFIED
}

impl EndpointAnnouncement {
    pub fn service_type() -> &'static str {
        SERVICE_TYPE
    }

    pub fn default_port() -> u16 {
        DEFAULT_CONTROL_PORT
    }

    pub fn register(&self, mdns: &ServiceDaemon) -> Result<(), mdns_sd::Error> {
        let service_type = format!("{}.local.", SERVICE_TYPE);
        let hostname = format!(
            "{}.local.",
            hostname::get().unwrap_or_default().to_string_lossy()
        );

        let v_str = PROTOCOL_VERSION.to_string();
        let caps_str = self.capabilities.to_string();
        let ch_str = self.channels_max.to_string();

        let mut props: Vec<(&str, &str)> = vec![
            ("v", &v_str),
            ("id", &self.endpoint_id),
            ("name", &self.instance_name),
            ("caps", &caps_str),
            ("ch", &ch_str),
        ];
        if let Some(ref vol) = self.volume_type {
            props.push(("vol", vol));
        }
        if let Some(ref model) = self.model {
            props.push(("model", model));
        }
        if let Some(ref vendor) = self.vendor {
            props.push(("vendor", vendor));
        }
        if let Some(ref fw) = self.firmware {
            props.push(("fw", fw));
        }

        let local_ip = detect_local_ip();
        let ip_str = local_ip.to_string();

        let service = ServiceInfo::new(
            &service_type,
            &self.instance_name,
            &hostname,
            &ip_str,
            self.port,
            &props[..],
        )?
        .enable_addr_auto();

        mdns.register(service)?;
        info!(
            name = %self.instance_name,
            port = self.port,
            ip = %local_ip,
            service_type = SERVICE_TYPE,
            "mDNS service registered"
        );
        Ok(())
    }
}
