//! endpoint_release (RFC §7.3.1): the endpoint gives up its output and tells
//! the controller, but only a controller that announced it handles it.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc;

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse, Zone};
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

/// An endpoint transport. Returns its application channel (the side that
/// sends the release), its control address and its event channel.
async fn spawn_endpoint(
    endpoint_id: &str,
) -> (mpsc::Sender<Message>, SocketAddr, mpsc::Receiver<EndpointEvent>) {
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = tcp.local_addr().unwrap();
    drop(tcp);

    let ep_config = EndpointConfig {
        endpoint_id: endpoint_id.into(),
        endpoint_name: format!("Release Test Endpoint {endpoint_id}"),
        control_addr,
        audio_addr: "127.0.0.1:0".parse().unwrap(),
        clock_addr: "127.0.0.1:0".parse().unwrap(),
        capabilities: capabilities(),
        buffer_size_ms: 1000,
        tls: false,
        defer_format_accept: false,
    };
    let (event_tx, event_rx) = mpsc::channel(64);
    let (ctrl_tx, ctrl_rx) = mpsc::channel(32);
    tokio::spawn(async move {
        EndpointTransport::run(ep_config, event_tx, ctrl_rx)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (ctrl_tx, control_addr, event_rx)
}

fn controller_config(features: Vec<String>) -> ControllerConfig {
    ControllerConfig {
        controller_id: "release-ctrl".into(),
        controller_name: "Release Controller".into(),
        features,
        clock_port: 9742,
        tls: false,
    }
}

/// An endpoint transport plus a controller connected to it with `features`.
/// Returns the endpoint's application channel and the controller.
async fn connect(features: Vec<String>) -> (mpsc::Sender<Message>, ConnectedEndpoint) {
    let (ctrl_tx, control_addr, mut event_rx) = spawn_endpoint("release-ep").await;
    let endpoint = ConnectedEndpoint::connect(&controller_config(features), control_addr)
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

/// In a zone, a release names the endpoint it came from, so the controller
/// can drop that one endpoint and keep the others playing.
#[tokio::test]
async fn zone_release_names_the_endpoint() {
    let mut zone = Zone::new(
        "release-zone".into(),
        "Release Zone".into(),
        controller_config(vec![FEATURE_ENDPOINT_RELEASE.into()]),
    );
    let mut releases = zone.take_release_rx().expect("release channel");
    assert!(zone.take_release_rx().is_none(), "taken only once");

    let mut ctrl_txs = Vec::new();
    for id in ["ep-a", "ep-b"] {
        let (ctrl_tx, control_addr, mut event_rx) = spawn_endpoint(id).await;
        tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
        assert_eq!(zone.add_endpoint(control_addr).await.unwrap(), id);
        ctrl_txs.push(ctrl_tx);
    }

    ctrl_txs[1].send(release()).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(2), releases.recv())
        .await
        .expect("no release from the zone")
        .expect("release channel closed");
    assert_eq!(r.endpoint_id, "ep-b");
    assert_eq!(r.release.stream_id, "s1");
    assert_eq!(r.release.reason, ReleaseReason::LocalSource);

    assert!(zone.remove_endpoint_and_notify(&r.endpoint_id).await);
    assert_eq!(zone.endpoint_ids(), vec!["ep-a".to_string()]);
    assert!(releases.try_recv().is_err(), "one release, once");
}
