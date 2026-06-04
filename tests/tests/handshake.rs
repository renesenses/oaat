use std::net::SocketAddr;
use tokio::sync::mpsc;

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse};
use oaat_core::ChannelLayout;
use oaat_core::format::AudioFormat;
use oaat_core::message::EndpointCapabilities;
use oaat_core::wire::PacketFlags;
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
        formats: vec![
            AudioFormat::PcmS16le,
            AudioFormat::PcmS24le,
            AudioFormat::PcmS32le,
        ],
        volume: None,
        gapless: true,
        seek: true,
    }
}

#[tokio::test]
async fn controller_connects_to_endpoint_and_handshakes() {
    init_tracing();

    let control_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let audio_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let clock_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

    // Bind the endpoint first to get actual ports
    let tcp_listener = tokio::net::TcpListener::bind(control_addr).await.unwrap();
    let actual_control = tcp_listener.local_addr().unwrap();
    let udp_audio = tokio::net::UdpSocket::bind(audio_addr).await.unwrap();
    let actual_audio = udp_audio.local_addr().unwrap();
    let udp_clock = tokio::net::UdpSocket::bind(clock_addr).await.unwrap();
    let actual_clock = udp_clock.local_addr().unwrap();

    // Drop the pre-bound sockets — the endpoint transport will bind them.
    // We just needed the port numbers.
    drop(tcp_listener);
    drop(udp_audio);
    drop(udp_clock);

    let ep_config = EndpointConfig {
        endpoint_id: "test-ep-001".into(),
        endpoint_name: "Test Endpoint".into(),
        control_addr: actual_control,
        audio_addr: actual_audio,
        clock_addr: actual_clock,
        capabilities: test_capabilities(),
        buffer_size_ms: 1000,
        tls: false,
    };

    let (event_tx, mut event_rx) = mpsc::channel(32);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    // Start endpoint in background
    let _ep_handle = tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .unwrap();
    });

    // Give the endpoint a moment to start listening
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Connect controller
    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-001".into(),
        controller_name: "Test Controller".into(),
        features: vec!["flac_transport".into()],
        clock_port: 9742,
        tls: false,
    };

    let endpoint = ConnectedEndpoint::connect(&ctrl_config, actual_control)
        .await
        .unwrap();

    // Verify we got the Connected event on the endpoint side
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();

    match event {
        EndpointEvent::Connected {
            controller_id,
            controller_name,
        } => {
            assert_eq!(controller_id, "ctrl-001");
            assert_eq!(controller_name, "Test Controller");
        }
        _ => panic!("expected Connected event"),
    }

    // Verify endpoint info from handshake
    assert_eq!(endpoint.info.endpoint_name, "Test Endpoint");
    assert_eq!(endpoint.info.endpoint_id, "test-ep-001");
    assert_eq!(endpoint.info.capabilities.pcm_max_rate, 192000);
    assert!(endpoint.info.capabilities.gapless);
}

#[tokio::test]
async fn controller_sends_format_propose_and_audio() {
    init_tracing();

    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let actual_control = tcp_listener.local_addr().unwrap();
    let udp_audio = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_audio = udp_audio.local_addr().unwrap();
    let udp_clock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_clock = udp_clock.local_addr().unwrap();
    drop(tcp_listener);
    drop(udp_audio);
    drop(udp_clock);

    let ep_config = EndpointConfig {
        endpoint_id: "test-ep-002".into(),
        endpoint_name: "Audio Test Endpoint".into(),
        control_addr: actual_control,
        audio_addr: actual_audio,
        clock_addr: actual_clock,
        capabilities: test_capabilities(),
        buffer_size_ms: 1000,
        tls: false,
    };

    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-002".into(),
        controller_name: "Audio Controller".into(),
        features: vec![],
        clock_port: 9742,
        tls: false,
    };

    let mut endpoint = ConnectedEndpoint::connect(&ctrl_config, actual_control)
        .await
        .unwrap();

    // Drain Connected event
    let _ = event_rx.recv().await;

    // Send FormatPropose
    endpoint
        .propose_format(
            "stream-1",
            AudioFormat::PcmS24le,
            96000,
            2,
            ChannelLayout::Stereo,
            24,
        )
        .await
        .unwrap();

    // Drain FormatAccepted event (endpoint auto-accepts this format)
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        EndpointEvent::FormatAccepted { stream_id } => {
            assert_eq!(stream_id, "stream-1");
        }
        _ => panic!("expected FormatAccepted event"),
    }

    // Then FormatProposed
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();

    match event {
        EndpointEvent::FormatProposed(fp) => {
            assert_eq!(fp.stream_id, "stream-1");
            assert_eq!(fp.sample_rate, 96000);
            assert_eq!(fp.format, AudioFormat::PcmS24le);
        }
        _ => panic!("expected FormatProposed event"),
    }

    // Send audio packets
    let silence = vec![0u8; 1152]; // 192 stereo 24-bit samples
    for i in 0..10u64 {
        endpoint
            .send_audio(
                1,
                AudioFormat::PcmS24le,
                i * 2_000_000, // 2ms per packet
                i * 192,       // sample offset
                &silence,
                if i == 0 {
                    PacketFlags::FIRST_PACKET
                } else {
                    PacketFlags::empty()
                },
            )
            .await
            .unwrap();
    }

    // Verify at least some audio packets arrived
    let mut audio_count = 0;
    for _ in 0..10 {
        match tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv()).await {
            Ok(Some(EndpointEvent::AudioPacket { header, payload })) => {
                assert_eq!(header.format, AudioFormat::PcmS24le);
                assert_eq!(payload.len(), 1152);
                audio_count += 1;
            }
            _ => break,
        }
    }

    assert!(
        audio_count >= 5,
        "expected at least 5 audio packets, got {audio_count}"
    );
}

#[tokio::test]
async fn clock_sync_bootstrap() {
    init_tracing();

    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let actual_control = tcp_listener.local_addr().unwrap();
    let udp_audio = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_audio = udp_audio.local_addr().unwrap();
    let udp_clock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_clock = udp_clock.local_addr().unwrap();
    drop(tcp_listener);
    drop(udp_audio);
    drop(udp_clock);

    let ep_config = EndpointConfig {
        endpoint_id: "test-ep-003".into(),
        endpoint_name: "Clock Test".into(),
        control_addr: actual_control,
        audio_addr: actual_audio,
        clock_addr: actual_clock,
        capabilities: test_capabilities(),
        buffer_size_ms: 1000,
        tls: false,
    };

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-003".into(),
        controller_name: "Clock Controller".into(),
        features: vec![],
        clock_port: 9742,
        tls: false,
    };

    let mut endpoint = ConnectedEndpoint::connect(&ctrl_config, actual_control)
        .await
        .unwrap();

    // Run bootstrap sync
    endpoint.clock_sync_bootstrap().await.unwrap();

    // On localhost, offset should be very small (< 1ms)
    let offset = endpoint.clock_offset_ns().await;
    assert!(
        offset.abs() < 10_000_000, // 10ms tolerance for CI
        "clock offset should be small on localhost, got {offset}ns"
    );
}

/// Helper: spin up endpoint + connect controller, return (endpoint_handle, event_rx, connected_endpoint).
async fn setup_endpoint_and_controller(
    caps: EndpointCapabilities,
) -> (mpsc::Receiver<EndpointEvent>, ConnectedEndpoint) {
    let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let actual_control = tcp_listener.local_addr().unwrap();
    let udp_audio = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_audio = udp_audio.local_addr().unwrap();
    let udp_clock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let actual_clock = udp_clock.local_addr().unwrap();
    drop(tcp_listener);
    drop(udp_audio);
    drop(udp_clock);

    let ep_config = EndpointConfig {
        endpoint_id: "test-ep-fmt".into(),
        endpoint_name: "Format Test Endpoint".into(),
        control_addr: actual_control,
        audio_addr: actual_audio,
        clock_addr: actual_clock,
        capabilities: caps,
        buffer_size_ms: 1000,
        tls: false,
    };

    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let ctrl_config = ControllerConfig {
        controller_id: "ctrl-fmt".into(),
        controller_name: "Format Controller".into(),
        features: vec![],
        clock_port: 9742,
        tls: false,
    };

    let endpoint = ConnectedEndpoint::connect(&ctrl_config, actual_control)
        .await
        .unwrap();

    // Drain the Connected event
    let _ = event_rx.recv().await;

    (event_rx, endpoint)
}

#[tokio::test]
async fn format_accept_when_supported() {
    init_tracing();

    let (mut event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    // Propose PCM S24LE at 96kHz/24-bit -- within capabilities (max 192kHz/24-bit)
    endpoint
        .propose_format(
            "stream-accept",
            AudioFormat::PcmS24le,
            96000,
            2,
            ChannelLayout::Stereo,
            24,
        )
        .await
        .unwrap();

    // Controller should receive FormatAccept
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    match resp {
        EndpointResponse::FormatAccept(fa) => {
            assert_eq!(fa.stream_id, "stream-accept");
        }
        other => panic!("expected FormatAccept, got {:?}", other),
    }

    // Endpoint should also emit FormatAccepted + FormatProposed events
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::FormatAccepted { stream_id } => {
            assert_eq!(stream_id, "stream-accept");
        }
        _ => panic!("expected FormatAccepted event"),
    }

    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::FormatProposed(fp) => {
            assert_eq!(fp.stream_id, "stream-accept");
        }
        _ => panic!("expected FormatProposed event"),
    }
}

#[tokio::test]
async fn format_counter_when_rate_too_high() {
    init_tracing();

    let (mut event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    // Propose PCM S24LE at 384kHz -- exceeds max 192kHz. Same 48k family.
    endpoint
        .propose_format(
            "stream-counter",
            AudioFormat::PcmS24le,
            384000,
            2,
            ChannelLayout::Stereo,
            24,
        )
        .await
        .unwrap();

    // Controller should receive FormatCounter
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    match resp {
        EndpointResponse::FormatCounter(fc) => {
            assert_eq!(fc.stream_id, "stream-counter");
            assert_eq!(fc.format, AudioFormat::PcmS24le);
            // Should counter with 192000 (highest in 48k family <= 192000)
            assert_eq!(fc.sample_rate, 192000);
            assert_eq!(fc.bits_per_sample, 24);
        }
        other => panic!("expected FormatCounter, got {:?}", other),
    }

    // Endpoint should emit FormatProposed event (no FormatAccepted/FormatRejected for counter)
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::FormatProposed(fp) => {
            assert_eq!(fp.stream_id, "stream-counter");
            assert_eq!(fp.sample_rate, 384000);
        }
        _ => panic!("expected FormatProposed event"),
    }
}

#[tokio::test]
async fn format_reject_when_unsupported() {
    init_tracing();

    let (mut event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    // Propose Flac -- not in test_capabilities().formats (which is S16LE, S24LE, S32LE only)
    endpoint
        .propose_format(
            "stream-reject",
            AudioFormat::Flac,
            44100,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();

    // Controller should receive FormatReject
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    match resp {
        EndpointResponse::FormatReject(fr) => {
            assert_eq!(fr.stream_id, "stream-reject");
            assert!(fr.reason.contains("unsupported format"));
        }
        other => panic!("expected FormatReject, got {:?}", other),
    }

    // Endpoint should emit FormatRejected + FormatProposed events
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::FormatRejected { stream_id, reason } => {
            assert_eq!(stream_id, "stream-reject");
            assert!(reason.contains("unsupported format"));
        }
        _ => panic!("expected FormatRejected event"),
    }

    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::FormatProposed(fp) => {
            assert_eq!(fp.stream_id, "stream-reject");
        }
        _ => panic!("expected FormatProposed event"),
    }
}

#[tokio::test]
async fn gapless_same_format_returns_next_track_ready() {
    init_tracing();

    let (mut event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    // First, establish a format via FormatPropose so the endpoint tracks it
    endpoint
        .propose_format(
            "stream-gapless",
            AudioFormat::PcmS16le,
            44100,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();

    // Drain FormatAccept response on controller side
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    match resp {
        EndpointResponse::FormatAccept(fa) => {
            assert_eq!(fa.stream_id, "stream-gapless");
        }
        other => panic!("expected FormatAccept, got {:?}", other),
    }

    // Drain endpoint-side events (FormatAccepted + FormatProposed)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv()).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv()).await;

    // Now send NextTrackPrepare with the SAME format
    endpoint
        .prepare_next_track(
            "stream-gapless-next",
            AudioFormat::PcmS16le,
            44100,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();

    // Controller should receive NextTrackReady
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    match resp {
        EndpointResponse::NextTrackReady(ntr) => {
            assert_eq!(ntr.stream_id, "stream-gapless-next");
        }
        other => panic!("expected NextTrackReady, got {:?}", other),
    }

    // Endpoint should emit NextTrackReady event
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::NextTrackReady { stream_id } => {
            assert_eq!(stream_id, "stream-gapless-next");
        }
        _ => panic!("expected NextTrackReady event"),
    }
}

#[tokio::test]
async fn gapless_different_format_returns_next_track_reformat() {
    init_tracing();

    let (mut event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    // Establish PCM S16LE at 44100Hz
    endpoint
        .propose_format(
            "stream-reformat",
            AudioFormat::PcmS16le,
            44100,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();

    // Drain FormatAccept response on controller side
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    match resp {
        EndpointResponse::FormatAccept(fa) => {
            assert_eq!(fa.stream_id, "stream-reformat");
        }
        other => panic!("expected FormatAccept, got {:?}", other),
    }

    // Drain endpoint-side events (FormatAccepted + FormatProposed)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv()).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv()).await;

    // Now send NextTrackPrepare with a DIFFERENT sample rate (96000 instead of 44100)
    endpoint
        .prepare_next_track(
            "stream-reformat-next",
            AudioFormat::PcmS16le,
            96000,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();

    // Controller should receive NextTrackReformat
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    match resp {
        EndpointResponse::NextTrackReformat(ntf) => {
            assert_eq!(ntf.stream_id, "stream-reformat-next");
            assert_eq!(ntf.format, AudioFormat::PcmS16le);
            assert_eq!(ntf.sample_rate, 96000);
        }
        other => panic!("expected NextTrackReformat, got {:?}", other),
    }

    // Endpoint should emit NextTrackReformat event
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match ev {
        EndpointEvent::NextTrackReformat {
            stream_id,
            format,
            sample_rate,
        } => {
            assert_eq!(stream_id, "stream-reformat-next");
            assert_eq!(format, AudioFormat::PcmS16le);
            assert_eq!(sample_rate, 96000);
        }
        _ => panic!("expected NextTrackReformat event"),
    }
}

#[tokio::test]
async fn dsd_format_accepted_when_supported() {
    init_tracing();

    let dsd_caps = EndpointCapabilities {
        pcm_max_rate: 192000,
        pcm_max_bits: 24,
        dsd_max_rate: Some(64),
        channels_max: 2,
        formats: vec![
            AudioFormat::PcmS16le,
            AudioFormat::PcmS24le,
            AudioFormat::DsdU8,
        ],
        volume: None,
        gapless: true,
        seek: true,
    };

    let (_event_rx, mut endpoint) = setup_endpoint_and_controller(dsd_caps).await;

    endpoint
        .propose_format(
            "stream-dsd-accept",
            AudioFormat::DsdU8,
            2822400,
            2,
            ChannelLayout::Stereo,
            1,
        )
        .await
        .unwrap();

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    match resp {
        EndpointResponse::FormatAccept(fa) => {
            assert_eq!(fa.stream_id, "stream-dsd-accept");
        }
        other => panic!("expected FormatAccept for DSD, got {:?}", other),
    }
}

#[tokio::test]
async fn dsd_format_rejected_when_not_supported() {
    init_tracing();

    let (_event_rx, mut endpoint) = setup_endpoint_and_controller(test_capabilities()).await;

    endpoint
        .propose_format(
            "stream-dsd-reject",
            AudioFormat::DsdU8,
            2822400,
            2,
            ChannelLayout::Stereo,
            1,
        )
        .await
        .unwrap();

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        endpoint.response_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    match resp {
        EndpointResponse::FormatReject(fr) => {
            assert_eq!(fr.stream_id, "stream-dsd-reject");
            assert!(fr.reason.contains("unsupported format"));
        }
        other => panic!("expected FormatReject for DSD, got {:?}", other),
    }
}

#[tokio::test]
async fn multiroom_zone_streams_to_two_endpoints() {
    init_tracing();
    use oaat_controller::Zone;

    // Start two endpoints
    let mut ep_addrs = Vec::new();
    let mut ep_rxs = Vec::new();
    for i in 0..2 {
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control = tcp.local_addr().unwrap();
        let audio = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let clock = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        drop(tcp);

        let ep_config = EndpointConfig {
            endpoint_id: format!("ep-zone-{i}"),
            endpoint_name: format!("Zone Endpoint {i}"),
            control_addr: control,
            audio_addr: audio,
            clock_addr: clock,
            capabilities: test_capabilities(),
            buffer_size_ms: 1000,
            tls: false,
        };

        let (event_tx, event_rx) = mpsc::channel(256);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

        tokio::spawn(async move {
            EndpointTransport::run(ep_config, event_tx, ctrl_rx)
                .await
                .ok();
        });

        ep_addrs.push(control);
        ep_rxs.push(event_rx);
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Create zone and add both endpoints
    let config = ControllerConfig {
        controller_id: "ctrl-zone".into(),
        controller_name: "Zone Controller".into(),
        features: vec![],
        clock_port: 9742,
        tls: false,
    };
    let mut zone = Zone::new("zone-test".into(), "Test Zone".into(), config);

    for addr in &ep_addrs {
        zone.add_endpoint(*addr).await.unwrap();
    }
    assert_eq!(zone.endpoint_count(), 2);
    assert!(zone.is_multiroom());

    // Drain Connected events from both endpoints
    for rx in &mut ep_rxs {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    }

    // Propose format to all
    zone.propose_format_all(
        "zone-stream",
        AudioFormat::PcmS16le,
        44100,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .unwrap();

    // Both endpoints should get FormatAccepted (may be preceded by zone events)
    for rx in &mut ep_rxs {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            match event {
                EndpointEvent::FormatAccepted { stream_id } => {
                    assert_eq!(stream_id, "zone-stream");
                    break;
                }
                EndpointEvent::ZoneAssigned { .. }
                | EndpointEvent::ZoneUpdated { .. }
                | EndpointEvent::FormatProposed(_) => continue,
                other => panic!("expected FormatAccepted, got {other:?}"),
            }
        }
    }

    // Play all
    zone.play_all("zone-stream").await.unwrap();

    // Send 5 audio packets to all
    for i in 0..5u64 {
        let payload = vec![0u8; 960]; // 240 stereo 16-bit samples
        let flags = if i == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };
        zone.send_audio_all(
            1,
            AudioFormat::PcmS16le,
            i * 5_000_000,
            i * 240,
            &payload,
            flags,
        )
        .await
        .unwrap();
    }

    // Both endpoints should receive audio packets
    for (idx, rx) in ep_rxs.iter_mut().enumerate() {
        // Drain FormatProposed and Play events first
        let mut audio_count = 0;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await {
                Ok(Some(EndpointEvent::AudioPacket { .. })) => audio_count += 1,
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        assert!(
            audio_count >= 3,
            "endpoint {idx} got {audio_count} audio packets, expected >= 3"
        );
    }

    zone.stop_all("zone-stream").await.unwrap();
}

// ---------------------------------------------------------------------------
// Multi-device tests
// ---------------------------------------------------------------------------

async fn spawn_test_endpoint(index: usize) -> (SocketAddr, mpsc::Receiver<EndpointEvent>) {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control = tcp.local_addr().unwrap();
    let audio: SocketAddr = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let clock: SocketAddr = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    drop(tcp);

    let ep_config = EndpointConfig {
        endpoint_id: format!("ep-multi-{index}"),
        endpoint_name: format!("Multi Endpoint {index}"),
        control_addr: control,
        audio_addr: audio,
        clock_addr: clock,
        capabilities: test_capabilities(),
        buffer_size_ms: 1000,
        tls: false,
    };

    let (event_tx, event_rx) = mpsc::channel(256);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (control, event_rx)
}

fn test_controller_config() -> ControllerConfig {
    ControllerConfig {
        controller_id: "ctrl-multi".into(),
        controller_name: "Multi Controller".into(),
        features: vec![],
        clock_port: 9742,
        tls: false,
    }
}

/// Drain events until we find one matching the predicate, or timeout.
async fn drain_until<F>(rx: &mut mpsc::Receiver<EndpointEvent>, pred: F) -> Option<EndpointEvent>
where
    F: Fn(&EndpointEvent) -> bool,
{
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(event)) if pred(&event) => return Some(event),
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
    None
}

#[tokio::test]
async fn zone_manager_create_and_dissolve() {
    init_tracing();
    use oaat_controller::ZoneManager;

    let mut mgr = ZoneManager::new(test_controller_config());
    let mut events = mgr.subscribe();

    mgr.create_zone("z1".into(), "Living Room".into());
    mgr.create_zone("z2".into(), "Kitchen".into());

    assert_eq!(mgr.zone_ids().len(), 2);

    let snap = mgr.snapshot();
    assert_eq!(snap.len(), 2);

    mgr.dissolve_zone("z1");
    assert_eq!(mgr.zone_ids().len(), 1);

    // Verify events
    let e1 = events.recv().await.unwrap();
    assert!(matches!(e1, oaat_controller::ZoneEvent::ZoneCreated { ref zone_id, .. } if zone_id == "z1"));
    let e2 = events.recv().await.unwrap();
    assert!(matches!(e2, oaat_controller::ZoneEvent::ZoneCreated { ref zone_id, .. } if zone_id == "z2"));
    let e3 = events.recv().await.unwrap();
    assert!(matches!(e3, oaat_controller::ZoneEvent::ZoneDissolved { ref zone_id } if zone_id == "z1"));
}

#[tokio::test]
async fn zone_manager_add_endpoint_emits_event() {
    init_tracing();
    use oaat_controller::ZoneManager;

    let (addr, mut ep_rx) = spawn_test_endpoint(10).await;
    let mut mgr = ZoneManager::new(test_controller_config());
    let mut events = mgr.subscribe();

    mgr.create_zone("z1".into(), "Zone".into());
    let ep_id = mgr.add_endpoint_to_zone("z1", addr).await.unwrap();
    assert!(!ep_id.is_empty());

    // Should have ZoneCreated + EndpointJoined
    let _ = events.recv().await; // ZoneCreated
    let e = events.recv().await.unwrap();
    assert!(matches!(e, oaat_controller::ZoneEvent::EndpointJoined { ref zone_id, .. } if zone_id == "z1"));

    // Endpoint should have received ZoneAssigned
    let ev = drain_until(&mut ep_rx, |e| matches!(e, EndpointEvent::ZoneAssigned { .. })).await;
    assert!(ev.is_some());

    mgr.dissolve_zone("z1");
}

#[tokio::test]
async fn per_device_volume() {
    init_tracing();

    let (addr0, _rx0) = spawn_test_endpoint(20).await;
    let (addr1, _rx1) = spawn_test_endpoint(21).await;

    let mut zone = oaat_controller::Zone::new(
        "z-vol".into(),
        "Volume Zone".into(),
        test_controller_config(),
    );

    let ep0 = zone.add_endpoint(addr0).await.unwrap();
    let ep1 = zone.add_endpoint(addr1).await.unwrap();

    // Set master volume
    zone.set_volume_all(80).await.unwrap();
    assert_eq!(zone.volume_map().master, 80);

    // Set per-device offset for endpoint 1
    zone.set_volume_offset(&ep1, -20).await.unwrap();

    // Check effective volumes
    assert_eq!(zone.volume_map().effective_volume(&ep0), 80); // no offset
    assert_eq!(zone.volume_map().effective_volume(&ep1), 60); // 80 + (-20)

    // Set absolute level for endpoint 0
    zone.set_volume_endpoint(&ep0, 50).await.unwrap();
    assert_eq!(zone.volume_map().effective_volume(&ep0), 50);
    // offset should be 50 - 80 = -30
    assert_eq!(zone.volume_map().offset(&ep0), -30);
}

#[tokio::test]
async fn late_join_catches_up_to_active_stream() {
    init_tracing();

    let (addr0, mut _rx0) = spawn_test_endpoint(30).await;
    let (addr1, mut rx1) = spawn_test_endpoint(31).await;

    let mut zone = oaat_controller::Zone::new(
        "z-late".into(),
        "Late Join Zone".into(),
        test_controller_config(),
    );

    // Add first endpoint and start streaming
    let _ep0 = zone.add_endpoint(addr0).await.unwrap();
    zone.propose_format_all("stream-1", AudioFormat::PcmS16le, 44100, 2, ChannelLayout::Stereo, 16)
        .await
        .unwrap();

    zone.send_metadata_all(oaat_core::message::TrackMetadata {
        title: "Test".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_ms: 60000,
        artwork_url: None,
        format: None,
    })
    .await
    .unwrap();

    zone.play_all("stream-1").await.unwrap();

    // Send a few audio packets
    for i in 0..3u64 {
        let payload = vec![0u8; 960];
        zone.send_audio_all(1, AudioFormat::PcmS16le, i * 5_000_000, i * 240, &payload, PacketFlags::empty())
            .await
            .unwrap();
    }

    // Now late-join the second endpoint
    let _ep1 = zone.join_active(addr1).await.unwrap();

    // The second endpoint should receive ZoneAssigned, FormatProposed/Accepted, Metadata, and Play
    let got_zone = drain_until(&mut rx1, |e| matches!(e, EndpointEvent::ZoneAssigned { .. })).await;
    assert!(got_zone.is_some(), "late-join endpoint should get ZoneAssigned");

    let got_format = drain_until(&mut rx1, |e| matches!(e, EndpointEvent::FormatAccepted { .. })).await;
    assert!(got_format.is_some(), "late-join endpoint should get FormatAccepted");

    let got_meta = drain_until(&mut rx1, |e| matches!(e, EndpointEvent::Metadata(_))).await;
    assert!(got_meta.is_some(), "late-join endpoint should get Metadata");

    let got_play = drain_until(&mut rx1, |e| matches!(e, EndpointEvent::Playback(oaat_endpoint::transport::PlaybackCommand::Play(_)))).await;
    assert!(got_play.is_some(), "late-join endpoint should get Play");

    // Verify the late-join endpoint also receives subsequent audio packets
    for i in 3..6u64 {
        let payload = vec![0u8; 960];
        zone.send_audio_all(1, AudioFormat::PcmS16le, i * 5_000_000, i * 240, &payload, PacketFlags::empty())
            .await
            .unwrap();
    }

    let got_audio = drain_until(&mut rx1, |e| matches!(e, EndpointEvent::AudioPacket { .. })).await;
    assert!(got_audio.is_some(), "late-join endpoint should receive audio packets");

    zone.stop_all("stream-1").await.unwrap();
}

#[tokio::test]
async fn remove_endpoint_sends_zone_release() {
    init_tracing();

    let (addr0, mut rx0) = spawn_test_endpoint(40).await;
    let (addr1, mut rx1) = spawn_test_endpoint(41).await;

    let mut zone = oaat_controller::Zone::new(
        "z-remove".into(),
        "Remove Zone".into(),
        test_controller_config(),
    );

    let ep0 = zone.add_endpoint(addr0).await.unwrap();
    let _ep1 = zone.add_endpoint(addr1).await.unwrap();
    assert_eq!(zone.endpoint_count(), 2);

    // Remove endpoint 0 with notification
    zone.remove_endpoint_and_notify(&ep0).await;
    assert_eq!(zone.endpoint_count(), 1);

    // Endpoint 1 should get a ZoneUpdate with 1 member remaining
    // (skip earlier ZoneUpdated events from add_endpoint broadcasts)
    let got_update = drain_until(&mut rx1, |e| {
        matches!(e, EndpointEvent::ZoneUpdated { endpoint_ids, .. } if endpoint_ids.len() == 1)
    })
    .await;
    assert!(got_update.is_some(), "remaining endpoint should get ZoneUpdate with 1 member after removal");

    // Endpoint 0 should get ZoneRelease (sent fire-and-forget on remove)
    let got_release = drain_until(&mut rx0, |e| matches!(e, EndpointEvent::ZoneReleased { .. })).await;
    assert!(got_release.is_some(), "removed endpoint should get ZoneRelease");
}
