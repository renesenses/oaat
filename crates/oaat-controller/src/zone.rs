use std::collections::HashMap;
use std::net::SocketAddr;

use tracing::{error, info, warn};

use oaat_core::format::AudioFormat;
use oaat_core::message::TrackMetadata;
use oaat_core::wire::PacketFlags;
use oaat_core::{ChannelLayout, OaatError, DEFAULT_MULTIROOM_PLAY_DELAY_MS, DEFAULT_SINGLE_PLAY_DELAY_MS};

use crate::transport::{ConnectedEndpoint, ControllerConfig};

pub struct Zone {
    pub zone_id: String,
    pub name: String,
    endpoints: HashMap<String, ConnectedEndpoint>,
    config: ControllerConfig,
    sequence: u16,
}

impl Zone {
    pub fn new(zone_id: String, name: String, config: ControllerConfig) -> Self {
        Self {
            zone_id,
            name,
            endpoints: HashMap::new(),
            config,
            sequence: 0,
        }
    }

    pub fn endpoint_count(&self) -> usize {
        self.endpoints.len()
    }

    pub fn endpoint_ids(&self) -> Vec<String> {
        self.endpoints.keys().cloned().collect()
    }

    pub fn is_multiroom(&self) -> bool {
        self.endpoints.len() > 1
    }

    pub fn play_delay_ms(&self) -> u64 {
        if self.is_multiroom() {
            DEFAULT_MULTIROOM_PLAY_DELAY_MS
        } else {
            DEFAULT_SINGLE_PLAY_DELAY_MS
        }
    }

    pub async fn add_endpoint(&mut self, addr: SocketAddr) -> Result<String, OaatError> {
        let mut ep = ConnectedEndpoint::connect(&self.config, addr).await?;

        if let Err(e) = ep.clock_sync_bootstrap().await {
            warn!(error = %e, "clock sync failed for endpoint, continuing");
        }

        let ep_id = ep.info.endpoint_id.clone();
        let ep_name = ep.info.endpoint_name.clone();
        info!(
            zone = %self.name,
            endpoint = %ep_name,
            id = %ep_id,
            "endpoint added to zone"
        );
        self.endpoints.insert(ep_id.clone(), ep);
        Ok(ep_id)
    }

    pub fn remove_endpoint(&mut self, endpoint_id: &str) -> bool {
        if self.endpoints.remove(endpoint_id).is_some() {
            info!(zone = %self.name, endpoint_id, "endpoint removed from zone");
            true
        } else {
            false
        }
    }

    pub async fn propose_format_all(
        &mut self,
        stream_id: &str,
        format: AudioFormat,
        sample_rate: u32,
        channels: u8,
        channel_layout: ChannelLayout,
        bits_per_sample: u8,
    ) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep
                .propose_format(stream_id, format, sample_rate, channels, channel_layout, bits_per_sample)
                .await
            {
                error!(endpoint = %id, error = %e, "format propose failed");
            }
        }
        Ok(())
    }

    pub async fn send_metadata_all(&mut self, track: TrackMetadata) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep.send_metadata(track.clone()).await {
                error!(endpoint = %id, error = %e, "metadata send failed");
            }
        }
        Ok(())
    }

    pub async fn play_all(&mut self, stream_id: &str) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep.send_play(stream_id).await {
                error!(endpoint = %id, error = %e, "play failed");
            }
        }
        Ok(())
    }

    pub async fn stop_all(&mut self, stream_id: &str) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep.send_stop(stream_id).await {
                error!(endpoint = %id, error = %e, "stop failed");
            }
        }
        Ok(())
    }

    /// Send the same audio packet to all endpoints in the zone simultaneously.
    /// PTS is in the controller's clock domain — each endpoint adjusts via its own clock offset.
    pub async fn send_audio_all(
        &mut self,
        stream_id: u32,
        format: AudioFormat,
        pts_ns: u64,
        sample_offset: u64,
        payload: &[u8],
        flags: PacketFlags,
    ) -> Result<(), OaatError> {
        let header = oaat_core::wire::AudioPacketHeader {
            version: oaat_core::wire::AudioPacketHeader::CURRENT_VERSION,
            flags,
            format,
            sequence: self.sequence,
            stream_id,
            pts_ns,
            sample_offset,
            payload_len: payload.len() as u16,
        };
        self.sequence = self.sequence.wrapping_add(1);

        let mut buf = vec![0u8; oaat_core::wire::AUDIO_HEADER_SIZE + payload.len()];
        let mut hdr_buf = [0u8; oaat_core::wire::AUDIO_HEADER_SIZE];
        header.encode(&mut hdr_buf);
        buf[..oaat_core::wire::AUDIO_HEADER_SIZE].copy_from_slice(&hdr_buf);
        buf[oaat_core::wire::AUDIO_HEADER_SIZE..].copy_from_slice(payload);

        // Fan-out: send the same packet to all endpoints
        for (id, ep) in &self.endpoints {
            if let Err(e) = ep.audio_socket.send_to(&buf, ep.audio_target).await {
                error!(endpoint = %id, error = %e, "audio send failed");
            }
        }
        Ok(())
    }

    pub async fn set_volume_all(&mut self, level: u8) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep.send_volume(level).await {
                error!(endpoint = %id, error = %e, "volume set failed");
            }
        }
        Ok(())
    }

    pub async fn set_mute_all(&mut self, muted: bool) -> Result<(), OaatError> {
        for (id, ep) in &mut self.endpoints {
            if let Err(e) = ep.send_mute(muted).await {
                error!(endpoint = %id, error = %e, "mute failed");
            }
        }
        Ok(())
    }

    /// Run clock sync for all endpoints (steady-state, single exchange each).
    pub async fn clock_sync_all(&mut self) {
        for (id, ep) in &mut self.endpoints {
            let seq = self.sequence;
            self.sequence = self.sequence.wrapping_add(1);
            if let Err(e) = ep.clock_sync_once(seq).await {
                warn!(endpoint = %id, error = %e, "clock sync failed");
            }
        }
    }

    /// Spawn a background task that runs steady-state clock sync every 2 seconds (RFC §6.3).
    /// Returns a handle to cancel the task.
    pub fn start_steady_clock_sync(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        for (id, ep) in &mut self.endpoints {
            let clock_socket = ep.clock_socket.clone();
            let clock_target = ep.clock_target;
            let clock_state = ep.clock_state.clone();
            let ep_id = id.clone();

            let handle = tokio::spawn(async move {
                let mut seq = 0u16;
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    interval.tick().await;
                    let t1 = now_ns();
                    let request = oaat_core::wire::ClockSyncPacket {
                        version: 1,
                        kind: oaat_core::wire::ClockSyncType::Request,
                        sequence: seq,
                        t1,
                        t2: 0,
                        t3: 0,
                    };
                    let mut buf = [0u8; oaat_core::wire::ClockSyncPacket::SIZE];
                    request.encode(&mut buf);

                    if clock_socket.send_to(&buf, clock_target).await.is_err() {
                        break;
                    }

                    let mut resp_buf = [0u8; oaat_core::wire::ClockSyncPacket::SIZE];
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        clock_socket.recv(&mut resp_buf),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {
                            let t4 = now_ns();
                            if let Ok(response) = oaat_core::wire::ClockSyncPacket::decode(&resp_buf) {
                                let mut state = clock_state.lock().await;
                                state.update(t1, response.t2, response.t3, t4);
                            }
                        }
                        _ => {
                            warn!(endpoint = %ep_id, "steady-state clock sync timeout");
                        }
                    }
                    seq = seq.wrapping_add(1);
                }
            });
            handles.push(handle);
        }
        handles
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
