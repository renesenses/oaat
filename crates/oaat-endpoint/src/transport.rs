use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use oaat_core::codec::FrameCodec;
use oaat_core::format::SampleRateFamily;
use oaat_core::message::*;
use oaat_core::wire::{AudioPacketHeader, ClockSyncPacket, ClockSyncType, AUDIO_HEADER_SIZE};
use oaat_core::{Message, OaatError, PROTOCOL_VERSION};

pub struct EndpointConfig {
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub control_addr: SocketAddr,
    pub audio_addr: SocketAddr,
    pub clock_addr: SocketAddr,
    pub capabilities: EndpointCapabilities,
    pub buffer_size_ms: u32,
    /// Enable TLS 1.3 on the control channel (self-signed cert, TOFU).
    pub tls: bool,
}

pub enum EndpointEvent {
    Connected {
        controller_id: String,
        controller_name: String,
    },
    FormatProposed(FormatPropose),
    FormatAccepted {
        stream_id: String,
    },
    FormatRejected {
        stream_id: String,
        reason: String,
    },
    AudioPacket {
        header: AudioPacketHeader,
        payload: Vec<u8>,
    },
    Playback(PlaybackCommand),
    Metadata(Metadata),
    Volume(VolumeCommand),
    NextTrackReady {
        stream_id: String,
    },
    NextTrackReformat {
        stream_id: String,
        format: oaat_core::format::AudioFormat,
        sample_rate: u32,
    },
    Disconnected,
    Error(OaatError),
}

pub enum PlaybackCommand {
    Play(String),
    Pause(String),
    Stop(String),
    Seek(String, u64),
}

pub enum VolumeCommand {
    Set(u8),
    Get,
    Mute(bool),
}

pub struct EndpointTransport;

impl EndpointTransport {
    pub async fn run(
        config: EndpointConfig,
        event_tx: mpsc::Sender<EndpointEvent>,
        _control_rx: mpsc::Receiver<Message>,
    ) -> Result<(), OaatError> {
        let tcp_listener = TcpListener::bind(config.control_addr).await?;
        let audio_socket = Arc::new(UdpSocket::bind(config.audio_addr).await?);
        let clock_socket = Arc::new(UdpSocket::bind(config.clock_addr).await?);

        let actual_audio_port = audio_socket.local_addr()?.port();
        let actual_clock_port = clock_socket.local_addr()?.port();

        // TLS setup (if enabled)
        #[cfg(feature = "tls")]
        let tls_acceptor = if config.tls {
            let (server_config, _cert_der, fingerprint) =
                oaat_core::tls::generate_self_signed_cert()
                    .map_err(|e| OaatError::Io(
                        std::io::Error::other(format!("TLS cert generation failed: {e}")),
                    ))?;
            info!(fingerprint = %fingerprint, "TLS enabled, self-signed certificate generated");
            Some(tokio_rustls::TlsAcceptor::from(Arc::new(server_config)))
        } else {
            None
        };

        info!(
            control = %tcp_listener.local_addr()?,
            audio = actual_audio_port,
            clock = actual_clock_port,
            tls = config.tls,
            "endpoint listening"
        );

        loop {
            let (stream, peer) = tcp_listener.accept().await?;
            info!(%peer, tls = config.tls, "controller connected");

            // Wrap in TLS if configured
            #[cfg(feature = "tls")]
            if let Some(ref acceptor) = tls_acceptor {
                match acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        info!(%peer, "TLS handshake complete");
                        let session = EndpointSession::new(
                            tls_stream,
                            audio_socket.clone(),
                            clock_socket.clone(),
                            event_tx.clone(),
                            config.endpoint_id.clone(),
                            config.endpoint_name.clone(),
                            config.capabilities.clone(),
                            actual_audio_port,
                            actual_clock_port,
                            config.buffer_size_ms,
                        );
                        if let Err(e) = session.run().await {
                            warn!(%peer, error = %e, "session ended");
                        }
                        info!("waiting for next controller connection...");
                        continue;
                    }
                    Err(e) => {
                        warn!(%peer, error = %e, "TLS handshake failed");
                        continue;
                    }
                }
            }

            // Plain TCP path (no TLS, or TLS feature disabled)
            let session = EndpointSession::new(
                stream,
                audio_socket.clone(),
                clock_socket.clone(),
                event_tx.clone(),
                config.endpoint_id.clone(),
                config.endpoint_name.clone(),
                config.capabilities.clone(),
                actual_audio_port,
                actual_clock_port,
                config.buffer_size_ms,
            );

            // Run session inline — when it ends, loop back to accept
            if let Err(e) = session.run().await {
                warn!(%peer, error = %e, "session ended");
            }
            info!("waiting for next controller connection...");
        }
    }
}

struct EndpointSession<S> {
    stream: S,
    audio_socket: Arc<UdpSocket>,
    clock_socket: Arc<UdpSocket>,
    event_tx: mpsc::Sender<EndpointEvent>,
    endpoint_id: String,
    endpoint_name: String,
    capabilities: EndpointCapabilities,
    audio_port: u16,
    clock_port: u16,
    buffer_size_ms: u32,
    /// Currently negotiated format (set after FormatAccept).
    current_format: Option<oaat_core::format::AudioFormat>,
    /// Currently negotiated sample rate (set after FormatAccept).
    current_sample_rate: Option<u32>,
}

impl<S> EndpointSession<S> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        stream: S,
        audio_socket: Arc<UdpSocket>,
        clock_socket: Arc<UdpSocket>,
        event_tx: mpsc::Sender<EndpointEvent>,
        endpoint_id: String,
        endpoint_name: String,
        capabilities: EndpointCapabilities,
        audio_port: u16,
        clock_port: u16,
        buffer_size_ms: u32,
    ) -> Self {
        Self {
            stream,
            audio_socket,
            clock_socket,
            event_tx,
            endpoint_id,
            endpoint_name,
            capabilities,
            audio_port,
            clock_port,
            buffer_size_ms,
            current_format: None,
            current_sample_rate: None,
        }
    }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> EndpointSession<S> {
    async fn run(self) -> Result<(), OaatError> {
        // Destructure to get owned fields -- avoids borrow conflicts after split().
        let EndpointSession {
            stream,
            audio_socket,
            clock_socket,
            event_tx,
            endpoint_id,
            endpoint_name,
            capabilities,
            audio_port,
            clock_port,
            buffer_size_ms,
            mut current_format,
            mut current_sample_rate,
        } = self;

        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut codec = FrameCodec::new();
        let mut read_buf = [0u8; 8192];

        // Phase 1: wait for Hello
        let hello = loop {
            let n = reader.read(&mut read_buf).await?;
            if n == 0 {
                let _ = event_tx.send(EndpointEvent::Disconnected).await;
                return Ok(());
            }
            codec.feed(&read_buf[..n]);
            if let Some(msg) = codec.decode_next()? {
                match msg {
                    Message::Hello(h) => break h,
                    _ => {
                        warn!("expected Hello, got {:?}", std::mem::discriminant(&msg));
                        continue;
                    }
                }
            }
        };

        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(OaatError::VersionMismatch {
                expected: PROTOCOL_VERSION,
                got: hello.protocol_version,
            });
        }

        let _ = event_tx
            .send(EndpointEvent::Connected {
                controller_id: hello.controller_id.clone(),
                controller_name: hello.controller_name.clone(),
            })
            .await;

        // Send HelloAck
        let ack = Message::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            endpoint_id,
            endpoint_name,
            capabilities: capabilities.clone(),
            audio_port,
            clock_port,
            buffer_size_ms,
        });
        writer.write_all(&FrameCodec::encode(&ack)).await?;

        info!(
            controller = %hello.controller_name,
            "handshake complete"
        );

        // Spawn clock sync responder
        tokio::spawn(async move {
            Self::clock_sync_loop(clock_socket).await;
        });

        // Spawn audio receiver
        let audio_tx = event_tx.clone();
        tokio::spawn(async move {
            Self::audio_receive_loop(audio_socket, audio_tx).await;
        });

        // Main control message loop
        loop {
            let n = reader.read(&mut read_buf).await?;
            if n == 0 {
                let _ = event_tx.send(EndpointEvent::Disconnected).await;
                break;
            }
            codec.feed(&read_buf[..n]);

            while let Some(msg) = codec.decode_next()? {
                match msg {
                    Message::FormatPropose(fp) => {
                        let response = negotiate_format(&capabilities, &fp);
                        writer.write_all(&FrameCodec::encode(&response)).await?;

                        match &response {
                            Message::FormatAccept(_) => {
                                current_format = Some(fp.format);
                                current_sample_rate = Some(fp.sample_rate);
                                let _ = event_tx
                                    .send(EndpointEvent::FormatAccepted {
                                        stream_id: fp.stream_id.clone(),
                                    })
                                    .await;
                            }
                            Message::FormatReject(r) => {
                                let _ = event_tx
                                    .send(EndpointEvent::FormatRejected {
                                        stream_id: fp.stream_id.clone(),
                                        reason: r.reason.clone(),
                                    })
                                    .await;
                            }
                            _ => {}
                        }

                        let _ = event_tx
                            .send(EndpointEvent::FormatProposed(fp))
                            .await;
                    }
                    Message::NextTrackPrepare(ntp) => {
                        let same_format = current_format == Some(ntp.format)
                            && current_sample_rate == Some(ntp.sample_rate);

                        if same_format {
                            info!(
                                stream_id = %ntp.stream_id,
                                "next track: same format, gapless ready"
                            );
                            let response = Message::NextTrackReady(NextTrackReady {
                                stream_id: ntp.stream_id.clone(),
                            });
                            writer.write_all(&FrameCodec::encode(&response)).await?;
                            let _ = event_tx
                                .send(EndpointEvent::NextTrackReady {
                                    stream_id: ntp.stream_id,
                                })
                                .await;
                        } else {
                            info!(
                                stream_id = %ntp.stream_id,
                                new_format = %ntp.format,
                                new_rate = ntp.sample_rate,
                                "next track: format change, reformat needed"
                            );
                            let response = Message::NextTrackReformat(NextTrackReformat {
                                stream_id: ntp.stream_id.clone(),
                                format: ntp.format,
                                sample_rate: ntp.sample_rate,
                            });
                            writer.write_all(&FrameCodec::encode(&response)).await?;
                            current_format = Some(ntp.format);
                            current_sample_rate = Some(ntp.sample_rate);
                            let _ = event_tx
                                .send(EndpointEvent::NextTrackReformat {
                                    stream_id: ntp.stream_id,
                                    format: ntp.format,
                                    sample_rate: ntp.sample_rate,
                                })
                                .await;
                        }
                    }
                    Message::Play(p) => {
                        let _ = event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Play(p.stream_id)))
                            .await;
                    }
                    Message::Pause(p) => {
                        let _ = event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Pause(p.stream_id)))
                            .await;
                    }
                    Message::Stop(s) => {
                        let _ = event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Stop(s.stream_id)))
                            .await;
                    }
                    Message::Seek(s) => {
                        let _ = event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Seek(
                                s.stream_id,
                                s.position_ms,
                            )))
                            .await;
                    }
                    Message::Metadata(m) => {
                        let _ = event_tx.send(EndpointEvent::Metadata(m)).await;
                    }
                    Message::VolumeSet(v) => {
                        let _ = event_tx
                            .send(EndpointEvent::Volume(VolumeCommand::Set(v.level)))
                            .await;
                    }
                    Message::VolumeGet(_) => {
                        let _ = event_tx
                            .send(EndpointEvent::Volume(VolumeCommand::Get))
                            .await;
                    }
                    Message::Mute(m) => {
                        let _ = event_tx
                            .send(EndpointEvent::Volume(VolumeCommand::Mute(m.muted)))
                            .await;
                    }
                    other => {
                        debug!("unhandled message: {:?}", std::mem::discriminant(&other));
                    }
                }
            }
        }

        Ok(())
    }

    async fn clock_sync_loop(socket: Arc<UdpSocket>) {
        let mut buf = [0u8; ClockSyncPacket::SIZE];
        loop {
            let (n, peer) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, "clock sync recv error");
                    break;
                }
            };
            if n < ClockSyncPacket::SIZE {
                continue;
            }
            let pkt = match ClockSyncPacket::decode(&buf) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if pkt.kind != ClockSyncType::Request {
                continue;
            }

            let t2 = now_ns();
            let t3 = now_ns();
            let response = ClockSyncPacket {
                version: 1,
                kind: ClockSyncType::Response,
                sequence: pkt.sequence,
                t1: pkt.t1,
                t2,
                t3,
            };
            let mut resp_buf = [0u8; ClockSyncPacket::SIZE];
            response.encode(&mut resp_buf);
            let _ = socket.send_to(&resp_buf, peer).await;
        }
    }

    async fn audio_receive_loop(
        socket: Arc<UdpSocket>,
        event_tx: mpsc::Sender<EndpointEvent>,
    ) {
        let mut buf = vec![0u8; AUDIO_HEADER_SIZE + oaat_core::MAX_AUDIO_PAYLOAD];
        loop {
            let n = match socket.recv(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    error!(error = %e, "audio recv error");
                    break;
                }
            };
            if n < AUDIO_HEADER_SIZE {
                continue;
            }
            let header_bytes: &[u8; AUDIO_HEADER_SIZE] =
                buf[..AUDIO_HEADER_SIZE].try_into().unwrap();
            let header = match AudioPacketHeader::decode(header_bytes) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let payload = buf[AUDIO_HEADER_SIZE..n].to_vec();
            let _ = event_tx
                .send(EndpointEvent::AudioPacket { header, payload })
                .await;
        }
    }
}

/// Decide whether to accept, counter-propose, or reject a format proposal.
fn negotiate_format(caps: &EndpointCapabilities, fp: &FormatPropose) -> Message {
    // Check if the format is supported at all
    if !caps.formats.contains(&fp.format) {
        return Message::FormatReject(FormatReject {
            stream_id: fp.stream_id.clone(),
            reason: format!("unsupported format: {}", fp.format),
        });
    }

    // For PCM formats, check sample rate and bit depth
    if fp.format.is_pcm() {
        let rate_ok = fp.sample_rate <= caps.pcm_max_rate;
        let bits_ok = fp.bits_per_sample <= caps.pcm_max_bits;

        if rate_ok && bits_ok {
            return Message::FormatAccept(FormatAccept {
                stream_id: fp.stream_id.clone(),
            });
        }

        // Counter-propose: stay in the same sample rate family
        let counter_rate = if rate_ok {
            fp.sample_rate
        } else {
            best_rate_in_family(fp.sample_rate, caps.pcm_max_rate)
        };

        let counter_bits = if bits_ok {
            fp.bits_per_sample
        } else {
            caps.pcm_max_bits
        };

        return Message::FormatCounter(FormatCounter {
            stream_id: fp.stream_id.clone(),
            format: fp.format,
            sample_rate: counter_rate,
            channels: fp.channels,
            channel_layout: fp.channel_layout,
            bits_per_sample: counter_bits,
            dsd_rate: fp.dsd_rate,
        });
    }

    // For DSD formats, verify the endpoint supports the requested DSD rate
    if fp.format.is_dsd() {
        match caps.dsd_max_rate {
            None => {
                return Message::FormatReject(FormatReject {
                    stream_id: fp.stream_id.clone(),
                    reason: "DSD not supported (no dsd_max_rate)".into(),
                });
            }
            Some(max_rate) => {
                let requested_rate = fp.dsd_rate.unwrap_or(64);
                if requested_rate > max_rate {
                    return Message::FormatReject(FormatReject {
                        stream_id: fp.stream_id.clone(),
                        reason: format!(
                            "DSD rate {requested_rate}x exceeds max {max_rate}x"
                        ),
                    });
                }
            }
        }
    }

    // For non-PCM formats (DSD, compressed) that are in the supported list, accept
    Message::FormatAccept(FormatAccept {
        stream_id: fp.stream_id.clone(),
    })
}

/// Find the highest sample rate in the same family that does not exceed `max_rate`.
fn best_rate_in_family(proposed_rate: u32, max_rate: u32) -> u32 {
    if let Some(family) = SampleRateFamily::of(proposed_rate) {
        // Pick the highest rate in the family that is <= max_rate
        family
            .rates()
            .iter()
            .rev()
            .copied()
            .find(|&r| r <= max_rate)
            .unwrap_or(family.rates()[0])
    } else {
        // Unknown family -- just clamp to max
        max_rate
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
