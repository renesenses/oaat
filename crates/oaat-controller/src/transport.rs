use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info};

use oaat_core::clock::ClockState;
use oaat_core::codec::FrameCodec;
use oaat_core::message::*;
use oaat_core::wire::{AudioPacketHeader, ClockSyncPacket, ClockSyncType, AUDIO_HEADER_SIZE};
use oaat_core::{Message, OaatError, PROTOCOL_VERSION};

pub struct ControllerConfig {
    pub controller_id: String,
    pub controller_name: String,
    pub features: Vec<String>,
    pub clock_port: u16,
}

/// A control-plane response received from the endpoint.
#[derive(Debug, Clone)]
pub enum EndpointResponse {
    FormatAccept(FormatAccept),
    FormatCounter(FormatCounter),
    FormatReject(FormatReject),
    NextTrackReady(NextTrackReady),
    NextTrackReformat(NextTrackReformat),
}

pub struct ConnectedEndpoint {
    writer: tokio::io::WriteHalf<TcpStream>,
    pub info: HelloAck,
    pub audio_socket: Arc<UdpSocket>,
    pub audio_target: SocketAddr,
    clock_socket: Arc<UdpSocket>,
    clock_target: SocketAddr,
    clock_state: Arc<Mutex<ClockState>>,
    sequence: u16,
    pub response_rx: mpsc::Receiver<EndpointResponse>,
}

impl ConnectedEndpoint {
    pub async fn connect(
        config: &ControllerConfig,
        endpoint_addr: SocketAddr,
    ) -> Result<Self, OaatError> {
        let stream = TcpStream::connect(endpoint_addr).await?;
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

        // Spawn reader task for control messages (responses, errors, etc.)
        tokio::spawn(async move {
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
                                    let _ = response_tx
                                        .send(EndpointResponse::FormatAccept(fa))
                                        .await;
                                }
                                Message::FormatCounter(fc) => {
                                    debug!(
                                        stream_id = %fc.stream_id,
                                        rate = fc.sample_rate,
                                        bits = fc.bits_per_sample,
                                        "format counter-proposed"
                                    );
                                    let _ = response_tx
                                        .send(EndpointResponse::FormatCounter(fc))
                                        .await;
                                }
                                Message::FormatReject(fr) => {
                                    debug!(
                                        stream_id = %fr.stream_id,
                                        reason = %fr.reason,
                                        "format rejected"
                                    );
                                    let _ = response_tx
                                        .send(EndpointResponse::FormatReject(fr))
                                        .await;
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
                                other => {
                                    debug!(
                                        "received: {:?}",
                                        std::mem::discriminant(&other)
                                    );
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
            writer,
            info: hello_ack,
            audio_socket,
            audio_target,
            clock_socket,
            clock_target,
            clock_state: Arc::new(Mutex::new(ClockState::new())),
            sequence: 0,
            response_rx,
        })
    }

    pub async fn send_message(&mut self, msg: &Message) -> Result<(), OaatError> {
        self.writer.write_all(&FrameCodec::encode(msg)).await?;
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
        let msg = Message::FormatPropose(FormatPropose {
            stream_id: stream_id.to_owned(),
            format,
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
            dsd_rate: None,
        });
        self.send_message(&msg).await
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
        let header = AudioPacketHeader {
            version: AudioPacketHeader::CURRENT_VERSION,
            flags,
            format,
            sequence: self.sequence,
            stream_id,
            pts_ns,
            sample_offset,
            payload_len: payload.len() as u16,
        };
        self.sequence = self.sequence.wrapping_add(1);

        let mut buf = vec![0u8; AUDIO_HEADER_SIZE + payload.len()];
        let mut hdr_buf = [0u8; AUDIO_HEADER_SIZE];
        header.encode(&mut hdr_buf);
        buf[..AUDIO_HEADER_SIZE].copy_from_slice(&hdr_buf);
        buf[AUDIO_HEADER_SIZE..].copy_from_slice(payload);

        self.audio_socket.send_to(&buf, self.audio_target).await?;
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
        let t1 = now_ns();
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
        let t4 = now_ns();

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
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
