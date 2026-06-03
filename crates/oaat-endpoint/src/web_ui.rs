//! Mini status web UI for Tune Bridge.
//!
//! Serves a single-page HTML dashboard on port 9741 showing bridge status,
//! current audio device, available devices, connection state, and stream info.
//! Also exposes `POST /api/device` to switch the active audio device.

use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tracing::{error, info, warn};

/// Shared state exposed to the web UI.
#[derive(Debug, Clone)]
pub struct BridgeStatus {
    pub bridge_name: String,
    pub version: String,
    pub current_device: String,
    pub available_devices: Vec<String>,
    pub connected: bool,
    pub controller_name: Option<String>,
    /// Current stream info (set when playing).
    pub stream_format: Option<String>,
    pub stream_sample_rate: Option<u32>,
    pub stream_bits: Option<u8>,
    pub stream_channels: Option<u8>,
}

impl Default for BridgeStatus {
    fn default() -> Self {
        Self {
            bridge_name: "Tune Bridge".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            current_device: String::new(),
            available_devices: Vec::new(),
            connected: false,
            controller_name: None,
            stream_format: None,
            stream_sample_rate: None,
            stream_bits: None,
            stream_channels: None,
        }
    }
}

/// Handle for updating bridge status from the main event loop.
#[derive(Clone)]
pub struct BridgeStatusHandle {
    inner: Arc<Mutex<BridgeStatus>>,
    notify: watch::Sender<()>,
}

impl BridgeStatusHandle {
    pub fn new(initial: BridgeStatus) -> (Self, BridgeStatusReader) {
        let inner = Arc::new(Mutex::new(initial));
        let (notify, rx) = watch::channel(());
        let handle = Self {
            inner: inner.clone(),
            notify,
        };
        let reader = BridgeStatusReader { inner, _rx: rx };
        (handle, reader)
    }

    pub async fn update<F: FnOnce(&mut BridgeStatus)>(&self, f: F) {
        let mut guard = self.inner.lock().await;
        f(&mut guard);
        let _ = self.notify.send(());
    }

    pub async fn snapshot(&self) -> BridgeStatus {
        self.inner.lock().await.clone()
    }
}

/// Read-only access to bridge status (for the web server).
#[derive(Clone)]
#[allow(dead_code)]
pub struct BridgeStatusReader {
    inner: Arc<Mutex<BridgeStatus>>,
    _rx: watch::Receiver<()>,
}

/// Channel for sending device-switch requests from the web UI to the main loop.
pub type DeviceSwitchTx = tokio::sync::mpsc::Sender<String>;
pub type DeviceSwitchRx = tokio::sync::mpsc::Receiver<String>;

/// Start the web UI HTTP server. Returns immediately, running in the background.
pub async fn start_web_ui(
    port: u16,
    status_handle: BridgeStatusHandle,
    device_switch_tx: DeviceSwitchTx,
) {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => {
            info!(port = l.local_addr().map(|a| a.port()).unwrap_or(port), "web UI listening");
            l
        }
        Err(e) => {
            error!(error = %e, port, "failed to start web UI server");
            return;
        }
    };

    let status_handle = Arc::new(status_handle);
    let device_switch_tx = Arc::new(device_switch_tx);

    tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "web UI accept error");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let status = status_handle.clone();
            let switch_tx = device_switch_tx.clone();

            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let status = status.clone();
                    let switch_tx = switch_tx.clone();
                    async move { handle_request(req, &status, &switch_tx).await }
                });
                if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                    // Connection reset is normal for browsers
                    if !e.is_incomplete_message() {
                        warn!(error = %e, "web UI connection error");
                    }
                }
            });
        }
    });
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    status_handle: &BridgeStatusHandle,
    device_switch_tx: &DeviceSwitchTx,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") | (&Method::GET, "/index.html") => {
            let status = status_handle.snapshot().await;
            let html = render_html(&status);
            Ok(Response::builder()
                .status(200)
                .header("Content-Type", "text/html; charset=utf-8")
                .body(Full::new(Bytes::from(html)))
                .unwrap())
        }
        (&Method::GET, "/api/status") => {
            let status = status_handle.snapshot().await;
            let json = render_status_json(&status);
            Ok(Response::builder()
                .status(200)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(json)))
                .unwrap())
        }
        (&Method::POST, "/api/device") => {
            // Read body to get device name
            use http_body_util::BodyExt;
            let body = req.collect().await.map(|c| c.to_bytes()).unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body);

            // Parse simple JSON: {"device": "name"}
            let device_name = body_str
                .trim()
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .and_then(|s| {
                    // Very simple JSON parsing — no external dep needed
                    s.split(':')
                        .nth(1)
                        .map(|v| v.trim().trim_matches('"').to_string())
                });

            match device_name {
                Some(name) if !name.is_empty() => {
                    info!(device = %name, "web UI: device switch requested");
                    let _ = device_switch_tx.send(name.clone()).await;
                    Ok(Response::builder()
                        .status(200)
                        .header("Content-Type", "application/json")
                        .body(Full::new(Bytes::from(
                            format!(r#"{{"ok":true,"device":"{}"}}"#, name),
                        )))
                        .unwrap())
                }
                _ => Ok(Response::builder()
                    .status(400)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from(
                        r#"{"ok":false,"error":"missing device name"}"#,
                    )))
                    .unwrap()),
            }
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap()),
    }
}

fn render_status_json(status: &BridgeStatus) -> String {
    let devices_json: Vec<String> = status
        .available_devices
        .iter()
        .map(|d| format!(r#""{}""#, d.replace('"', r#"\""#)))
        .collect();
    let stream_info = if let Some(ref fmt) = status.stream_format {
        format!(
            r#","stream_format":"{}","stream_sample_rate":{},"stream_bits":{},"stream_channels":{}"#,
            fmt,
            status.stream_sample_rate.unwrap_or(0),
            status.stream_bits.unwrap_or(0),
            status.stream_channels.unwrap_or(0),
        )
    } else {
        String::new()
    };
    format!(
        r#"{{"bridge_name":"{}","version":"{}","current_device":"{}","available_devices":[{}],"connected":{},"controller_name":{}{}}}"#,
        status.bridge_name.replace('"', r#"\""#),
        status.version,
        status.current_device.replace('"', r#"\""#),
        devices_json.join(","),
        status.connected,
        status
            .controller_name
            .as_ref()
            .map(|n| format!(r#""{}""#, n.replace('"', r#"\""#)))
            .unwrap_or_else(|| "null".into()),
        stream_info,
    )
}

fn render_html(status: &BridgeStatus) -> String {
    let device_options: String = status
        .available_devices
        .iter()
        .map(|d| {
            let selected = if *d == status.current_device {
                " selected"
            } else {
                ""
            };
            format!(
                r#"<option value="{}"{}>{}</option>"#,
                d.replace('"', "&quot;"),
                selected,
                d
            )
        })
        .collect::<Vec<_>>()
        .join("\n              ");

    let connection_status = if status.connected {
        let ctrl = status
            .controller_name
            .as_deref()
            .unwrap_or("unknown");
        format!(
            r#"<span class="status connected">Connected to {}</span>"#,
            ctrl
        )
    } else {
        r#"<span class="status waiting">Waiting for connection...</span>"#.into()
    };

    let stream_info = if let Some(ref fmt) = status.stream_format {
        format!(
            r#"<div class="stream-info">
            <div class="info-row"><span class="label">Format:</span> <span class="value">{}</span></div>
            <div class="info-row"><span class="label">Sample Rate:</span> <span class="value">{} Hz</span></div>
            <div class="info-row"><span class="label">Bit Depth:</span> <span class="value">{}-bit</span></div>
            <div class="info-row"><span class="label">Channels:</span> <span class="value">{}</span></div>
          </div>"#,
            fmt,
            status.stream_sample_rate.unwrap_or(0),
            status.stream_bits.unwrap_or(0),
            status.stream_channels.unwrap_or(0),
        )
    } else {
        r#"<div class="stream-info idle">No active stream</div>"#.into()
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Tune Bridge</title>
  <style>
    * {{ margin: 0; padding: 0; box-sizing: border-box; }}
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
           background: #1a1a2e; color: #e0e0e0; min-height: 100vh;
           display: flex; justify-content: center; align-items: center; }}
    .container {{ max-width: 480px; width: 100%; padding: 2rem; }}
    h1 {{ font-size: 1.5rem; font-weight: 600; margin-bottom: 0.25rem; color: #fff; }}
    .version {{ color: #888; font-size: 0.85rem; margin-bottom: 1.5rem; }}
    .card {{ background: #16213e; border-radius: 12px; padding: 1.25rem;
             margin-bottom: 1rem; border: 1px solid #0f3460; }}
    .card-title {{ font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.05em;
                   color: #888; margin-bottom: 0.75rem; }}
    .status {{ display: inline-block; padding: 0.35rem 0.75rem; border-radius: 6px;
               font-size: 0.9rem; font-weight: 500; }}
    .status.connected {{ background: #0a3d2a; color: #4ade80; }}
    .status.waiting {{ background: #3d2a0a; color: #fbbf24; }}
    select {{ width: 100%; padding: 0.6rem; border-radius: 8px; border: 1px solid #0f3460;
              background: #1a1a2e; color: #e0e0e0; font-size: 0.95rem; cursor: pointer; }}
    select:focus {{ outline: none; border-color: #4ade80; }}
    .stream-info {{ font-size: 0.95rem; }}
    .stream-info.idle {{ color: #666; font-style: italic; }}
    .info-row {{ display: flex; justify-content: space-between; padding: 0.3rem 0;
                 border-bottom: 1px solid #0f3460; }}
    .info-row:last-child {{ border-bottom: none; }}
    .label {{ color: #888; }}
    .value {{ color: #fff; font-weight: 500; }}
    .refresh {{ text-align: center; margin-top: 0.5rem; }}
    .refresh a {{ color: #4ade80; text-decoration: none; font-size: 0.8rem; }}
    .refresh a:hover {{ text-decoration: underline; }}
  </style>
</head>
<body>
  <div class="container">
    <h1>{bridge_name}</h1>
    <div class="version">v{version}</div>

    <div class="card">
      <div class="card-title">Connection</div>
      {connection_status}
    </div>

    <div class="card">
      <div class="card-title">Audio Device</div>
      <select id="device-select" onchange="switchDevice(this.value)">
        {device_options}
      </select>
    </div>

    <div class="card">
      <div class="card-title">Stream</div>
      {stream_info}
    </div>

    <div class="refresh"><a href="/">Refresh</a></div>
  </div>

  <script>
    function switchDevice(name) {{
      fetch('/api/device', {{
        method: 'POST',
        headers: {{ 'Content-Type': 'application/json' }},
        body: JSON.stringify({{ device: name }})
      }}).then(r => r.json()).then(d => {{
        if (d.ok) location.reload();
      }});
    }}
    // Auto-refresh every 5 seconds
    setTimeout(() => location.reload(), 5000);
  </script>
</body>
</html>"##,
        bridge_name = status.bridge_name,
        version = status.version,
        connection_status = connection_status,
        device_options = device_options,
        stream_info = stream_info,
    )
}
