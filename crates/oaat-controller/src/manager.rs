use std::collections::HashMap;
use std::net::SocketAddr;

use tokio::sync::broadcast;
use tracing::info;

use oaat_core::OaatError;

use crate::transport::ControllerConfig;
use crate::zone::Zone;

#[derive(Debug, Clone)]
pub enum ZoneEvent {
    ZoneCreated {
        zone_id: String,
        name: String,
    },
    ZoneDissolved {
        zone_id: String,
    },
    EndpointJoined {
        zone_id: String,
        endpoint_id: String,
        endpoint_name: String,
    },
    EndpointLeft {
        zone_id: String,
        endpoint_id: String,
    },
    EndpointFailed {
        zone_id: String,
        endpoint_id: String,
        error: String,
    },
}

pub struct ZoneManager {
    zones: HashMap<String, Zone>,
    config: ControllerConfig,
    event_tx: broadcast::Sender<ZoneEvent>,
}

impl ZoneManager {
    pub fn new(config: ControllerConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            zones: HashMap::new(),
            config,
            event_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ZoneEvent> {
        self.event_tx.subscribe()
    }

    pub fn zone_ids(&self) -> Vec<String> {
        self.zones.keys().cloned().collect()
    }

    pub fn zone(&self, zone_id: &str) -> Option<&Zone> {
        self.zones.get(zone_id)
    }

    pub fn zone_mut(&mut self, zone_id: &str) -> Option<&mut Zone> {
        self.zones.get_mut(zone_id)
    }

    pub fn create_zone(&mut self, zone_id: String, name: String) -> &mut Zone {
        let zone = Zone::new(zone_id.clone(), name.clone(), self.config.clone());
        self.zones.insert(zone_id.clone(), zone);
        let _ = self.event_tx.send(ZoneEvent::ZoneCreated {
            zone_id: zone_id.clone(),
            name,
        });
        info!(zone_id = %zone_id, "zone created");
        self.zones.get_mut(&zone_id).unwrap()
    }

    pub fn dissolve_zone(&mut self, zone_id: &str) -> bool {
        if self.zones.remove(zone_id).is_some() {
            let _ = self.event_tx.send(ZoneEvent::ZoneDissolved {
                zone_id: zone_id.to_owned(),
            });
            info!(zone_id, "zone dissolved");
            true
        } else {
            false
        }
    }

    pub async fn add_endpoint_to_zone(
        &mut self,
        zone_id: &str,
        addr: SocketAddr,
    ) -> Result<String, OaatError> {
        let zone = self
            .zones
            .get_mut(zone_id)
            .ok_or_else(|| OaatError::Protocol(format!("zone not found: {zone_id}")))?;

        let ep_id = zone.add_endpoint(addr).await?;
        let ep_name = zone
            .endpoint_name(&ep_id)
            .unwrap_or_default()
            .to_owned();

        let _ = self.event_tx.send(ZoneEvent::EndpointJoined {
            zone_id: zone_id.to_owned(),
            endpoint_id: ep_id.clone(),
            endpoint_name: ep_name,
        });

        Ok(ep_id)
    }

    pub fn remove_endpoint_from_zone(&mut self, zone_id: &str, endpoint_id: &str) -> bool {
        let Some(zone) = self.zones.get_mut(zone_id) else {
            return false;
        };
        if zone.remove_endpoint(endpoint_id) {
            let _ = self.event_tx.send(ZoneEvent::EndpointLeft {
                zone_id: zone_id.to_owned(),
                endpoint_id: endpoint_id.to_owned(),
            });
            true
        } else {
            false
        }
    }

    pub async fn move_endpoint(
        &mut self,
        endpoint_id: &str,
        from_zone: &str,
        to_zone: &str,
    ) -> Result<(), OaatError> {
        let addr = {
            let zone = self
                .zones
                .get(from_zone)
                .ok_or_else(|| OaatError::Protocol(format!("zone not found: {from_zone}")))?;
            zone.endpoint_addr(endpoint_id)
                .ok_or_else(|| OaatError::Protocol(format!("endpoint not found: {endpoint_id}")))?
        };

        self.remove_endpoint_from_zone(from_zone, endpoint_id);
        self.add_endpoint_to_zone(to_zone, addr).await?;
        info!(endpoint_id, from = from_zone, to = to_zone, "endpoint moved between zones");
        Ok(())
    }

    pub fn emit_endpoint_failed(&self, zone_id: &str, endpoint_id: &str, error: String) {
        let _ = self.event_tx.send(ZoneEvent::EndpointFailed {
            zone_id: zone_id.to_owned(),
            endpoint_id: endpoint_id.to_owned(),
            error,
        });
    }

    pub fn snapshot(&self) -> Vec<ZoneSnapshot> {
        self.zones
            .values()
            .map(|z| ZoneSnapshot {
                zone_id: z.zone_id.clone(),
                name: z.name.clone(),
                endpoints: z.endpoint_snapshots(),
                is_streaming: z.is_streaming(),
                volume: z.volume_map().master,
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ZoneSnapshot {
    pub zone_id: String,
    pub name: String,
    pub endpoints: Vec<EndpointSnapshot>,
    pub is_streaming: bool,
    pub volume: u8,
}

#[derive(Debug, Clone)]
pub struct EndpointSnapshot {
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub addr: SocketAddr,
    pub state: EndpointState,
    pub volume_offset: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    Connected,
    Syncing,
    Ready,
    Streaming,
    Degraded,
    Disconnected,
}

impl std::fmt::Display for EndpointState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected => write!(f, "connected"),
            Self::Syncing => write!(f, "syncing"),
            Self::Ready => write!(f, "ready"),
            Self::Streaming => write!(f, "streaming"),
            Self::Degraded => write!(f, "degraded"),
            Self::Disconnected => write!(f, "disconnected"),
        }
    }
}
