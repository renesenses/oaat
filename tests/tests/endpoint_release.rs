//! endpoint_release (RFC §7.3.1): the endpoint gives up its output and tells
//! the controller, but only a controller that announced it handles it.

use std::time::Duration;

use tokio::sync::mpsc;

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse};
use oaat_core::Message;
use oaat_core::format::{AudioFormat, ChannelLayout};
use oaat_core::message::{
    EndpointCapabilities, EndpointRelease, FEATURE_ENDPOINT_RELEASE, RELEASE_REJECT_REASON,
    ReleaseReason, StreamStats,
};
use oaat_endpoint::{EndpointConfig, EndpointEvent, EndpointTransport};

const ACCEPT_TIMEOUT: Duration = Duration::from_millis(300);
const RELEASE_ACCEPT_TIMEOUT: Duration = Duration::from_millis(1000);

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
    let (ctrl_tx, endpoint, mut event_rx) = connect_with_events(features, false).await;
    // keep the event channel open for the transport
    tokio::spawn(async move { while event_rx.recv().await.is_some() {} });
    (ctrl_tx, endpoint)
}

/// Like `connect`, but the application also gets the endpoint's events, and
/// `defer_format_accept` is configurable. The fail-open delays are short:
/// `ACCEPT_TIMEOUT`, `RELEASE_ACCEPT_TIMEOUT`.
async fn connect_with_events(
    features: Vec<String>,
    defer_format_accept: bool,
) -> (
    mpsc::Sender<Message>,
    ConnectedEndpoint,
    mpsc::Receiver<EndpointEvent>,
) {
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
        defer_format_accept,
        accept_timeout: ACCEPT_TIMEOUT,
        release_accept_timeout: RELEASE_ACCEPT_TIMEOUT,
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

    (ctrl_tx, endpoint, event_rx)
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

/// A release while the endpoint holds a format accept back (device not open
/// yet): the transport answers the negotiation with `format_reject`
/// (`released`) instead of leaving the controller to its timeout, and its
/// `ReleaseHandled` event comes after the `FormatProposed`, so the
/// application can tell the proposal was ended by the release.
#[tokio::test]
async fn release_during_a_deferred_accept_rejects_the_negotiation() {
    let (ctrl_tx, mut endpoint, mut events) =
        connect_with_events(vec![FEATURE_ENDPOINT_RELEASE.into()], true).await;
    endpoint
        .propose_format(
            "s2",
            AudioFormat::PcmS16le,
            48000,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();
    // the application sees the proposal, but releases before it accepts
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("no FormatProposed")
            .expect("event channel closed");
        if matches!(ev, EndpointEvent::FormatProposed(_)) {
            break;
        }
    }
    ctrl_tx.send(release()).await.unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(2), endpoint.response_rx.recv())
        .await
        .expect("negotiation not answered")
        .expect("response channel closed");
    match resp {
        EndpointResponse::FormatReject(r) => {
            assert_eq!(r.stream_id, "s2");
            assert_eq!(r.reason, RELEASE_REJECT_REASON);
        }
        other => panic!("expected FormatReject, got {other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(2), endpoint.release_rx.recv())
        .await
        .expect("no release from the endpoint")
        .expect("release channel closed");
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("no ReleaseHandled")
            .expect("event channel closed");
        if matches!(ev, EndpointEvent::ReleaseHandled) {
            break;
        }
    }
    // no auto accept after the fail-open delay either
    tokio::time::sleep(RELEASE_ACCEPT_TIMEOUT + Duration::from_millis(500)).await;
    assert!(endpoint.response_rx.try_recv().is_err());
}

/// Without a negotiation in progress, a release rejects nothing.
#[tokio::test]
async fn release_without_a_negotiation_rejects_nothing() {
    let (ctrl_tx, mut endpoint, mut events) =
        connect_with_events(vec![FEATURE_ENDPOINT_RELEASE.into()], true).await;
    ctrl_tx.send(release()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), endpoint.release_rx.recv())
        .await
        .expect("no release from the endpoint")
        .expect("release channel closed");
    let ev = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("no ReleaseHandled")
        .expect("event channel closed");
    assert!(matches!(ev, EndpointEvent::ReleaseHandled), "got {ev:?}");
    assert!(endpoint.response_rx.try_recv().is_err());
}

/// Time until the auto accept of a deferred accept the application never
/// confirms.
async fn fail_open_after(features: Vec<String>) -> Duration {
    let (_ctrl_tx, mut endpoint, mut events) = connect_with_events(features, true).await;
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let t0 = std::time::Instant::now();
    endpoint
        .propose_format(
            "s3",
            AudioFormat::PcmS16le,
            48000,
            2,
            ChannelLayout::Stereo,
            16,
        )
        .await
        .unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(5), endpoint.response_rx.recv())
        .await
        .expect("no auto accept")
        .expect("response channel closed");
    assert!(
        matches!(resp, EndpointResponse::FormatAccept(_)),
        "got {resp:?}"
    );
    t0.elapsed()
}

/// A controller that handles endpoint_release waits longer for an accept
/// (§7.3.1), so the endpoint waits longer for its device before it fails
/// open; an older controller gets the usual delay.
#[tokio::test]
async fn fail_open_is_longer_for_a_controller_that_handles_release() {
    let older = fail_open_after(vec![]).await;
    assert!(
        older >= ACCEPT_TIMEOUT && older < RELEASE_ACCEPT_TIMEOUT,
        "older controller: auto accept after {older:?}"
    );
    let aware = fail_open_after(vec![FEATURE_ENDPOINT_RELEASE.into()]).await;
    assert!(
        aware >= RELEASE_ACCEPT_TIMEOUT,
        "release-aware controller: auto accept after {aware:?}"
    );
}
