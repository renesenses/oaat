use std::net::SocketAddr;
use tokio::sync::mpsc;

use oaat_core::format::AudioFormat;
use oaat_core::message::EndpointCapabilities;
use oaat_core::wire::PacketFlags;
use oaat_core::ChannelLayout;
use oaat_controller::{ConnectedEndpoint, ControllerConfig};
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
                i * 2_000_000,       // 2ms per packet
                i * 192,             // sample offset
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

    assert!(audio_count >= 5, "expected at least 5 audio packets, got {audio_count}");
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
