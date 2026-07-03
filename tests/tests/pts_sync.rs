//! Integration tests for PTS-based playback support:
//! - FEC parity packets must never surface as audio
//! - endpoint-initiated clock sync against the controller's ClockResponder

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

use oaat_controller::{ClockResponder, ConnectedEndpoint, ControllerConfig};
use oaat_core::format::AudioFormat;
use oaat_core::message::EndpointCapabilities;
use oaat_core::wire::PacketFlags;
use oaat_endpoint::sync::SharedClock;
use oaat_endpoint::{EndpointConfig, EndpointEvent, EndpointTransport};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("oaat=debug")
        .try_init();
}

fn test_capabilities() -> EndpointCapabilities {
    EndpointCapabilities {
        pcm_max_rate: 192000,
        pcm_max_bits: 24,
        dsd_max_rate: None,
        channels_max: 2,
        formats: vec![AudioFormat::PcmS16le, AudioFormat::PcmS24le],
        volume: None,
        gapless: true,
        seek: true,
    }
}

/// Bind then drop sockets to reserve real port numbers for the endpoint.
async fn reserve_endpoint_ports() -> (SocketAddr, SocketAddr, SocketAddr) {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control = tcp.local_addr().unwrap();
    let udp_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let audio = udp_a.local_addr().unwrap();
    let udp_c = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let clock = udp_c.local_addr().unwrap();
    drop(tcp);
    drop(udp_a);
    drop(udp_c);
    (control, audio, clock)
}

fn endpoint_config(
    control: SocketAddr,
    audio: SocketAddr,
    clock: SocketAddr,
) -> EndpointConfig {
    EndpointConfig {
        endpoint_id: "test-ep-pts".into(),
        endpoint_name: "PTS Test Endpoint".into(),
        control_addr: control,
        audio_addr: audio,
        clock_addr: clock,
        capabilities: test_capabilities(),
        buffer_size_ms: 1000,
        tls: false,
        defer_format_accept: false,
    }
}

#[tokio::test]
async fn fec_parity_packets_never_reach_the_audio_path() {
    init_tracing();

    let (control, audio, clock) = reserve_endpoint_ports().await;
    let ep_config = endpoint_config(control, audio, clock);

    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);
    let _ep = tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-fec".into(),
        controller_name: "FEC Test Controller".into(),
        features: vec![],
        clock_port: 0,
        tls: false,
    };
    let mut endpoint = ConnectedEndpoint::connect(&ctrl_config, control).await.unwrap();

    // Drain the Connected event.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();

    // Data packet → parity packet → data packet.
    let data = vec![0x11u8; 320];
    let parity = vec![0xEEu8; 320];
    endpoint
        .send_audio(1, AudioFormat::PcmS16le, 1_000, 0, &data, PacketFlags::FIRST_PACKET)
        .await
        .unwrap();
    endpoint
        .send_audio(1, AudioFormat::PcmS16le, 1_000, 0, &parity, PacketFlags::FEC)
        .await
        .unwrap();
    endpoint
        .send_audio(1, AudioFormat::PcmS16le, 2_000, 80, &data, PacketFlags::empty())
        .await
        .unwrap();

    // Both data packets must arrive; the parity packet must be filtered.
    let mut audio_packets = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while audio_packets.len() < 2 {
        let event = tokio::time::timeout_at(deadline, event_rx.recv())
            .await
            .expect("timed out waiting for audio packets")
            .unwrap();
        if let EndpointEvent::AudioPacket { header, payload } = event {
            assert!(
                !header.flags.contains(PacketFlags::FEC),
                "FEC parity packet leaked into the audio path"
            );
            assert_eq!(payload, data);
            audio_packets.push(header.sequence);
        }
    }

    // Grace period: verify no third (parity) packet trickles in.
    let extra = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        loop {
            if let Some(EndpointEvent::AudioPacket { header, .. }) = event_rx.recv().await {
                return header;
            }
        }
    })
    .await;
    assert!(extra.is_err(), "unexpected extra audio packet: {extra:?}");
}

#[tokio::test]
async fn endpoint_clock_bootstraps_against_controller_responder() {
    init_tracing();

    // Controller-side clock responder (the piece announced in Hello).
    let (responder_port, _responder) =
        ClockResponder::spawn("127.0.0.1:0".parse().unwrap()).await.unwrap();

    let (control, audio, clock_addr) = reserve_endpoint_ports().await;
    let ep_config = endpoint_config(control, audio, clock_addr);

    let shared_clock = Arc::new(SharedClock::new());
    let ep_clock = shared_clock.clone();
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);
    let _ep = tokio::spawn(async move {
        EndpointTransport::run_with_clock(ep_config, event_tx, ctrl_rx, ep_clock)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Hello announces the responder port; the endpoint's clock client
    // starts its own exchanges right after the handshake.
    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-clock".into(),
        controller_name: "Clock Test Controller".into(),
        features: vec![],
        clock_port: responder_port,
        tls: false,
    };
    let _endpoint = ConnectedEndpoint::connect(&ctrl_config, control).await.unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();

    // Bootstrap: 10 exchanges at the 100 ms bootstrap cadence → ≤ ~1.5 s.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !shared_clock.is_bootstrapped() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "clock never bootstrapped (samples so far: offset={} rtt={})",
            shared_clock.offset_ns(),
            shared_clock.rtt_ns()
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Same machine, same clock: offset must be tiny, RTT sane.
    let offset = shared_clock.offset_ns();
    assert!(
        offset.abs() < 10_000_000,
        "localhost clock offset should be < 10ms, got {offset}ns"
    );

    // Domain conversions must round-trip through the published offset.
    let local = 1_000_000_000_000u64;
    let there_and_back = shared_clock.controller_to_local(shared_clock.local_to_controller(local));
    assert_eq!(there_and_back, local);
}
