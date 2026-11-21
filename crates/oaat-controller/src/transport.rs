use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info};

use oaat_core::clock::ClockState;
use oaat_core::codec::FrameCodec;
use oaat_core::message::*;
use oaat_core::wire::{AUDIO_HEADER_SIZE, AudioPacketHeader, ClockSyncPacket, ClockSyncType};
use oaat_core::{Message, OaatError, PROTOCOL_VERSION};

use crate::clock::{ClockMeasurement, MonotonicTimeSource, SharedTimeSource};

#[derive(Clone)]
pub struct ControllerConfig {
    pub controller_id: String,
    pub controller_name: String,
    pub features: Vec<String>,
    pub clock_port: u16,
    /// Enable TLS 1.3 on the control channel (TOFU client).
    pub tls: bool,
}

/// A control-plane response received from the endpoint.
#[derive(Debug, Clone)]
pub enum EndpointResponse {
    FormatAccept(FormatAccept),
    FormatCounter(FormatCounter),
    FormatReject(FormatReject),
    NextTrackReady(NextTrackReady),
    NextTrackReformat(NextTrackReformat),
    StreamStats(StreamStats),
}

pub struct ConnectedEndpoint {
    writer: tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>,
    pub info: HelloAck,
    pub audio_socket: Arc<UdpSocket>,
    pub audio_target: SocketAddr,
    pub(crate) clock_socket: Arc<UdpSocket>,
    pub(crate) clock_target: SocketAddr,
    pub(crate) clock_state: Arc<Mutex<ClockState>>,
    pub(crate) time_source: SharedTimeSource,
    sequence: u16,
    fec_encoder: Option<oaat_core::fec::FecEncoder>,
    arq_cache: Option<Arc<std::sync::Mutex<ArqCache>>>,
    arq_task: Option<tokio::task::JoinHandle<()>>,
    pub response_rx: mpsc::Receiver<EndpointResponse>,
    /// `endpoint_release` (RFC §7.3.1): the endpoint gave up its audio
    /// output. Kept apart from `response_rx`, which carries replies to the
    /// controller's own requests: a release can arrive at any time. Only
    /// sent when `ControllerConfig::features` holds
    /// `FEATURE_ENDPOINT_RELEASE`.
    pub release_rx: mpsc::Receiver<EndpointRelease>,
    reader_task: tokio::task::JoinHandle<()>,
}

impl Drop for ConnectedEndpoint {
    fn drop(&mut self) {
        self.reader_task.abort();
        if let Some(task) = self.arq_task.take() {
            task.abort();
        }
    }
}

impl ConnectedEndpoint {
    pub async fn connect(
        config: &ControllerConfig,
        endpoint_addr: SocketAddr,
    ) -> Result<Self, OaatError> {
        Self::connect_with_time_source(config, endpoint_addr, Arc::new(MonotonicTimeSource)).await
    }

    /// Connect using an explicit controller clock.
    ///
    /// The regular [`Self::connect`] remains the production entry point. This
    /// seam is deliberately narrow so deterministic conformance tests can
    /// drive bootstrap and steady-state synchronization through the real
    /// transport.
    pub async fn connect_with_time_source(
        config: &ControllerConfig,
        endpoint_addr: SocketAddr,
        time_source: SharedTimeSource,
    ) -> Result<Self, OaatError> {
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(endpoint_addr),
        )
        .await
        .map_err(|_| {
            OaatError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connect timeout (5s) to {endpoint_addr}"),
            ))
        })??;

        #[cfg(feature = "tls")]
        if config.tls {
            let tls_config = oaat_core::tls::make_client_config_tofu();
            let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
            let server_name = rustls::pki_types::ServerName::try_from("oaat-endpoint")
                .map_err(|e| {
                    OaatError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("invalid server name: {e}"),
                    ))
                })?
                .to_owned();
            let tls_stream = connector.connect(server_name, stream).await.map_err(|e| {
                OaatError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("TLS handshake failed: {e}"),
                ))
            })?;

            // Log the endpoint certificate fingerprint
            let (_, server_conn) = tls_stream.get_ref();
            if let Some(cert) = server_conn.peer_certificates().and_then(|c| c.first()) {
                let fp = oaat_core::tls::cert_fingerprint(cert.as_ref());
                info!(fingerprint = %fp, "TLS connected (TOFU)");
            }

            return Self::handshake_and_spawn(config, endpoint_addr, tls_stream, time_source).await;
        }

        Self::handshake_and_spawn(config, endpoint_addr, stream, time_source).await
    }

    async fn handshake_and_spawn<S>(
        config: &ControllerConfig,
        endpoint_addr: SocketAddr,
        stream: S,
        time_source: SharedTimeSource,
    ) -> Result<Self, OaatError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut reader, mut writer) = tokio::io::split(stream);

        let hello = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            controller_id: config.controller_id.clone(),
            controller_name: config.controller_name.clone(),
            clock_port: config.clock_port,
            features: config.features.clone(),
        });
        writer.write_all(&FrameCodec::encode(&hello)).await?;

        let mut codec = FrameCodec::new();
        let mut read_buf = [0u8; 8192];
        let hello_ack = loop {
            let n = reader.read(&mut read_buf).await?;
            if n == 0 {
                return Err(OaatError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "endpoint closed during handshake",
                )));
            }
            codec.feed(&read_buf[..n]);
            if let Some(msg) = codec.decode_next()? {
                match msg {
                    Message::HelloAck(ack) => break ack,
                    _ => continue,
                }
            }
        };

        if hello_ack.protocol_version != PROTOCOL_VERSION {
            return Err(OaatError::VersionMismatch {
                expected: PROTOCOL_VERSION,
                got: hello_ack.protocol_version,
            });
        }

        info!(
            endpoint = %hello_ack.endpoint_name,
            id = %hello_ack.endpoint_id,
            "handshake complete"
        );

        let ep_ip = endpoint_addr.ip();
        let audio_target = SocketAddr::new(ep_ip, hello_ack.audio_port);
        let clock_target = SocketAddr::new(ep_ip, hello_ack.clock_port);

        let audio_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let clock_socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

        // Channel for forwarding format negotiation responses to the caller
        let (response_tx, response_rx) = mpsc::channel::<EndpointResponse>(32);
        let (release_tx, release_rx) = mpsc::channel::<EndpointRelease>(4);

        let reader_task = tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => {
                        info!("endpoint disconnected");
                        break;
                    }
                    Ok(n) => {
                        codec.feed(&buf[..n]);
                        while let Ok(Some(msg)) = codec.decode_next() {
                            match msg {
                                Message::FormatAccept(fa) => {
                                    debug!(stream_id = %fa.stream_id, "format accepted");
                                    let _ =
                                        response_tx.send(EndpointResponse::FormatAccept(fa)).await;
                                }
                                Message::FormatCounter(fc) => {
                                    debug!(
                                        stream_id = %fc.stream_id,
                                        rate = fc.sample_rate,
                                        bits = fc.bits_per_sample,
                                        "format counter-proposed"
                                    );
                                    let _ =
                                        response_tx.send(EndpointResponse::FormatCounter(fc)).await;
                                }
                                Message::FormatReject(fr) => {
                                    debug!(
                                        stream_id = %fr.stream_id,
                                        reason = %fr.reason,
                                        "format rejected"
                                    );
                                    let _ =
                                        response_tx.send(EndpointResponse::FormatReject(fr)).await;
                                }
                                Message::NextTrackReady(ntr) => {
                                    debug!(
                                        stream_id = %ntr.stream_id,
                                        "next track ready (gapless)"
                                    );
                                    let _ = response_tx
                                        .send(EndpointResponse::NextTrackReady(ntr))
                                        .await;
                                }
                                Message::NextTrackReformat(ntf) => {
                                    debug!(
                                        stream_id = %ntf.stream_id,
                                        format = %ntf.format,
                                        rate = ntf.sample_rate,
                                        "next track reformat"
                                    );
                                    let _ = response_tx
                                        .send(EndpointResponse::NextTrackReformat(ntf))
                                        .await;
                                }
                                Message::StreamStats(ss) => {
                                    info!(
                                        stream_id = %ss.stream_id,
                                        buffer_frames = ss.buffer_frames,
                                        drift_us = ss.drift_us,
                                        lost = ss.packets_lost,
                                        recovered = ss.packets_recovered,
                                        underruns = ss.underruns,
                                        arq_requested = ss.arq_requested,
                                        arq_recovered = ss.arq_recovered,
                                        arq_timeout = ss.arq_timeout,
                                        jitter_buffer_ms = ss.jitter_buffer_ms,
                                        bit_perfect_path = ss.bit_perfect,
                                        stream_integrity_ok = ss.packets_lost <= ss.arq_recovered && ss.arq_timeout == 0 && ss.underruns == 0,
                                        "endpoint stream stats"
                                    );
                                    let _ =
                                        response_tx.send(EndpointResponse::StreamStats(ss)).await;
                                }
                                Message::EndpointRelease(er) => {
                                    info!(
                                        stream_id = %er.stream_id,
                                        reason = ?er.reason,
                                        "endpoint released its output"
                                    );
                                    // never block the reader on it: an unread
                                    // release only needs to be seen once
                                    let _ = release_tx.try_send(er);
                                }
                                other => {
                                    debug!("received: {:?}", std::mem::discriminant(&other));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "control read error");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            writer: tokio::sync::Mutex::new(Box::new(writer)),
            info: hello_ack,
            audio_socket,
            audio_target,
            clock_socket,
            clock_target,
            clock_state: Arc::new(Mutex::new(ClockState::new())),
            time_source,
            sequence: 0,
            fec_encoder: None,
            arq_cache: None,
            arq_task: None,
            response_rx,
            release_rx,
            reader_task,
        })
    }

    /// True once the TCP control-plane reader has terminated. UDP send_to()
    /// alone cannot detect a remote endpoint restart.
    pub fn is_disconnected(&self) -> bool {
        self.reader_task.is_finished()
    }

    /// Mark the current session unusable after a transport write failure.
    pub fn disconnect_reader(&self) {
        self.reader_task.abort();
    }

    pub async fn send_message(&mut self, msg: &Message) -> Result<(), OaatError> {
        let mut w = self.writer.lock().await;
        w.write_all(&FrameCodec::encode(msg)).await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn propose_format(
        &mut self,
        stream_id: &str,
        format: oaat_core::AudioFormat,
        sample_rate: u32,
        channels: u8,
        channel_layout: oaat_core::ChannelLayout,
        bits_per_sample: u8,
    ) -> Result<(), OaatError> {
        self.propose_format_with_dsd_rate(
            stream_id,
            format,
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
            None,
        )
        .await
    }

    /// Explicit DSD multiplier (64/128/256/512); None preserves PCM behavior.
    pub async fn propose_format_with_dsd_rate(
        &mut self,
        stream_id: &str,
        format: oaat_core::AudioFormat,
        sample_rate: u32,
        channels: u8,
        channel_layout: oaat_core::ChannelLayout,
        bits_per_sample: u8,
        dsd_rate: Option<u16>,
    ) -> Result<(), OaatError> {
        let msg = Message::FormatPropose(FormatPropose {
            stream_id: stream_id.to_owned(),
            format,
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
            dsd_rate,
        });
        self.send_message(&msg).await
    }

    /// Enable XOR parity FEC for this connection. One parity datagram is sent
    /// after each group of `group_size` audio datagrams.
    pub fn enable_fec(&mut self, group_size: u8) {
        self.fec_encoder = Some(oaat_core::fec::FecEncoder::new(group_size));
    }

    /// Enable optional selective retransmission. The cache is bounded and
    /// receives NACKs on the controller's existing audio UDP socket.
    pub fn enable_arq(&mut self) {
        if self.arq_cache.is_some() {
            return;
        }
        let cache = Arc::new(std::sync::Mutex::new(ArqCache::default()));
        let socket = self.audio_socket.clone();
        let peer = self.audio_target;
        let shared = cache.clone();
        self.arq_task = Some(tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                let Ok((n, source)) = socket.recv_from(&mut buf).await else {
                    break;
                };
                if source != peer || n != 6 || &buf[..4] != b"GPAN" {
                    continue;
                }
                let seq = u16::from_be_bytes([buf[4], buf[5]]);
                let packet = shared
                    .lock()
                    .ok()
                    .and_then(|cache| cache.packets.get(&seq).cloned());
                if let Some(packet) = packet {
                    let mut reply = Vec::with_capacity(4 + packet.len());
                    reply.extend_from_slice(b"GPAR");
                    reply.extend_from_slice(&packet);
                    let _ = socket.send_to(&reply, peer).await;
                }
            }
        }));
        self.arq_cache = Some(cache);
    }

    pub async fn send_audio(
        &mut self,
        stream_id: u32,
        format: oaat_core::AudioFormat,
        pts_ns: u64,
        sample_offset: u64,
        payload: &[u8],
        flags: oaat_core::PacketFlags,
    ) -> Result<(), OaatError> {
        // End-of-stream is not part of a FEC group; flush the last partial
        // group at the receiver using LAST_PACKET as before.
        let use_fec = !payload.is_empty();
        let (fec_group_size, fec_index) = if use_fec {
            self.fec_encoder
                .as_ref()
                .map(|fec| (fec.group_size(), fec.next_index()))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let header = AudioPacketHeader {
            version: AudioPacketHeader::CURRENT_VERSION,
            flags,
            format,
            sequence: self.sequence,
            stream_id,
            pts_ns,
            sample_offset,
            payload_len: payload.len() as u16,
            fec_group_size,
            fec_index,
            fec_len_xor: 0,
        };
        self.sequence = self.sequence.wrapping_add(1);

        let mut buf = vec![0u8; AUDIO_HEADER_SIZE + payload.len()];
        let mut hdr_buf = [0u8; AUDIO_HEADER_SIZE];
        header.encode(&mut hdr_buf);
        buf[..AUDIO_HEADER_SIZE].copy_from_slice(&hdr_buf);
        buf[AUDIO_HEADER_SIZE..].copy_from_slice(payload);

        self.audio_socket.send_to(&buf, self.audio_target).await?;
        if let Some(cache) = &self.arq_cache {
            if let Ok(mut cache) = cache.lock() {
                cache.insert(header.sequence, buf);
            }
        }

        // Parity packets consume sequence numbers, just like Zone's FEC
        // implementation. Keep the encoder state on this connection only.
        if use_fec
            && let Some(ref mut fec) = self.fec_encoder
            && let Some(parity) = fec.feed(payload)
        {
            let parity_header = AudioPacketHeader {
                version: AudioPacketHeader::CURRENT_VERSION,
                flags: oaat_core::PacketFlags::FEC,
                format,
                sequence: self.sequence,
                stream_id,
                pts_ns,
                sample_offset,
                payload_len: parity.payload.len() as u16,
                fec_group_size,
                fec_index: 0,
                fec_len_xor: parity.len_xor,
            };
            self.sequence = self.sequence.wrapping_add(1);
            let mut parity_buf = vec![0u8; AUDIO_HEADER_SIZE + parity.payload.len()];
            let mut parity_hdr_buf = [0u8; AUDIO_HEADER_SIZE];
            parity_header.encode(&mut parity_hdr_buf);
            parity_buf[..AUDIO_HEADER_SIZE].copy_from_slice(&parity_hdr_buf);
            parity_buf[AUDIO_HEADER_SIZE..].copy_from_slice(&parity.payload);
            self.audio_socket
                .send_to(&parity_buf, self.audio_target)
                .await?;
            if let Some(cache) = &self.arq_cache {
                if let Ok(mut cache) = cache.lock() {
                    cache.insert(parity_header.sequence, parity_buf);
                }
            }
        }
        Ok(())
    }

    pub async fn send_play(&mut self, stream_id: &str) -> Result<(), OaatError> {
        self.send_message(&Message::Play(Play {
            stream_id: stream_id.to_owned(),
        }))
        .await
    }

    pub async fn send_stop(&mut self, stream_id: &str) -> Result<(), OaatError> {
        self.send_message(&Message::Stop(Stop {
            stream_id: stream_id.to_owned(),
        }))
        .await
    }

    pub async fn send_metadata(&mut self, track: TrackMetadata) -> Result<(), OaatError> {
        self.send_message(&Message::Metadata(Metadata { track }))
            .await
    }

    pub async fn send_volume(&mut self, level: u8) -> Result<(), OaatError> {
        self.send_message(&Message::VolumeSet(VolumeSet { level }))
            .await
    }

    pub async fn send_mute(&mut self, muted: bool) -> Result<(), OaatError> {
        self.send_message(&Message::Mute(Mute { muted })).await
    }

    /// Inform the endpoint about the next track's format so it can decide
    /// whether gapless playback is possible (same format) or a reformat is
    /// needed (different format / sample rate).
    pub async fn prepare_next_track(
        &mut self,
        stream_id: &str,
        format: oaat_core::AudioFormat,
        sample_rate: u32,
        channels: u8,
        channel_layout: oaat_core::ChannelLayout,
        bits_per_sample: u8,
    ) -> Result<(), OaatError> {
        let msg = Message::NextTrackPrepare(NextTrackPrepare {
            stream_id: stream_id.to_owned(),
            format,
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
        });
        self.send_message(&msg).await
    }

    /// Run clock sync exchange. Returns (offset_ns, rtt_ns) after this sample.
    pub async fn clock_sync_once(&mut self, seq: u16) -> Result<(i64, u64), OaatError> {
        let t1 = self.time_source.now_ns();
        let request = ClockSyncPacket {
            version: 1,
            kind: ClockSyncType::Request,
            sequence: seq,
            t1,
            t2: 0,
            t3: 0,
        };
        let mut buf = [0u8; ClockSyncPacket::SIZE];
        request.encode(&mut buf);
        self.clock_socket.send_to(&buf, self.clock_target).await?;

        let mut resp_buf = [0u8; ClockSyncPacket::SIZE];
        let _ = self.clock_socket.recv(&mut resp_buf).await?;
        let t4 = self.time_source.now_ns();

        let response = ClockSyncPacket::decode(&resp_buf)?;

        let mut state = self.clock_state.lock().await;
        state.update(t1, response.t2, response.t3, t4);
        Ok((state.offset_ns(), state.rtt_ns()))
    }

    /// Run the bootstrap clock sync (10 rapid exchanges).
    pub async fn clock_sync_bootstrap(&mut self) -> Result<(), OaatError> {
        for seq in 0..10u16 {
            let (offset, rtt) = self.clock_sync_once(seq).await?;
            debug!(seq, offset, rtt, "clock sync");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        info!("clock sync bootstrap complete");
        Ok(())
    }

    pub async fn clock_offset_ns(&self) -> i64 {
        self.clock_state.lock().await.offset_ns()
    }

    /// Latest immutable clock estimate, or `None` until one valid exchange
    /// has completed.
    pub async fn clock_measurement(&self) -> Option<ClockMeasurement> {
        let state = self.clock_state.lock().await;
        (state.samples() > 0).then(|| ClockMeasurement {
            samples: state.samples(),
            bootstrapped: state.is_bootstrapped(),
            offset_ns: state.offset_ns(),
            rtt_ns: state.rtt_ns(),
            jitter_ns: state.jitter_ns().round().max(0.0) as u64,
        })
    }

    /// Check whether the TCP reader task is still running.
    /// Returns `false` once the endpoint has disconnected or the reader errored out.
    pub fn is_reader_alive(&self) -> bool {
        !self.reader_task.is_finished()
    }
}

/// Bounded retransmission cache: 4096 datagrams, no unbounded allocations.
#[derive(Default)]
struct ArqCache {
    packets: HashMap<u16, Vec<u8>>,
    order: VecDeque<u16>,
}
impl ArqCache {
    fn insert(&mut self, seq: u16, packet: Vec<u8>) {
        if !self.packets.contains_key(&seq) {
            self.order.push_back(seq);
        }
        self.packets.insert(seq, packet);
        while self.order.len() > 4096 {
            if let Some(old) = self.order.pop_front() {
                self.packets.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod gp_arq_tests {
    use super::*;
    #[test]
    fn cache_bounded_and_wrap_safe() {
        let mut cache = ArqCache::default();
        for i in 0..5000u32 {
            cache.insert(i as u16, vec![i as u8]);
        }
        assert_eq!(cache.packets.len(), 4096);
        assert!(cache.packets.get(&0).is_none());
        assert!(cache.packets.get(&4999).is_some());
    }
}
