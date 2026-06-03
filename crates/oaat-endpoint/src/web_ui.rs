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
    pub stream_format: Option<String>,
    pub stream_sample_rate: Option<u32>,
    pub stream_bits: Option<u8>,
    pub stream_channels: Option<u8>,
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub track_album: Option<String>,
    pub artwork_url: Option<String>,
    pub volume: u8,
    pub playing: bool,
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
            track_title: None,
            track_artist: None,
            track_album: None,
            artwork_url: None,
            volume: 100,
            playing: false,
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

fn json_str(s: &Option<String>) -> String {
    match s {
        Some(v) => format!(r#""{}""#, v.replace('"', r#"\""#)),
        None => "null".into(),
    }
}

fn render_status_json(status: &BridgeStatus) -> String {
    let devices_json: Vec<String> = status
        .available_devices
        .iter()
        .map(|d| format!(r#""{}""#, d.replace('"', r#"\""#)))
        .collect();
    format!(
        r#"{{"bridge_name":"{}","version":"{}","current_device":"{}","available_devices":[{}],"connected":{},"controller_name":{},"playing":{},"volume":{},"track_title":{},"track_artist":{},"track_album":{},"artwork_url":{},"stream_format":{},"stream_sample_rate":{},"stream_bits":{},"stream_channels":{}}}"#,
        status.bridge_name.replace('"', r#"\""#),
        status.version,
        status.current_device.replace('"', r#"\""#),
        devices_json.join(","),
        status.connected,
        json_str(&status.controller_name),
        status.playing,
        status.volume,
        json_str(&status.track_title),
        json_str(&status.track_artist),
        json_str(&status.track_album),
        json_str(&status.artwork_url),
        json_str(&status.stream_format),
        status.stream_sample_rate.unwrap_or(0),
        status.stream_bits.unwrap_or(0),
        status.stream_channels.unwrap_or(0),
    )
}

fn render_html(status: &BridgeStatus) -> String {
    let now_playing = if status.playing {
        let title = status.track_title.as_deref().unwrap_or("Unknown");
        let artist = status.track_artist.as_deref().unwrap_or("");
        let album = status.track_album.as_deref().unwrap_or("");
        let artwork = status.artwork_url.as_deref().unwrap_or("");
        let format_badge = if let Some(ref fmt) = status.stream_format {
            let rate = status.stream_sample_rate.unwrap_or(0);
            let bits = status.stream_bits.unwrap_or(0);
            let rate_khz = rate as f64 / 1000.0;
            if rate_khz.fract() == 0.0 {
                format!(r#"<span class="badge">{fmt} {bits}/{}</span>"#, rate_khz as u32)
            } else {
                format!(r#"<span class="badge">{fmt} {bits}/{rate_khz:.1}</span>"#)
            }
        } else {
            String::new()
        };
        let art_html = if artwork.is_empty() {
            r#"<div class="art-placeholder"></div>"#.to_string()
        } else {
            format!(r#"<img class="art" src="{artwork}" alt="">"#)
        };
        format!(
            r#"<div class="now-playing">
      {art_html}
      <div class="track-info">
        <div class="track-title">{title}</div>
        <div class="track-artist">{artist}</div>
        <div class="track-album">{album}</div>
        {format_badge}
      </div>
    </div>"#
        )
    } else {
        r#"<div class="now-playing idle">
      <div class="art-placeholder"></div>
      <div class="track-info"><div class="track-title idle-text">Idle</div></div>
    </div>"#
            .into()
    };

    let conn_dot = if status.connected { "connected" } else { "disconnected" };
    let conn_text = if status.connected {
        status.controller_name.as_deref().unwrap_or("Connected")
    } else {
        "Waiting..."
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
  <title>{bridge_name}</title>
  <style>
    * {{ margin:0; padding:0; box-sizing:border-box; }}
    html {{ font-size:18px; }}
    body {{ font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
           background:#0d0d1a; color:#ccc; height:100vh; overflow:hidden;
           display:flex; flex-direction:column; }}
    header {{ display:flex; align-items:center; justify-content:space-between;
              padding:0.6rem 1rem; background:#111125; border-bottom:1px solid #1a1a3a; }}
    .bridge-name {{ font-size:0.85rem; font-weight:600; color:#fff; }}
    .conn {{ display:flex; align-items:center; gap:0.4rem; font-size:0.75rem; }}
    .dot {{ width:8px; height:8px; border-radius:50%; }}
    .dot.connected {{ background:#4ade80; box-shadow:0 0 6px #4ade80; }}
    .dot.disconnected {{ background:#f87171; }}
    main {{ flex:1; display:flex; flex-direction:column; justify-content:center;
            align-items:center; padding:1.5rem; gap:1.5rem; }}
    .now-playing {{ display:flex; align-items:center; gap:1.2rem; width:100%; max-width:500px; }}
    .now-playing.idle {{ opacity:0.4; }}
    .art {{ width:120px; height:120px; border-radius:8px; object-fit:cover;
            background:#1a1a3a; flex-shrink:0; }}
    .art-placeholder {{ width:120px; height:120px; border-radius:8px;
                        background:#1a1a3a; flex-shrink:0;
                        display:flex; align-items:center; justify-content:center; }}
    .art-placeholder::after {{ content:""; display:block; width:40px; height:40px;
                               border-radius:50%; border:3px solid #333; }}
    .track-info {{ min-width:0; }}
    .track-title {{ font-size:1.3rem; font-weight:700; color:#fff;
                    white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
    .track-artist {{ font-size:1rem; color:#aaa; margin-top:0.15rem;
                     white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
    .track-album {{ font-size:0.8rem; color:#666; margin-top:0.1rem;
                    white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
    .idle-text {{ color:#555; font-style:italic; }}
    .badge {{ display:inline-block; margin-top:0.5rem; padding:0.2rem 0.6rem;
              border-radius:4px; font-size:0.7rem; font-weight:600;
              background:#1a1a3a; color:#4ade80; letter-spacing:0.03em; }}
    .volume {{ width:100%; max-width:500px; }}
    .vol-row {{ display:flex; align-items:center; gap:0.8rem; }}
    .vol-icon {{ font-size:1.2rem; color:#888; flex-shrink:0; cursor:pointer; }}
    .vol-slider {{ flex:1; -webkit-appearance:none; appearance:none; height:6px;
                   border-radius:3px; background:#1a1a3a; outline:none; }}
    .vol-slider::-webkit-slider-thumb {{ -webkit-appearance:none; width:24px; height:24px;
                                         border-radius:50%; background:#4ade80; cursor:pointer; }}
    .vol-val {{ font-size:0.8rem; color:#888; width:2.5rem; text-align:right; }}
    .device {{ font-size:0.7rem; color:#555; text-align:center; }}
  </style>
</head>
<body>
  <header>
    <span class="bridge-name">{bridge_name}</span>
    <span class="conn"><span class="dot {conn_dot}"></span>{conn_text}</span>
  </header>
  <main>
    {now_playing}
    <div class="volume">
      <div class="vol-row">
        <span class="vol-icon" onclick="toggleMute()">&#128264;</span>
        <input class="vol-slider" type="range" min="0" max="100" value="{volume}"
               oninput="setVol(this.value)">
        <span class="vol-val" id="vol-val">{volume}%</span>
      </div>
    </div>
    <div class="device">{current_device}</div>
  </main>
  <script>
    function setVol(v) {{
      document.getElementById('vol-val').textContent = v + '%';
      fetch('/api/volume', {{method:'POST',
        headers:{{'Content-Type':'application/json'}},
        body:JSON.stringify({{volume:parseInt(v)}})
      }});
    }}
    function toggleMute() {{
      fetch('/api/mute', {{method:'POST'}});
    }}
    async function poll() {{
      try {{
        const r = await fetch('/api/status');
        const s = await r.json();
        if (s.track_title) {{
          document.querySelector('.track-title').textContent = s.track_title || 'Unknown';
          document.querySelector('.track-artist').textContent = s.track_artist || '';
          document.querySelector('.track-album').textContent = s.track_album || '';
        }}
      }} catch(e) {{}}
    }}
    setInterval(poll, 3000);
  </script>
</body>
</html>"##,
        bridge_name = status.bridge_name,
        conn_dot = conn_dot,
        conn_text = conn_text,
        now_playing = now_playing,
        volume = status.volume,
        current_device = status.current_device,
    )
}
