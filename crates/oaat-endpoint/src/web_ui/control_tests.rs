use super::*;
use http_body_util::BodyExt;

fn policy(remote: bool, token: Option<&str>) -> ControlPolicy {
    ControlPolicy {
        local: "127.0.0.1:9741".parse().unwrap(),
        peer: if remote {
            "192.0.2.4:43210"
        } else {
            "127.0.0.1:43210"
        }
        .parse()
        .unwrap(),
        token: Arc::new(token.map(str::to_owned)),
    }
}

fn request(
    path: &str,
    origin: Option<&str>,
    mime: Option<&str>,
    bearer: Option<&str>,
) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("host", "127.0.0.1:9741");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    if let Some(mime) = mime {
        builder = builder.header("content-type", mime);
    }
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", bearer);
    }
    builder
        .body(Full::new(Bytes::from_static(
            br#"{"device":"test device"}"#,
        )))
        .unwrap()
}

async fn rejected(req: Request<Full<Bytes>>, policy: ControlPolicy, expected: StatusCode) {
    let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
    let (devices, mut device_rx) = tokio::sync::mpsc::channel(1);
    let (releases, mut release_rx) = tokio::sync::mpsc::channel(1);
    let reply = handle_request(req, &status, &devices, &Some(releases), &policy)
        .await
        .unwrap();
    assert_eq!(
        reply.status(),
        expected,
        "unauthorized control request was accepted"
    );
    assert!(
        device_rx.try_recv().is_err(),
        "rejected request switched a device"
    );
    assert!(
        release_rx.try_recv().is_err(),
        "rejected request released the output"
    );
}

#[tokio::test]
async fn cross_origin_and_rebinding_requests_never_control_the_output() {
    for path in ["/api/device", "/api/release"] {
        for origin in [
            Some("null"),
            Some("http://evil.example"),
            Some("http://127.0.0.1:9742"),
        ] {
            rejected(
                request(path, origin, Some("application/json"), None),
                policy(false, None),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
        let mut req = request(
            path,
            Some("http://evil.example:9741"),
            Some("application/json"),
            None,
        );
        req.headers_mut()
            .insert("host", "evil.example:9741".parse().unwrap());
        rejected(req, policy(false, None), StatusCode::FORBIDDEN).await;
        let mut req = request(
            path,
            Some("http://127.0.0.1:9741"),
            Some("application/json"),
            None,
        );
        req.headers_mut()
            .append("origin", "http://evil.example".parse().unwrap());
        rejected(req, policy(false, None), StatusCode::FORBIDDEN).await;
    }
}

#[tokio::test]
async fn controls_require_json_content_type() {
    for path in ["/api/device", "/api/release"] {
        for mime in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
        ] {
            rejected(
                request(path, Some("http://127.0.0.1:9741"), mime, None),
                policy(false, None),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn remote_controls_are_denied_without_a_configured_token() {
    for path in ["/api/device", "/api/release"] {
        rejected(
            request(
                path,
                Some("http://127.0.0.1:9741"),
                Some("application/json"),
                None,
            ),
            policy(true, None),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
}

#[tokio::test]
async fn configured_token_is_required_locally_and_remotely_and_cannot_be_empty() {
    for remote in [false, true] {
        for path in ["/api/device", "/api/release"] {
            for bearer in [None, Some("Bearer wrong"), Some("Bearer test-secret-wrong")] {
                rejected(
                    request(
                        path,
                        Some("http://127.0.0.1:9741"),
                        Some("application/json"),
                        bearer,
                    ),
                    policy(remote, Some("test-secret")),
                    StatusCode::UNAUTHORIZED,
                )
                .await;
            }
            rejected(
                request(
                    path,
                    Some("http://127.0.0.1:9741"),
                    Some("application/json"),
                    Some("Bearer "),
                ),
                policy(remote, Some("")),
                StatusCode::UNAUTHORIZED,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn authorized_controls_reach_only_the_expected_channel() {
    for (remote, token) in [
        (false, None),
        (false, Some("test-secret")),
        (true, Some("test-secret")),
    ] {
        let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
        let (devices, mut device_rx) = tokio::sync::mpsc::channel(1);
        let (releases, mut release_rx) = tokio::sync::mpsc::channel(1);
        let releases = Some(releases);
        let bearer = token.map(|_| "Bearer test-secret");
        let req = request(
            "/api/device",
            Some("http://127.0.0.1:9741"),
            Some("application/json; charset=utf-8"),
            bearer,
        );
        assert_eq!(
            handle_request(req, &status, &devices, &releases, &policy(remote, token))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(device_rx.try_recv().unwrap(), "test device");
        assert!(release_rx.try_recv().is_err());
        let req = request(
            "/api/release",
            Some("http://127.0.0.1:9741"),
            Some("application/json"),
            bearer,
        );
        assert_eq!(
            handle_request(req, &status, &devices, &releases, &policy(remote, token))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(release_rx.try_recv(), Ok(()));
        assert!(device_rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn status_remains_read_only_without_authentication_and_does_not_disclose_the_token() {
    let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
    let (devices, mut device_rx) = tokio::sync::mpsc::channel(1);
    let (releases, mut release_rx) = tokio::sync::mpsc::channel(1);
    for path in ["/", "/api/status"] {
        let req = Request::builder()
            .uri(path)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = handle_request(
            req,
            &status,
            &devices,
            &Some(releases.clone()),
            &policy(true, Some("test-secret")),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&body).contains("test-secret"));
    }
    assert!(device_rx.try_recv().is_err());
    assert!(release_rx.try_recv().is_err());
}

#[test]
fn matching_ipv6_and_localhost_origins_are_supported() {
    for authority in ["localhost:9741", "[::1]:9741"] {
        let mut p = policy(false, None);
        p.local = "[::1]:9741".parse().unwrap();
        let origin = format!("http://{authority}");
        let mut req = request(
            "/api/release",
            Some(&origin),
            Some("application/json"),
            None,
        );
        req.headers_mut().insert("host", authority.parse().unwrap());
        assert_eq!(p.authorize(&req), Ok(()));
    }
}

#[tokio::test]
async fn a_valid_token_does_not_bypass_origin_or_json_checks() {
    for path in ["/api/device", "/api/release"] {
        rejected(
            request(
                path,
                Some("http://evil.example"),
                Some("application/json"),
                Some("Bearer test-secret"),
            ),
            policy(true, Some("test-secret")),
            StatusCode::FORBIDDEN,
        )
        .await;
        rejected(
            request(
                path,
                Some("http://127.0.0.1:9741"),
                Some("text/plain"),
                Some("Bearer test-secret"),
            ),
            policy(true, Some("test-secret")),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        )
        .await;
    }
}

#[tokio::test]
async fn a_json_content_type_does_not_turn_invalid_json_into_a_device_command() {
    let mut req = request(
        "/api/device",
        Some("http://127.0.0.1:9741"),
        Some("application/json"),
        None,
    );
    *req.body_mut() = Full::new(Bytes::from_static(b"{device:invalid}"));
    rejected(req, policy(false, None), StatusCode::BAD_REQUEST).await;
}

#[test]
fn the_default_http_port_accepts_the_browser_canonical_origin() {
    for host in ["127.0.0.1", "127.0.0.1:80"] {
        let mut p = policy(false, None);
        p.local = "127.0.0.1:80".parse().unwrap();
        let mut req = request(
            "/api/release",
            Some("http://127.0.0.1"),
            Some("application/json"),
            None,
        );
        req.headers_mut().insert("host", host.parse().unwrap());
        assert_eq!(p.authorize(&req), Ok(()));
    }
}

// ---- Non-browser host controllers (GHSA-34qc-hmv4-hmg2 compatibility) ----

/// The request a host controller (Verox) sends: no Origin, JSON content type,
/// empty body, over loopback.
fn controller_request(
    path: &str,
    mime: Option<&str>,
    bearer: Option<&str>,
) -> Request<Full<Bytes>> {
    let mut req = request(path, None, mime, bearer);
    *req.body_mut() = Full::new(Bytes::new());
    req
}

#[tokio::test]
async fn loopback_controller_without_origin_and_empty_json_body_can_release() {
    let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
    let (devices, mut device_rx) = tokio::sync::mpsc::channel(1);
    let (releases, mut release_rx) = tokio::sync::mpsc::channel(1);
    let reply = handle_request(
        controller_request("/api/release", Some("application/json"), None),
        &status,
        &devices,
        &Some(releases),
        &policy(false, None),
    )
    .await
    .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(release_rx.try_recv(), Ok(()));
    assert!(device_rx.try_recv().is_err());
}

#[tokio::test]
async fn controller_without_origin_still_needs_json_loopback_or_token() {
    for path in ["/api/device", "/api/release"] {
        for mime in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
        ] {
            rejected(
                controller_request(path, mime, None),
                policy(false, None),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            )
            .await;
        }
        rejected(
            controller_request(path, Some("application/json"), None),
            policy(true, None),
            StatusCode::FORBIDDEN,
        )
        .await;
        for remote in [false, true] {
            for bearer in [None, Some("Bearer wrong"), Some("test-secret")] {
                rejected(
                    controller_request(path, Some("application/json"), bearer),
                    policy(remote, Some("test-secret")),
                    StatusCode::UNAUTHORIZED,
                )
                .await;
            }
        }
    }
    for remote in [false, true] {
        let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
        let (devices, _device_rx) = tokio::sync::mpsc::channel(1);
        let (releases, mut release_rx) = tokio::sync::mpsc::channel(1);
        let reply = handle_request(
            controller_request(
                "/api/release",
                Some("application/json"),
                Some("Bearer test-secret"),
            ),
            &status,
            &devices,
            &Some(releases),
            &policy(remote, Some("test-secret")),
        )
        .await
        .unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(release_rx.try_recv(), Ok(()));
    }
}

#[tokio::test]
async fn a_repeated_or_empty_origin_is_not_treated_as_missing() {
    for path in ["/api/device", "/api/release"] {
        let mut req = controller_request(path, Some("application/json"), None);
        req.headers_mut()
            .append("origin", "http://127.0.0.1:9741".parse().unwrap());
        req.headers_mut()
            .append("origin", "http://127.0.0.1:9741".parse().unwrap());
        rejected(req, policy(false, None), StatusCode::FORBIDDEN).await;
        let mut req = controller_request(path, Some("application/json"), None);
        req.headers_mut().insert("origin", "".parse().unwrap());
        rejected(req, policy(false, None), StatusCode::FORBIDDEN).await;
    }
}

// ---- Over a real socket ----

async fn serve_on(addr: &str) -> (SocketAddr, tokio::sync::mpsc::Receiver<()>) {
    let listener = TcpListener::bind(addr).await.unwrap();
    let local = listener.local_addr().unwrap();
    let (status, _reader) = BridgeStatusHandle::new(BridgeStatus::default());
    let (devices, _device_rx) = tokio::sync::mpsc::channel(1);
    let (releases, release_rx) = tokio::sync::mpsc::channel(4);
    serve_web_ui(listener, status, devices, Some(releases), None);
    (local, release_rx)
}

async fn raw_post(addr: SocketAddr, head: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req =
        format!("POST /api/release HTTP/1.1\r\nHost: {addr}\r\n{head}Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    reply
}

#[tokio::test]
async fn verox_release_request_over_loopback_socket_is_accepted() {
    let (addr, mut release_rx) = serve_on("127.0.0.1:0").await;
    // curl -X POST -H 'Content-Type: application/json' (no body, no length)
    let reply = raw_post(addr, "Content-Type: application/json\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert_eq!(release_rx.recv().await, Some(()));
    // an HTTP client library sending an explicit empty body
    let reply = raw_post(
        addr,
        "Content-Type: application/json\r\nContent-Length: 0\r\n",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert_eq!(release_rx.recv().await, Some(()));
}

#[tokio::test]
async fn foreign_origin_or_non_json_over_loopback_socket_is_refused() {
    let (addr, mut release_rx) = serve_on("127.0.0.1:0").await;
    let reply = raw_post(
        addr,
        "Origin: http://evil.example\r\nContent-Type: application/json\r\nContent-Length: 0\r\n",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    let reply = raw_post(addr, "Content-Type: text/plain\r\nContent-Length: 0\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 415"), "{reply}");
    assert!(release_rx.try_recv().is_err());
}

/// A non-loopback address of this host, if it has one (no packet is sent).
fn non_loopback_local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

#[tokio::test]
async fn loopback_listener_is_unreachable_from_another_interface() {
    let (addr, _release_rx) = serve_on("127.0.0.1:0").await;
    let Some(ip) = non_loopback_local_ip() else {
        eprintln!("no non-loopback interface: skipped");
        return;
    };
    let other = SocketAddr::new(ip, addr.port());
    assert!(
        tokio::net::TcpStream::connect(other).await.is_err(),
        "loopback-bound controls reachable on {other}"
    );
}
