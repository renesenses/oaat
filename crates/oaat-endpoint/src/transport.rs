use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use oaat_core::codec::FrameCodec;
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
}

pub enum EndpointEvent {
    Connected {
        controller_id: String,
        controller_name: String,
    },
    FormatProposed(FormatPropose),
    AudioPacket {
        header: AudioPacketHeader,
        payload: Vec<u8>,
    },
    Playback(PlaybackCommand),
    Metadata(Metadata),
    Volume(VolumeCommand),
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

        info!(
            control = %tcp_listener.local_addr()?,
            audio = actual_audio_port,
            clock = actual_clock_port,
            "endpoint listening"
        );

        let (stream, peer) = tcp_listener.accept().await?;
        info!(%peer, "controller connected");

        let session = EndpointSession {
            stream,
            audio_socket,
            clock_socket,
            event_tx,
            endpoint_id: config.endpoint_id,
            endpoint_name: config.endpoint_name,
            capabilities: config.capabilities,
            audio_port: actual_audio_port,
            clock_port: actual_clock_port,
            buffer_size_ms: config.buffer_size_ms,
        };

        tokio::spawn(async move {
            if let Err(e) = session.run().await {
                error!(%peer, error = %e, "session ended with error");
            }
        });

        Ok(())
    }
}

struct EndpointSession {
    stream: TcpStream,
    audio_socket: Arc<UdpSocket>,
    clock_socket: Arc<UdpSocket>,
    event_tx: mpsc::Sender<EndpointEvent>,
    endpoint_id: String,
    endpoint_name: String,
    capabilities: EndpointCapabilities,
    audio_port: u16,
    clock_port: u16,
    buffer_size_ms: u32,
}

impl EndpointSession {
    async fn run(mut self) -> Result<(), OaatError> {
        let (mut reader, mut writer) = self.stream.split();
        let mut codec = FrameCodec::new();
        let mut read_buf = [0u8; 8192];

        // Phase 1: wait for Hello
        let hello = loop {
            let n = reader.read(&mut read_buf).await?;
            if n == 0 {
                let _ = self.event_tx.send(EndpointEvent::Disconnected).await;
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

        let _ = self
            .event_tx
            .send(EndpointEvent::Connected {
                controller_id: hello.controller_id.clone(),
                controller_name: hello.controller_name.clone(),
            })
            .await;

        // Send HelloAck
        let ack = Message::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            endpoint_id: self.endpoint_id.clone(),
            endpoint_name: self.endpoint_name.clone(),
            capabilities: self.capabilities.clone(),
            audio_port: self.audio_port,
            clock_port: self.clock_port,
            buffer_size_ms: self.buffer_size_ms,
        });
        writer.write_all(&FrameCodec::encode(&ack)).await?;

        info!(
            controller = %hello.controller_name,
            "handshake complete"
        );

        // Spawn clock sync responder
        let clock_sock = self.clock_socket.clone();
        tokio::spawn(async move {
            Self::clock_sync_loop(clock_sock).await;
        });

        // Spawn audio receiver
        let audio_sock = self.audio_socket.clone();
        let audio_tx = self.event_tx.clone();
        tokio::spawn(async move {
            Self::audio_receive_loop(audio_sock, audio_tx).await;
        });

        // Main control message loop
        loop {
            let n = reader.read(&mut read_buf).await?;
            if n == 0 {
                let _ = self.event_tx.send(EndpointEvent::Disconnected).await;
                break;
            }
            codec.feed(&read_buf[..n]);

            while let Some(msg) = codec.decode_next()? {
                match msg {
                    Message::FormatPropose(fp) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::FormatProposed(fp))
                            .await;
                    }
                    Message::Play(p) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Play(p.stream_id)))
                            .await;
                    }
                    Message::Pause(p) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Pause(p.stream_id)))
                            .await;
                    }
                    Message::Stop(s) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Stop(s.stream_id)))
                            .await;
                    }
                    Message::Seek(s) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Playback(PlaybackCommand::Seek(
                                s.stream_id,
                                s.position_ms,
                            )))
                            .await;
                    }
                    Message::Metadata(m) => {
                        let _ = self.event_tx.send(EndpointEvent::Metadata(m)).await;
                    }
                    Message::VolumeSet(v) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Volume(VolumeCommand::Set(v.level)))
                            .await;
                    }
                    Message::VolumeGet(_) => {
                        let _ = self
                            .event_tx
                            .send(EndpointEvent::Volume(VolumeCommand::Get))
                            .await;
                    }
                    Message::Mute(m) => {
                        let _ = self
                            .event_tx
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
            let header_bytes: &[u8; AUDIO_HEADER_SIZE] = buf[..AUDIO_HEADER_SIZE].try_into().unwrap();
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

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
