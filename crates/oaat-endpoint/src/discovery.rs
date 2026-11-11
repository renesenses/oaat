use mdns_sd::{DaemonEvent, DaemonStatus, ServiceDaemon, ServiceInfo};
use oaat_core::capability::Capabilities;
use oaat_core::{DEFAULT_CONTROL_PORT, PROTOCOL_VERSION, SERVICE_TYPE};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// How often the endpoint re-announces itself on mDNS.
///
/// Registering once at startup and then only answering queries left the
/// endpoint invisible for good as soon as one controller query was missed
/// (Wi-Fi power save, multicast briefly filtered, daemon gone): a controller
/// restarted hours later never saw it again until the endpoint restarted.
/// A periodic unsolicited announcement lets controllers (re)discover it
/// within this delay whatever happened in between.
pub const REANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for the mDNS daemon to report its status.
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
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
    if let Some(sock) = std::net::UdpSocket::bind("0.0.0.0:0").ok()
        && sock.connect("8.8.8.8:53").is_ok()
        && let Ok(addr) = sock.local_addr()
        && let std::net::IpAddr::V4(v4) = addr.ip()
    {
        return v4;
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

    /// Builds the mDNS service record. The local IP and TXT `ip` are
    /// detected on every call, and the record follows the host interfaces
    /// (`enable_addr_auto`), so a re-announcement picks up a DHCP change.
    fn service_info(&self) -> Result<(ServiceInfo, Ipv4Addr), mdns_sd::Error> {
        let service_type = format!("{}.local.", SERVICE_TYPE);
        let hostname = format!(
            "{}.local.",
            hostname::get().unwrap_or_default().to_string_lossy()
        );

        let v_str = PROTOCOL_VERSION.to_string();
        let caps_str = self.capabilities.to_string();
        let ch_str = self.channels_max.to_string();
        let local_ip = detect_local_ip();
        let ip_str = local_ip.to_string();

        let mut props: Vec<(&str, &str)> = vec![
            ("v", &v_str),
            ("id", &self.endpoint_id),
            ("name", &self.instance_name),
            ("caps", &caps_str),
            ("ch", &ch_str),
            ("ip", &ip_str),
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

        let service = ServiceInfo::new(
            &service_type,
            &self.instance_name,
            &hostname,
            &ip_str,
            self.port,
            &props[..],
        )?
        .enable_addr_auto();
        Ok((service, local_ip))
    }

    pub fn register(&self, mdns: &ServiceDaemon) -> Result<(), mdns_sd::Error> {
        let (service, local_ip) = self.service_info()?;
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

/// Keeps the endpoint announced on mDNS for as long as it runs: re-announces
/// every [`REANNOUNCE_INTERVAL`], recreates the daemon if it has stopped, and
/// logs the daemon's errors (they were invisible so far).
pub struct MdnsKeepalive {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MdnsKeepalive {
    /// Starts the keepalive thread for an announcement already registered on
    /// `mdns`.
    pub fn spawn(announcement: EndpointAnnouncement, mdns: ServiceDaemon, every: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = std::thread::Builder::new()
            .name("oaat-mdns-keepalive".into())
            .spawn(move || keepalive_loop(announcement, mdns, every, stop_t))
            .ok();
        if handle.is_none() {
            warn!("could not start the mDNS keepalive thread, announcing once only");
        }
        Self { stop, handle }
    }

    /// Stops the thread. A daemon it had to recreate is shut down; the one
    /// passed to [`MdnsKeepalive::spawn`] is left to its owner.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MdnsKeepalive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn daemon_running(mdns: &ServiceDaemon) -> bool {
    mdns.status()
        .ok()
        .and_then(|rx| rx.recv_timeout(STATUS_TIMEOUT).ok())
        .is_some_and(|st| st == DaemonStatus::Running)
}

/// One keepalive step: recreates the daemon if it has stopped, then
/// re-announces. `recreated` tells whether `mdns` is a daemon of our own.
fn keepalive_tick(
    announcement: &EndpointAnnouncement,
    mdns: &mut ServiceDaemon,
    recreated: &mut bool,
) -> Result<bool, String> {
    let mut fresh = false;
    if !daemon_running(mdns) {
        warn!("mDNS daemon not running, recreating it");
        let d =
            ServiceDaemon::new().map_err(|e| format!("could not recreate the mDNS daemon: {e}"))?;
        if *recreated {
            let _ = mdns.shutdown();
        }
        *mdns = d;
        *recreated = true;
        fresh = true;
    }
    announcement
        .service_info()
        .and_then(|(svc, _)| mdns.register(svc))
        .map_err(|e| format!("mDNS re-announcement failed: {e}"))?;
    Ok(fresh)
}

fn keepalive_loop(
    announcement: EndpointAnnouncement,
    original: ServiceDaemon,
    every: Duration,
    stop: Arc<AtomicBool>,
) {
    let mut mdns = original;
    let mut recreated = false;
    let mut events = mdns.monitor().ok();
    let mut healthy = true;
    let mut next = Instant::now() + every;

    while !stop.load(Ordering::Relaxed) {
        if let Some(rx) = &events {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    DaemonEvent::Error(e) => warn!(error = %e, "mDNS daemon error"),
                    DaemonEvent::IpAdd(ip) => info!(%ip, "mDNS: interface address added"),
                    DaemonEvent::IpDel(ip) => info!(%ip, "mDNS: interface address removed"),
                    other => debug!(event = ?other, "mDNS daemon event"),
                }
            }
        }
        if Instant::now() < next {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        next = Instant::now() + every;

        match keepalive_tick(&announcement, &mut mdns, &mut recreated) {
            Ok(fresh) => {
                if fresh {
                    events = mdns.monitor().ok();
                }
                if !healthy {
                    info!(name = %announcement.instance_name, "mDNS announcement restored");
                }
                healthy = true;
                debug!(name = %announcement.instance_name, "mDNS re-announced");
            }
            Err(e) => {
                if healthy {
                    warn!(error = %e, "mDNS announcement lost, retrying");
                }
                healthy = false;
            }
        }
    }

    if recreated {
        let _ = mdns.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oaat_core::capability::Capabilities;

    fn announcement(name: &str) -> EndpointAnnouncement {
        EndpointAnnouncement {
            instance_name: name.into(),
            port: 9740,
            endpoint_id: format!("test-{name}"),
            capabilities: Capabilities {
                pcm_max_rate_khz: 192,
                pcm_max_bits: 32,
                dsd_max_multiplier: None,
                flac: false,
                opus: false,
                truehd: false,
                eac3: false,
            },
            channels_max: 2,
            volume_type: None,
            model: None,
            vendor: None,
            firmware: None,
        }
    }

    #[test]
    fn record_follows_interfaces() {
        let (svc, _) = announcement("KeepaliveRecord").service_info().unwrap();
        assert!(
            svc.is_addr_auto(),
            "the record must follow interface changes, not freeze the startup IP"
        );
    }

    /// The daemon is shut down behind the endpoint's back: the keepalive
    /// must recreate one and announce again, instead of staying silent.
    #[test]
    fn keepalive_recreates_a_stopped_daemon() {
        let mut mdns = ServiceDaemon::new().expect("mDNS daemon");
        let ann = announcement("KeepaliveRecreate");
        ann.register(&mdns).unwrap();
        let _ = mdns.shutdown();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!daemon_running(&mdns), "precondition: daemon stopped");

        let mut recreated = false;
        let fresh = keepalive_tick(&ann, &mut mdns, &mut recreated)
            .expect("keepalive tick on a stopped daemon");
        assert!(
            fresh && recreated && daemon_running(&mdns),
            "a stopped mDNS daemon must be recreated and the endpoint announced again"
        );
        // A healthy daemon is kept, not recreated.
        assert!(!keepalive_tick(&ann, &mut mdns, &mut recreated).unwrap());
        let _ = mdns.shutdown();
    }

    #[test]
    fn keepalive_thread_stops_and_survives_a_dead_daemon() {
        let mdns = ServiceDaemon::new().expect("mDNS daemon");
        let ann = announcement("KeepaliveStop");
        ann.register(&mdns).unwrap();
        let _ = mdns.shutdown();
        let k = MdnsKeepalive::spawn(ann, mdns, Duration::from_millis(200));
        std::thread::sleep(Duration::from_millis(900));
        // Must return promptly (no hang on the dead daemon) and shut down
        // the daemon it recreated.
        let t = Instant::now();
        k.stop();
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "keepalive did not stop promptly"
        );
    }
}
