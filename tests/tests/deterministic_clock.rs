use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oaat_controller::{ControllerConfig, TimeSource, Zone};
use oaat_core::codec::FrameCodec;
use oaat_core::format::AudioFormat;
use oaat_core::message::{EndpointCapabilities, HelloAck};
use oaat_core::wire::{AUDIO_HEADER_SIZE, AudioPacketHeader, ClockSyncPacket, ClockSyncType};
use oaat_core::{Message, PROTOCOL_VERSION, PacketFlags};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;

/// One logical second between exchanges, but only 2 ms between t1 and t4.
/// This keeps RTT realistic while making ±100 ppm drift observable without
/// sleeping for minutes.
#[derive(Default)]
struct ExchangeClock {
    calls: AtomicU64,
}

impl TimeSource for ExchangeClock {
    fn now_ns(&self) -> u64 {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let exchange = call / 2;
        let phase = call % 2;
        1_000_000_000_000 + exchange * 1_000_000_000 + phase * 2_000_000
    }
}

#[derive(Clone)]
enum ClockBehaviour {
    Measured {
        offset_ns: i64,
        drift_ppm: i64,
        jitter_ns: Arc<[i64]>,
    },
    Unmeasured,
}

struct SimulatedEndpoint {
    control_addr: SocketAddr,
    audio_rx: mpsc::Receiver<AudioPacketHeader>,
}

fn shifted(timestamp: u64, delta: i64) -> u64 {
    (timestamp as i128 + delta as i128).clamp(0, u64::MAX as i128) as u64
}

async fn spawn_simulated_endpoint(
    endpoint_id: &str,
    behaviour: ClockBehaviour,
) -> SimulatedEndpoint {
    let control = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control.local_addr().unwrap();
    let audio = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let audio_port = audio.local_addr().unwrap().port();
    let clock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let clock_port = clock.local_addr().unwrap().port();

    let id = endpoint_id.to_owned();
    tokio::spawn(async move {
        let (mut stream, _) = control.accept().await.unwrap();
        let mut codec = FrameCodec::new();
        let mut read_buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut read_buf).await.unwrap();
            assert!(n > 0, "controller closed before Hello");
            codec.feed(&read_buf[..n]);
            if matches!(codec.decode_next().unwrap(), Some(Message::Hello(_))) {
                break;
            }
        }

        let ack = Message::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            endpoint_id: id.clone(),
            endpoint_name: format!("Simulated {id}"),
            capabilities: EndpointCapabilities {
                pcm_max_rate: 192_000,
                pcm_max_bits: 24,
                dsd_max_rate: None,
                channels_max: 2,
                formats: vec![AudioFormat::PcmS16le],
                volume: None,
                gapless: true,
                seek: true,
            },
            audio_port,
            clock_port,
            buffer_size_ms: 1_000,
        });
        stream.write_all(&FrameCodec::encode(&ack)).await.unwrap();

        // Keep the real control connection alive and consume ZoneAssign /
        // ZoneUpdate messages. No format negotiation is needed for the PTS
        // fan-out exercised below.
        loop {
            match stream.read(&mut read_buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => codec.feed(&read_buf[..n]),
            }
        }
    });

    tokio::spawn(async move {
        let mut buf = [0u8; ClockSyncPacket::SIZE];
        let mut sample = 0u64;
        let mut first_t1 = None;
        loop {
            let (n, peer) = clock.recv_from(&mut buf).await.unwrap();
            if n < ClockSyncPacket::SIZE {
                continue;
            }
            let request = ClockSyncPacket::decode(&buf).unwrap();
            if request.kind != ClockSyncType::Request {
                continue;
            }

            let response = match &behaviour {
                ClockBehaviour::Unmeasured => [0u8; ClockSyncPacket::SIZE],
                ClockBehaviour::Measured {
                    offset_ns,
                    drift_ppm,
                    jitter_ns,
                } => {
                    let origin = *first_t1.get_or_insert(request.t1);
                    let elapsed_ns = request.t1.saturating_sub(origin);
                    let drift_ns = (elapsed_ns as i128 * *drift_ppm as i128 / 1_000_000)
                        .clamp(i64::MIN as i128, i64::MAX as i128)
                        as i64;
                    let jitter = jitter_ns[sample as usize % jitter_ns.len()];
                    let measured_offset = offset_ns.saturating_add(drift_ns).saturating_add(jitter);
                    // The injected controller clock makes t4 exactly 2 ms
                    // after t1. Symmetric 1 ms transit then yields precisely
                    // measured_offset in the PTP formula.
                    let t2 = shifted(request.t1, 1_000_000 + measured_offset);
                    let packet = ClockSyncPacket {
                        version: 1,
                        kind: ClockSyncType::Response,
                        sequence: request.sequence,
                        t1: request.t1,
                        t2,
                        t3: t2,
                    };
                    let mut encoded = [0u8; ClockSyncPacket::SIZE];
                    packet.encode(&mut encoded);
                    encoded
                }
            };
            sample += 1;
            clock.send_to(&response, peer).await.unwrap();
        }
    });

    let (audio_tx, audio_rx) = mpsc::channel(8);
    tokio::spawn(async move {
        let mut buf = vec![0u8; AUDIO_HEADER_SIZE + 1_500];
        loop {
            let n = audio.recv(&mut buf).await.unwrap();
            if n < AUDIO_HEADER_SIZE {
                continue;
            }
            let header_bytes: &[u8; AUDIO_HEADER_SIZE] =
                buf[..AUDIO_HEADER_SIZE].try_into().unwrap();
            let header = AudioPacketHeader::decode(header_bytes).unwrap();
            if audio_tx.send(header).await.is_err() {
                break;
            }
        }
    });

    SimulatedEndpoint {
        control_addr,
        audio_rx,
    }
}

fn controller_config() -> ControllerConfig {
    ControllerConfig {
        controller_id: "deterministic-controller".into(),
        controller_name: "Deterministic controller".into(),
        features: vec![],
        clock_port: 0,
        tls: false,
    }
}

async fn next_header(rx: &mut mpsc::Receiver<AudioPacketHeader>) -> AudioPacketHeader {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("audio packet timeout")
        .expect("audio endpoint stopped")
}

#[tokio::test]
async fn deux_renderers_mesures_recoivent_la_meme_timeline_pts() {
    let injected_clock = Arc::new(ExchangeClock::default());
    let no_jitter: Arc<[i64]> = Arc::from([0]);
    let mut ahead = spawn_simulated_endpoint(
        "ahead",
        ClockBehaviour::Measured {
            offset_ns: 50_000_000,
            drift_ppm: 100,
            jitter_ns: no_jitter.clone(),
        },
    )
    .await;
    let mut behind = spawn_simulated_endpoint(
        "behind",
        ClockBehaviour::Measured {
            offset_ns: -50_000_000,
            drift_ppm: -100,
            jitter_ns: no_jitter,
        },
    )
    .await;

    let mut zone = Zone::new_with_time_source(
        "deterministic-zone".into(),
        "Deterministic zone".into(),
        controller_config(),
        injected_clock.clone(),
    );
    zone.add_endpoint(ahead.control_addr).await.unwrap();
    zone.add_endpoint(behind.control_addr).await.unwrap();

    let initial = zone.endpoint_clock_snapshots().await;
    assert_eq!(initial.len(), 2);
    let ahead_clock = initial
        .iter()
        .find(|s| s.endpoint_id == "ahead")
        .unwrap()
        .measurement
        .unwrap();
    let behind_clock = initial
        .iter()
        .find(|s| s.endpoint_id == "behind")
        .unwrap()
        .measurement
        .unwrap();
    assert!(ahead_clock.bootstrapped && behind_clock.bootstrapped);
    assert!(ahead_clock.offset_ns > 40_000_000);
    assert!(behind_clock.offset_ns < -40_000_000);
    assert!(
        injected_clock.calls.load(Ordering::SeqCst) >= 40,
        "bootstrap did not use the injected source for t1/t4"
    );
    let calls_after_bootstrap = injected_clock.calls.load(Ordering::SeqCst);

    let first_pts = 4_000_000_000_000u64;
    let packet_delta = 10_000_000u64;
    let payload = [0x5au8; 96];
    zone.send_audio_all(
        7,
        AudioFormat::PcmS16le,
        first_pts,
        0,
        &payload,
        PacketFlags::FIRST_PACKET,
    )
    .await
    .unwrap();
    zone.send_audio_all(
        7,
        AudioFormat::PcmS16le,
        first_pts + packet_delta,
        480,
        &payload,
        PacketFlags::empty(),
    )
    .await
    .unwrap();

    let ahead_headers = [
        next_header(&mut ahead.audio_rx).await,
        next_header(&mut ahead.audio_rx).await,
    ];
    let behind_headers = [
        next_header(&mut behind.audio_rx).await,
        next_header(&mut behind.audio_rx).await,
    ];
    for headers in [&ahead_headers, &behind_headers] {
        assert!(headers[0].pts_ns < headers[1].pts_ns);
        assert_eq!(headers[1].pts_ns - headers[0].pts_ns, packet_delta);
        assert_eq!(headers[1].sample_offset - headers[0].sample_offset, 480);
    }
    assert_eq!(
        ahead_headers.map(|h| h.pts_ns),
        behind_headers.map(|h| h.pts_ns)
    );

    // Exercise the real steady-state task too. The injected source advances
    // one logical second per exchange, so ±100 ppm must move the estimates in
    // opposite directions after another sample.
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    let steady = zone.endpoint_clock_snapshots().await;
    let ahead_steady = steady
        .iter()
        .find(|s| s.endpoint_id == "ahead")
        .unwrap()
        .measurement
        .unwrap();
    let behind_steady = steady
        .iter()
        .find(|s| s.endpoint_id == "behind")
        .unwrap()
        .measurement
        .unwrap();
    assert!(ahead_steady.samples > ahead_clock.samples);
    assert!(behind_steady.samples > behind_clock.samples);
    assert!(
        injected_clock.calls.load(Ordering::SeqCst) >= calls_after_bootstrap + 4,
        "steady sync did not use the injected source for both endpoints"
    );
    assert!(ahead_steady.offset_ns > ahead_clock.offset_ns);
    assert!(behind_steady.offset_ns < behind_clock.offset_ns);
}

#[tokio::test]
async fn endpoint_sans_mesure_reste_explicitement_unmeasured() {
    let endpoint = spawn_simulated_endpoint("unmeasured", ClockBehaviour::Unmeasured).await;
    let mut zone = Zone::new_with_time_source(
        "unmeasured-zone".into(),
        "Unmeasured zone".into(),
        controller_config(),
        Arc::new(ExchangeClock::default()),
    );

    zone.add_endpoint(endpoint.control_addr).await.unwrap();
    let snapshots = zone.endpoint_clock_snapshots().await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].endpoint_id, "unmeasured");
    assert_eq!(snapshots[0].measurement, None);
}

#[tokio::test]
async fn jitter_scripté_est_visible_dans_le_snapshot() {
    let endpoint = spawn_simulated_endpoint(
        "jittered",
        ClockBehaviour::Measured {
            offset_ns: 0,
            drift_ppm: 0,
            jitter_ns: Arc::from([0, 1_000_000, -1_000_000, 500_000, -500_000]),
        },
    )
    .await;
    let mut zone = Zone::new_with_time_source(
        "jitter-zone".into(),
        "Jitter zone".into(),
        controller_config(),
        Arc::new(ExchangeClock::default()),
    );
    zone.add_endpoint(endpoint.control_addr).await.unwrap();

    let measurement = zone.endpoint_clock_snapshots().await[0]
        .measurement
        .expect("valid scripted exchanges must produce a measurement");
    assert!(measurement.bootstrapped);
    assert!(
        measurement.jitter_ns > 250_000,
        "scripted jitter disappeared from the measurement: {measurement:?}"
    );
}
