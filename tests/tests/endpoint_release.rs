//! endpoint_release (RFC §7.3.1): the endpoint gives up its output and tells
//! the controller, but only a controller that announced it handles it.

use std::time::Duration;

use tokio::sync::mpsc;

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse};
use oaat_core::Message;
use oaat_core::format::AudioFormat;
use oaat_core::message::{
    EndpointCapabilities, EndpointRelease, FEATURE_ENDPOINT_RELEASE, ReleaseReason, StreamStats,
};
use oaat_endpoint::{EndpointConfig, EndpointEvent, EndpointTransport};

fn capabilities() -> EndpointCapabilities {
    EndpointCapabilities {
        pcm_max_rate: 192000,
        pcm_max_bits: 24,
        dsd_max_rate: None,
        channels_max: 2,
        formats: vec![AudioFormat::PcmS16le],
        volume: None,
        gapless: true,
        seek: true,
    }
}

/// An endpoint transport plus a controller connected to it with `features`.
/// Returns the endpoint's application channel (the side that sends the
/// release) and the controller.
async fn connect(features: Vec<String>) -> (mpsc::Sender<Message>, ConnectedEndpoint) {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = tcp.local_addr().unwrap();
    drop(tcp);

    let ep_config = EndpointConfig {
        endpoint_id: "release-ep".into(),
        endpoint_name: "Release Test Endpoint".into(),
        control_addr,
        audio_addr: "127.0.0.1:0".parse().unwrap(),
        clock_addr: "127.0.0.1:0".parse().unwrap(),
        capabilities: capabilities(),
        buffer_size_ms: 1000,
        tls: false,
        defer_format_accept: false,
    };
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (ctrl_tx, ctrl_rx) = mpsc::channel(32);
    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ctrl_config = ControllerConfig {
        controller_id: "release-ctrl".into(),
        controller_name: "Release Controller".into(),
        features,
        clock_port: 9742,
        tls: false,
    };
    let endpoint = ConnectedEndpoint::connect(&ctrl_config, control_addr)
        .await
        .unwrap();
    assert!(matches!(
        event_rx.recv().await,
        Some(EndpointEvent::Connected { .. })
    ));
    // keep the event channel open for the transport
    tokio::spawn(async move { while event_rx.recv().await.is_some() {} });

    (ctrl_tx, endpoint)
}

fn release() -> Message {
    Message::EndpointRelease(EndpointRelease {
        stream_id: "s1".into(),
        reason: ReleaseReason::LocalSource,
    })
}

fn stats() -> Message {
    Message::StreamStats(StreamStats {
        stream_id: "s1".into(),
        buffer_frames: 0,
        drift_us: 0,
        corrections_net_frames: 0,
        packets_lost: 0,
        packets_recovered: 0,
        bit_perfect: true,
        underruns: 0,
    })
}

#[tokio::test]
async fn endpoint_announces_release_in_hello_ack() {
    let (_ctrl_tx, endpoint) = connect(vec![]).await;
    assert!(
        endpoint
            .info
            .features
            .iter()
            .any(|f| f == FEATURE_ENDPOINT_RELEASE)
    );
}

#[tokio::test]
async fn release_reaches_a_controller_that_handles_it() {
    let (ctrl_tx, mut endpoint) = connect(vec![FEATURE_ENDPOINT_RELEASE.into()]).await;
    ctrl_tx.send(release()).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(2), endpoint.release_rx.recv())
        .await
        .expect("no release from the endpoint")
        .expect("release channel closed");
    assert_eq!(r.stream_id, "s1");
    assert_eq!(r.reason, ReleaseReason::LocalSource);
    // a release is not a reply: response_rx stays empty
    assert!(endpoint.response_rx.try_recv().is_err());
}

/// A controller that didn't announce the feature never sees the message; the
/// next message on the control channel arrives as usual.
#[tokio::test]
async fn release_is_withheld_from_an_older_controller() {
    let (ctrl_tx, mut endpoint) = connect(vec![]).await;
    ctrl_tx.send(release()).await.unwrap();
    ctrl_tx.send(stats()).await.unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(2), endpoint.response_rx.recv())
        .await
        .expect("no response from the endpoint")
        .expect("response channel closed");
    assert!(
        matches!(resp, EndpointResponse::StreamStats(_)),
        "expected StreamStats, got {resp:?}"
    );
    assert!(endpoint.release_rx.try_recv().is_err());
}
