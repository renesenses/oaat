use std::collections::HashMap;
use std::net::SocketAddr;

use tracing::{error, info, warn};

use oaat_core::format::AudioFormat;
use oaat_core::message::{TrackMetadata, ZoneAssign, ZoneRelease, ZoneUpdate};
use oaat_core::wire::PacketFlags;
use oaat_core::{
    ChannelLayout, DEFAULT_MULTIROOM_PLAY_DELAY_MS, DEFAULT_SINGLE_PLAY_DELAY_MS, Message,
    OaatError,
};

use crate::manager::{EndpointSnapshot, EndpointState};
use crate::transport::{ConnectedEndpoint, ControllerConfig};

/// Per-endpoint volume offset model.
/// Effective volume = clamp(master + offset, 0, 100).
#[derive(Debug, Clone)]
pub struct VolumeMap {
    pub master: u8,
    offsets: HashMap<String, i8>,
}

impl VolumeMap {
    fn new() -> Self {
        Self {
            master: 100,
            offsets: HashMap::new(),
        }
    }

    pub fn effective_volume(&self, endpoint_id: &str) -> u8 {
        let offset = self.offsets.get(endpoint_id).copied().unwrap_or(0);
        (self.master as i16 + offset as i16).clamp(0, 100) as u8
    }

    pub fn offset(&self, endpoint_id: &str) -> i8 {
        self.offsets.get(endpoint_id).copied().unwrap_or(0)
    }
}

/// Tracks what the zone is currently doing, enabling late-join.
#[derive(Debug, Clone)]
struct ActiveStream {
    stream_id: String,
    format: AudioFormat,
    sample_rate: u32,
    channels: u8,
    channel_layout: ChannelLayout,
    bits_per_sample: u8,
    metadata: Option<TrackMetadata>,
    playing: bool,
}

/// Per-endpoint health + address tracking.
struct EndpointEntry {
    endpoint: ConnectedEndpoint,
    addr: SocketAddr,
    state: EndpointState,
    clock_sync_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for EndpointEntry {
    fn drop(&mut self) {
        if let Some(h) = self.clock_sync_handle.take() {
            h.abort();
        }
    }
}

pub struct Zone {
    pub zone_id: String,
    pub name: String,
    endpoints: HashMap<String, EndpointEntry>,
    config: ControllerConfig,
    sequence: u16,
    volume: VolumeMap,
    active_stream: Option<ActiveStream>,
    fec_encoder: Option<oaat_core::fec::FecEncoder>,
}

impl Zone {
    pub fn new(zone_id: String, name: String, config: ControllerConfig) -> Self {
        Self {
            zone_id,
            name,
            endpoints: HashMap::new(),
            config,
            sequence: 0,
            volume: VolumeMap::new(),
            active_stream: None,
            fec_encoder: None,
        }
    }

    // -- Query methods --

    pub fn endpoint_count(&self) -> usize {
        self.endpoints.len()
    }

    pub fn endpoint_ids(&self) -> Vec<String> {
        self.endpoints.keys().cloned().collect()
    }

    pub fn endpoint_name(&self, endpoint_id: &str) -> Option<&str> {
        self.endpoints
            .get(endpoint_id)
            .map(|e| e.endpoint.info.endpoint_name.as_str())
    }

    pub fn endpoint_addr(&self, endpoint_id: &str) -> Option<SocketAddr> {
        self.endpoints.get(endpoint_id).map(|e| e.addr)
    }

    pub fn is_multiroom(&self) -> bool {
        self.endpoints.len() > 1
    }

    pub fn is_streaming(&self) -> bool {
        self.active_stream
            .as_ref()
            .is_some_and(|s| s.playing)
    }

    pub fn play_delay_ms(&self) -> u64 {
        if self.is_multiroom() {
            DEFAULT_MULTIROOM_PLAY_DELAY_MS
        } else {
            DEFAULT_SINGLE_PLAY_DELAY_MS
        }
    }

    pub fn volume_map(&self) -> &VolumeMap {
        &self.volume
    }

    pub fn endpoint_snapshots(&self) -> Vec<EndpointSnapshot> {
        self.endpoints
            .iter()
            .map(|(id, entry)| EndpointSnapshot {
                endpoint_id: id.clone(),
                endpoint_name: entry.endpoint.info.endpoint_name.clone(),
                addr: entry.addr,
                state: entry.state,
                volume_offset: self.volume.offset(id),
            })
            .collect()
    }

    // -- Endpoint lifecycle --

    pub async fn add_endpoint(&mut self, addr: SocketAddr) -> Result<String, OaatError> {
        let mut ep = ConnectedEndpoint::connect(&self.config, addr).await?;

        if let Err(e) = ep.clock_sync_bootstrap().await {
            warn!(error = %e, "clock sync failed for endpoint, continuing");
        }

        let ep_id = ep.info.endpoint_id.clone();
        let ep_name = ep.info.endpoint_name.clone();

        // Notify the endpoint about its zone assignment
        ep.send_message(&Message::ZoneAssign(ZoneAssign {
            zone_id: self.zone_id.clone(),
            endpoint_id: ep_id.clone(),
        }))
        .await
        .ok();

        let mut entry = EndpointEntry {
            endpoint: ep,
            addr,
            state: EndpointState::Ready,
            clock_sync_handle: None,
        };

        // Start steady-state clock sync for this endpoint
        entry.clock_sync_handle = Some(spawn_endpoint_clock_sync(&entry.endpoint));

        info!(
            zone = %self.name,
            endpoint = %ep_name,
            id = %ep_id,
            "endpoint added to zone"
        );

        self.endpoints.insert(ep_id.clone(), entry);

        // Notify all other endpoints of the updated zone membership
        self.broadcast_zone_update().await;

        Ok(ep_id)
    }

    /// Add an endpoint and catch it up to the active stream (late-join).
    /// If no stream is active, behaves like `add_endpoint`.
    pub async fn join_active(
        &mut self,
        addr: SocketAddr,
    ) -> Result<String, OaatError> {
        let ep_id = self.add_endpoint(addr).await?;

        if let Some(stream) = self.active_stream.clone() {
            let entry = self.endpoints.get_mut(&ep_id).unwrap();

            // Propose the current format
            if let Err(e) = entry
                .endpoint
                .propose_format(
                    &stream.stream_id,
                    stream.format,
                    stream.sample_rate,
                    stream.channels,
                    stream.channel_layout,
                    stream.bits_per_sample,
                )
                .await
            {
                error!(endpoint = %ep_id, error = %e, "late-join format propose failed");
                self.endpoints.remove(&ep_id);
                return Err(e);
            }

            // Send metadata if available
            if let Some(meta) = &stream.metadata {
                entry.endpoint.send_metadata(meta.clone()).await.ok();
            }

            // Send volume
            let vol = self.volume.effective_volume(&ep_id);
            entry.endpoint.send_volume(vol).await.ok();

            // Send play if currently playing
            if stream.playing {
                entry.endpoint.send_play(&stream.stream_id).await.ok();
                entry.state = EndpointState::Streaming;
            }

            info!(
                zone = %self.name,
                endpoint = %ep_id,
                stream = %stream.stream_id,
                "late-join: endpoint caught up to active stream"
            );
        }

        Ok(ep_id)
    }

    /// Gracefully remove an endpoint: send ZoneRelease, then drop.
    pub fn remove_endpoint(&mut self, endpoint_id: &str) -> bool {
        if let Some(mut entry) = self.endpoints.remove(endpoint_id) {
            // Fire-and-forget: send ZoneRelease before dropping
            let zone_id = self.zone_id.clone();
            let ep_id = endpoint_id.to_owned();
            tokio::spawn(async move {
                entry
                    .endpoint
                    .send_message(&Message::ZoneRelease(ZoneRelease {
                        zone_id,
                        endpoint_id: ep_id,
                    }))
                    .await
                    .ok();
                // entry drops here, aborting reader task
            });

            self.volume.offsets.remove(endpoint_id);
            info!(zone = %self.name, endpoint_id, "endpoint removed from zone");
            true
        } else {
            false
        }
    }

    /// Remove endpoint and notify remaining endpoints of the updated membership.
    pub async fn remove_endpoint_and_notify(&mut self, endpoint_id: &str) -> bool {
        if self.remove_endpoint(endpoint_id) {
            self.broadcast_zone_update().await;
            true
        } else {
            false
        }
    }

    /// Mark an endpoint as degraded (e.g. clock sync failures, high packet loss).
    pub fn mark_degraded(&mut self, endpoint_id: &str) {
        if let Some(entry) = self.endpoints.get_mut(endpoint_id) {
            entry.state = EndpointState::Degraded;
            warn!(zone = %self.name, endpoint = endpoint_id, "endpoint marked degraded");
        }
    }

    /// Remove all endpoints that are in Disconnected state.
    pub fn prune_disconnected(&mut self) -> Vec<String> {
        let dead: Vec<String> = self
            .endpoints
            .iter()
            .filter(|(_, e)| e.state == EndpointState::Disconnected)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &dead {
            self.endpoints.remove(id);
            self.volume.offsets.remove(id);
        }
        if !dead.is_empty() {
            info!(zone = %self.name, removed = ?dead, "pruned disconnected endpoints");
        }
        dead
    }

    // -- Format negotiation --

    pub async fn propose_format_all(
        &mut self,
        stream_id: &str,
        format: AudioFormat,
        sample_rate: u32,
        channels: u8,
        channel_layout: ChannelLayout,
        bits_per_sample: u8,
    ) -> Result<(), OaatError> {
        // Track the active stream for late-joiners
        self.active_stream = Some(ActiveStream {
            stream_id: stream_id.to_owned(),
            format,
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
            metadata: None,
            playing: false,
        });

        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry
                .endpoint
                .propose_format(
                    stream_id,
                    format,
                    sample_rate,
                    channels,
                    channel_layout,
                    bits_per_sample,
                )
                .await
            {
                error!(endpoint = %id, error = %e, "format propose failed");
            }
        }
        Ok(())
    }

    // -- Metadata --

    pub async fn send_metadata_all(&mut self, track: TrackMetadata) -> Result<(), OaatError> {
        if let Some(ref mut stream) = self.active_stream {
            stream.metadata = Some(track.clone());
        }
        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry.endpoint.send_metadata(track.clone()).await {
                error!(endpoint = %id, error = %e, "metadata send failed");
            }
        }
        Ok(())
    }

    // -- Playback control --

    pub async fn play_all(&mut self, stream_id: &str) -> Result<(), OaatError> {
        if let Some(ref mut stream) = self.active_stream {
            stream.playing = true;
        }
        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry.endpoint.send_play(stream_id).await {
                error!(endpoint = %id, error = %e, "play failed");
            } else {
                entry.state = EndpointState::Streaming;
            }
        }
        Ok(())
    }

    pub async fn stop_all(&mut self, stream_id: &str) -> Result<(), OaatError> {
        self.active_stream = None;
        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry.endpoint.send_stop(stream_id).await {
                error!(endpoint = %id, error = %e, "stop failed");
            } else {
                entry.state = EndpointState::Ready;
            }
        }
        Ok(())
    }

    // -- FEC --

    /// Enable Forward Error Correction. Sends a parity packet every `group_size` data packets.
    pub fn enable_fec(&mut self, group_size: u8) {
        self.fec_encoder = Some(oaat_core::fec::FecEncoder::new(group_size));
        info!(zone = %self.name, group_size, "FEC enabled");
    }

    pub fn disable_fec(&mut self) {
        self.fec_encoder = None;
    }

    pub fn fec_enabled(&self) -> bool {
        self.fec_encoder.is_some()
    }

    // -- Audio --

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

        let targets: Vec<(String, std::sync::Arc<tokio::net::UdpSocket>, std::net::SocketAddr)> =
            self.endpoints
                .iter()
                .filter(|(_, e)| e.state != EndpointState::Disconnected)
                .map(|(id, e)| {
                    (
                        id.clone(),
                        e.endpoint.audio_socket.clone(),
                        e.endpoint.audio_target,
                    )
                })
                .collect();

        for (id, socket, target) in &targets {
            if let Err(e) = socket.send_to(&buf, target).await {
                error!(endpoint = %id, error = %e, "audio send failed");
            }
        }

        // FEC: accumulate and send parity packet when group is complete
        if let Some(ref mut fec) = self.fec_encoder {
            if let Some(parity_payload) = fec.feed(self.sequence.wrapping_sub(1), payload) {
                let parity_header = oaat_core::wire::AudioPacketHeader {
                    version: oaat_core::wire::AudioPacketHeader::CURRENT_VERSION,
                    flags: PacketFlags::FEC,
                    format,
                    sequence: self.sequence,
                    stream_id,
                    pts_ns,
                    sample_offset,
                    payload_len: parity_payload.len() as u16,
                };
                self.sequence = self.sequence.wrapping_add(1);

                let mut parity_buf =
                    vec![0u8; oaat_core::wire::AUDIO_HEADER_SIZE + parity_payload.len()];
                let mut ph_buf = [0u8; oaat_core::wire::AUDIO_HEADER_SIZE];
                parity_header.encode(&mut ph_buf);
                parity_buf[..oaat_core::wire::AUDIO_HEADER_SIZE].copy_from_slice(&ph_buf);
                parity_buf[oaat_core::wire::AUDIO_HEADER_SIZE..].copy_from_slice(&parity_payload);

                for (id, socket, target) in &targets {
                    if let Err(e) = socket.send_to(&parity_buf, target).await {
                        error!(endpoint = %id, error = %e, "FEC parity send failed");
                    }
                }
            }
        }

        Ok(())
    }

    // -- Volume: zone-wide + per-device --

    pub async fn set_volume_all(&mut self, level: u8) -> Result<(), OaatError> {
        self.volume.master = level;
        for (id, entry) in &mut self.endpoints {
            let effective = self.volume.effective_volume(id);
            if let Err(e) = entry.endpoint.send_volume(effective).await {
                error!(endpoint = %id, error = %e, "volume set failed");
            }
        }
        Ok(())
    }

    pub async fn set_volume_endpoint(
        &mut self,
        endpoint_id: &str,
        level: u8,
    ) -> Result<(), OaatError> {
        let offset = level as i8 - self.volume.master as i8;
        self.volume
            .offsets
            .insert(endpoint_id.to_owned(), offset);

        if let Some(entry) = self.endpoints.get_mut(endpoint_id) {
            entry.endpoint.send_volume(level).await?;
            info!(
                zone = %self.name,
                endpoint = endpoint_id,
                level,
                offset,
                "per-device volume set"
            );
        }
        Ok(())
    }

    pub async fn set_volume_offset(
        &mut self,
        endpoint_id: &str,
        offset: i8,
    ) -> Result<(), OaatError> {
        self.volume
            .offsets
            .insert(endpoint_id.to_owned(), offset);

        if let Some(entry) = self.endpoints.get_mut(endpoint_id) {
            let effective = self.volume.effective_volume(endpoint_id);
            entry.endpoint.send_volume(effective).await?;
            info!(
                zone = %self.name,
                endpoint = endpoint_id,
                offset,
                effective,
                "per-device volume offset set"
            );
        }
        Ok(())
    }

    pub async fn set_mute_all(&mut self, muted: bool) -> Result<(), OaatError> {
        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry.endpoint.send_mute(muted).await {
                error!(endpoint = %id, error = %e, "mute failed");
            }
        }
        Ok(())
    }

    pub async fn set_mute_endpoint(
        &mut self,
        endpoint_id: &str,
        muted: bool,
    ) -> Result<(), OaatError> {
        if let Some(entry) = self.endpoints.get_mut(endpoint_id) {
            entry.endpoint.send_mute(muted).await?;
        }
        Ok(())
    }

    // -- Clock sync --

    pub async fn clock_sync_all(&mut self) {
        for (id, entry) in &mut self.endpoints {
            let seq = self.sequence;
            self.sequence = self.sequence.wrapping_add(1);
            if let Err(e) = entry.endpoint.clock_sync_once(seq).await {
                warn!(endpoint = %id, error = %e, "clock sync failed");
            }
        }
    }

    pub fn start_steady_clock_sync(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        for entry in self.endpoints.values_mut() {
            if entry.clock_sync_handle.is_some() {
                continue;
            }
            let handle = spawn_endpoint_clock_sync(&entry.endpoint);
            handles.push(handle);
        }
        // Clock sync handles are also stored per-entry via add_endpoint/join_active.
        // These returned handles are for backward compatibility with existing callers.
        handles
    }

    // -- Health monitoring --

    /// Check which endpoints have a dead TCP reader task.
    /// Returns the endpoint IDs whose reader has finished (connection dropped).
    pub fn check_health(&self) -> Vec<String> {
        self.endpoints
            .iter()
            .filter(|(_, entry)| {
                entry.state != EndpointState::Disconnected
                    && !entry.endpoint.is_reader_alive()
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Mark an endpoint as disconnected.
    pub fn mark_disconnected(&mut self, endpoint_id: &str) {
        if let Some(entry) = self.endpoints.get_mut(endpoint_id)
            && entry.state != EndpointState::Disconnected
        {
            entry.state = EndpointState::Disconnected;
            warn!(zone = %self.name, endpoint = endpoint_id, "endpoint marked disconnected");
        }
    }

    // -- Internal helpers --

    async fn broadcast_zone_update(&mut self) {
        let ep_ids: Vec<String> = self.endpoints.keys().cloned().collect();
        let msg = Message::ZoneUpdate(ZoneUpdate {
            zone_id: self.zone_id.clone(),
            endpoint_ids: ep_ids,
        });
        for (id, entry) in &mut self.endpoints {
            if let Err(e) = entry.endpoint.send_message(&msg).await {
                warn!(endpoint = %id, error = %e, "zone update broadcast failed");
            }
        }
    }

}

fn spawn_endpoint_clock_sync(ep: &ConnectedEndpoint) -> tokio::task::JoinHandle<()> {
    let clock_socket = ep.clock_socket.clone();
    let clock_target = ep.clock_target;
    let clock_state = ep.clock_state.clone();
    let ep_id = ep.info.endpoint_id.clone();

    tokio::spawn(async move {
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
    })
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
