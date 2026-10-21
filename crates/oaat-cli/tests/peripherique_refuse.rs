//! #62: the real `tune-bridge endpoint`, on a fake ALSA `hw:` device that
//! behaves like the RME ADI-2 DAC (it opens, takes S32_LE, refuses
//! DSD_U32_BE with EINVAL), driven by an OAAT controller.
//!
//! The fake device is ALSA configuration, not code: a file added to
//! `ALSA_CONFIG_PATH`, after the system `alsa.conf`, redefines `pcm.hw` as a
//! plug onto the `null` PCM, restricted to S32_LE. alsa-lib reloads the
//! files of `ALSA_CONFIG_PATH` when one changes, so a test can change what
//! the "DAC" accepts mid-session.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse};
use oaat_core::format::{AudioFormat, ChannelLayout};
use oaat_core::message::FormatPropose;
use oaat_core::{Message, PacketFlags};

const DEVICE: &str = "hw:CARD=FakeDac,DEV=0";

/// `hw:` as a plug onto `null`: S32_LE only, DSD_U32_BE refused (EINVAL from
/// `snd_pcm_hw_params_set_format`, as on the ADI-2).
const DAC_PCM_ONLY: &str = "pcm.!hw { @args [ CARD DEV SUBDEV ] @args.CARD { type string } \
     @args.DEV { type integer default 0 } @args.SUBDEV { type integer default -1 } \
     type plug slave { pcm \"null\" format S32_LE } }\n";

/// `hw:` as the bare `null` PCM: accepts every format, DSD included.
const DAC_ACCEPTS_DSD: &str = "pcm.!hw { @args [ CARD DEV SUBDEV ] @args.CARD { type string } \
     @args.DEV { type integer default 0 } @args.SUBDEV { type integer default -1 } \
     type null }\n";

struct Bridge {
    child: Child,
    home: PathBuf,
    addr: std::net::SocketAddr,
}

impl Bridge {
    /// Start the endpoint with `--dsd` on the fake device described by
    /// `asoundrc`, and wait for it to listen.
    fn start(tag: &str, asoundrc: &str) -> Self {
        let home = std::env::temp_dir().join(format!("oaat-62-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        set_dac(&home, asoundrc);
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let log = std::fs::File::create(home.join("endpoint.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_tune-bridge"))
            .args(["endpoint", "--name", &format!("oaat-62-{tag}")])
            .args(["--port", &port.to_string(), "--no-web-ui", "--dsd"])
            .args(["--audio-device", DEVICE])
            .env("HOME", &home)
            .env(
                "ALSA_CONFIG_PATH",
                format!("{SYSTEM_ALSA_CONF}:{}", home.join(FAKE_DAC_CONF).display()),
            )
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start tune-bridge");
        let addr = format!("127.0.0.1:{port}").parse().unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while std::net::TcpStream::connect(addr).is_err() {
            assert!(Instant::now() < end, "tune-bridge does not listen");
            std::thread::sleep(Duration::from_millis(50));
        }
        // that probe connection is a session of its own: let it end
        std::thread::sleep(Duration::from_millis(300));
        Self { child, home, addr }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("endpoint.log")).unwrap_or_default()
    }

    async fn connect(&self) -> ConnectedEndpoint {
        let cfg = ControllerConfig {
            controller_id: "oaat-62".into(),
            controller_name: "oaat-62 controller".into(),
            features: vec![],
            clock_port: 0,
            tls: false,
        };
        tokio::time::timeout(Duration::from_secs(5), ConnectedEndpoint::connect(&cfg, self.addr))
            .await
            .expect("connect timed out")
            .expect("connect failed")
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

const SYSTEM_ALSA_CONF: &str = "/usr/share/alsa/alsa.conf";
const FAKE_DAC_CONF: &str = "fake-dac.conf";

/// Write the fake DAC's configuration. alsa-lib reloads it when the file's
/// inode or mtime (in seconds) changes: written then renamed, it gets a new
/// inode, so a change within the same second is seen.
fn set_dac(home: &Path, conf: &str) {
    let tmp = home.join("fake-dac.new");
    std::fs::write(&tmp, conf).unwrap();
    std::fs::rename(&tmp, home.join(FAKE_DAC_CONF)).unwrap();
}

fn dsd64(stream_id: &str) -> FormatPropose {
    // What Tune proposes for a DSF file (native DSD path).
    FormatPropose {
        stream_id: stream_id.into(),
        format: AudioFormat::DsdU8,
        sample_rate: 2_822_400,
        channels: 2,
        channel_layout: ChannelLayout::Stereo,
        bits_per_sample: 1,
        dsd_rate: Some(64),
    }
}

fn pcm(stream_id: &str) -> FormatPropose {
    FormatPropose {
        stream_id: stream_id.into(),
        format: AudioFormat::PcmS24le,
        sample_rate: 44_100,
        channels: 2,
        channel_layout: ChannelLayout::Stereo,
        bits_per_sample: 24,
        dsd_rate: None,
    }
}

#[derive(Debug)]
enum Answer {
    Accept,
    Reject(String),
}

/// Propose and wait for the answer, longer than the endpoint's 5 s
/// fail-open on the deferred accept. Any other outcome fails the test.
async fn propose(bridge: &Bridge, ep: &mut ConnectedEndpoint, fp: FormatPropose) -> Answer {
    ep.send_message(&Message::FormatPropose(fp)).await.unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ep.response_rx.recv()).await {
            Ok(Some(EndpointResponse::FormatAccept(_))) => return Answer::Accept,
            Ok(Some(EndpointResponse::FormatReject(r))) => return Answer::Reject(r.reason),
            Ok(Some(EndpointResponse::StreamStats(_))) => continue,
            Ok(Some(other)) => panic!("unexpected answer {other:?}\n{}", bridge.log()),
            Ok(None) => panic!("connection closed\n{}", bridge.log()),
            Err(_) => panic!("no answer within 8 s\n{}", bridge.log()),
        }
    }
}

/// Stream `secs` of silence at the real-time rate of S24 44.1 kHz stereo.
async fn stream_pcm(ep: &mut ConnectedEndpoint, secs: f64) {
    let chunk = 1_404; // 234 frames
    let bytes_per_s = 44_100.0 * 6.0;
    let n = (bytes_per_s * secs / chunk as f64) as u32;
    let every = Duration::from_secs_f64(chunk as f64 / bytes_per_s);
    let payload = vec![0u8; chunk];
    let t0 = Instant::now();
    for i in 0..n {
        let flags = if i == 0 { PacketFlags::FIRST_PACKET } else { PacketFlags::empty() };
        ep.send_audio(1, AudioFormat::PcmS24le, 0, 0, &payload, flags)
            .await
            .unwrap();
        if let Some(d) = (t0 + every * (i + 1)).checked_duration_since(Instant::now()) {
            tokio::time::sleep(d).await;
        }
    }
}

/// The ADI-2 case: the DAC refuses DSD_U32_BE, so the endpoint announces no
/// native DSD at all (dsd_max_rate absent, no DSD format) and says the
/// server will convert. A DSD proposal is refused at negotiation, and PCM
/// plays.
#[tokio::test(flavor = "multi_thread")]
async fn un_dac_qui_refuse_le_dsd_n_annonce_aucun_dsd_natif() {
    let bridge = Bridge::start("refus", DAC_PCM_ONLY);
    let mut ep = bridge.connect().await;
    let caps = &ep.info.capabilities;
    assert_eq!(caps.dsd_max_rate, None, "log:\n{}", bridge.log());
    assert!(
        !caps.formats.iter().any(|f| f.is_dsd()),
        "no DSD format announced: {:?}",
        caps.formats
    );
    let log = bridge.log();
    assert!(
        log.contains("native DSD not supported by device, DSD will be converted to PCM by the server"),
        "{log}"
    );
    assert!(!log.contains("native DSD capability enabled"), "{log}");

    match propose(&bridge, &mut ep, dsd64("dsd-1")).await {
        Answer::Reject(_) => {}
        other => panic!("DSD must be refused, got {other:?}\n{}", bridge.log()),
    }
    match propose(&bridge, &mut ep, pcm("pcm-1")).await {
        Answer::Accept => {}
        other => panic!("PCM must be accepted, got {other:?}\n{}", bridge.log()),
    }
}

/// The device refuses the stream when it is opened, mid-session (here, the
/// DAC took DSD at startup and no longer does). The endpoint answers
/// FormatReject with the device error instead of accepting a stream it
/// cannot play, goes back to idle, and the next PCM stream plays, on the
/// same connection and on the next one.
#[tokio::test(flavor = "multi_thread")]
async fn une_ouverture_refusee_en_session_est_signalee_et_le_pcm_suivant_joue() {
    let bridge = Bridge::start("session", DAC_ACCEPTS_DSD);
    let mut ep = bridge.connect().await;
    assert!(
        ep.info.capabilities.dsd_max_rate.is_some(),
        "the fake DAC takes DSD at startup\n{}",
        bridge.log()
    );
    set_dac(&bridge.home, DAC_PCM_ONLY);

    let t0 = Instant::now();
    match propose(&bridge, &mut ep, dsd64("dsd-1")).await {
        Answer::Reject(reason) => assert!(
            reason.contains("DSD_U32_BE"),
            "the reason carries the device error: {reason}"
        ),
        other => panic!("the refused open must be reported, got {other:?}\n{}", bridge.log()),
    }
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "reported before the 5 s fail-open, took {:?}",
        t0.elapsed()
    );

    match propose(&bridge, &mut ep, pcm("pcm-1")).await {
        Answer::Accept => {}
        other => panic!("next PCM stream must be accepted, got {other:?}\n{}", bridge.log()),
    }
    ep.send_play("pcm-1").await.unwrap();
    stream_pcm(&mut ep, 1.0).await;
    ep.send_stop("pcm-1").await.unwrap();
    drop(ep);

    // the next track comes on a new connection, as Tune does it
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut ep = bridge.connect().await;
    match propose(&bridge, &mut ep, pcm("pcm-2")).await {
        Answer::Accept => {}
        other => panic!("PCM after reconnect must be accepted, got {other:?}\n{}", bridge.log()),
    }
    ep.send_play("pcm-2").await.unwrap();
    stream_pcm(&mut ep, 0.5).await;
    drop(ep);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let log = bridge.log();
    let opened_s32 = log
        .lines()
        .filter(|l| l.contains("ALSA device opened") && l.contains("S32_LE"))
        .count();
    assert!(opened_s32 >= 2, "both PCM streams opened the device:\n{log}");
}
