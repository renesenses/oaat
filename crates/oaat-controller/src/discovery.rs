use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use oaat_core::capability::Capabilities;
use oaat_core::{CTRL_SERVICE_TYPE, PROTOCOL_VERSION, SERVICE_TYPE};
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct DiscoveredEndpoint {
    pub name: String,
    pub endpoint_id: String,
    pub addr: SocketAddr,
    pub capabilities: Option<Capabilities>,
    pub channels_max: u8,
    pub model: Option<String>,
    pub vendor: Option<String>,
}

pub struct ControllerDiscovery {
    mdns: ServiceDaemon,
}

impl ControllerDiscovery {
    pub fn new() -> Result<Self, mdns_sd::Error> {
        let mdns = ServiceDaemon::new()?;
        Ok(Self { mdns })
    }

    pub fn browse(&self) -> Result<mdns_sd::Receiver<ServiceEvent>, mdns_sd::Error> {
        let service_type = format!("{}.local.", SERVICE_TYPE);
        self.mdns.browse(&service_type)
    }

    /// Browse for endpoints, returning the first one found within timeout.
    pub fn find_first(&self, timeout: Duration) -> Option<DiscoveredEndpoint> {
        let receiver = self.browse().ok()?;
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }

            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some(ep) = Self::parse_endpoint(&info) {
                        return Some(ep);
                    }
                }
                Ok(event) => {
                    debug!("mDNS event: {:?}", event);
                }
                Err(_) => return None,
            }
        }
    }

    /// Browse and collect all endpoints found within timeout.
    pub fn find_all(&self, timeout: Duration) -> Vec<DiscoveredEndpoint> {
        let mut endpoints = Vec::new();
        let receiver = match self.browse() {
            Ok(r) => r,
            Err(_) => return endpoints,
        };
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    if let Some(ep) = Self::parse_endpoint(&info) {
                        info!(name = %ep.name, addr = %ep.addr, "discovered endpoint");
                        endpoints.push(ep);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        endpoints
    }

    fn parse_endpoint(info: &ServiceInfo) -> Option<DiscoveredEndpoint> {
        let ip = *info.get_addresses().iter().next()?;
        let port = info.get_port();
        let props = info.get_properties();

        let endpoint_id = props.get("id")?.val_str().to_owned();
        let name = props
            .get("name")
            .map(|p| p.val_str().to_owned())
            .unwrap_or_else(|| info.get_fullname().to_owned());
        let caps = props
            .get("caps")
            .and_then(|p| Capabilities::parse(p.val_str()).ok());
        let channels_max = props
            .get("ch")
            .and_then(|p| p.val_str().parse().ok())
            .unwrap_or(2);
        let model = props.get("model").map(|p| p.val_str().to_owned());
        let vendor = props.get("vendor").map(|p| p.val_str().to_owned());

        Some(DiscoveredEndpoint {
            name,
            endpoint_id,
            addr: SocketAddr::new(ip, port),
            capabilities: caps,
            channels_max,
            model,
            vendor,
        })
    }

    pub fn announce_controller(
        &self,
        controller_id: &str,
        controller_name: &str,
        port: u16,
        zone_count: u32,
    ) -> Result<(), mdns_sd::Error> {
        let service_type = format!("{}.local.", CTRL_SERVICE_TYPE);
        let hostname = format!(
            "{}.local.",
            hostname::get().unwrap_or_default().to_string_lossy()
        );
        let props = [
            ("v", PROTOCOL_VERSION.to_string()),
            ("id", controller_id.to_owned()),
            ("name", controller_name.to_owned()),
            ("zones", zone_count.to_string()),
        ];
        let props_ref: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();

        let service = ServiceInfo::new(
            &service_type,
            controller_name,
            &hostname,
            "",
            port,
            &props_ref[..],
        )?;

        self.mdns.register(service)?;
        info!(name = controller_name, "controller announced via mDNS");
        Ok(())
    }

    pub fn shutdown(self) -> Result<(), mdns_sd::Error> {
        self.mdns.shutdown().map(|_| ())
    }
}
