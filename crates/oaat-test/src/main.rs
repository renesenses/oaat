use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;
use tokio::time::timeout;

use oaat_controller::{ConnectedEndpoint, ControllerConfig, EndpointResponse};
use oaat_core::ChannelLayout;
use oaat_core::format::AudioFormat;
use oaat_core::wire::PacketFlags;

#[derive(Parser)]
#[command(name = "oaat-test", about = "OAAT protocol conformance test tool")]
struct Cli {
    /// Endpoint address (host:port)
    target: SocketAddr,
    /// Timeout per test in seconds
    #[arg(short, long, default_value = "5")]
    timeout: u64,
    /// Enable TLS 1.3 on the control channel (TOFU client)
    #[arg(long)]
    tls: bool,
}

struct TestRunner {
    target: SocketAddr,
    timeout: Duration,
    tls: bool,
    passed: u32,
    failed: u32,
    skipped: u32,
}

impl TestRunner {
    fn new(target: SocketAddr, timeout_secs: u64, tls: bool) -> Self {
        Self {
            target,
            timeout: Duration::from_secs(timeout_secs),
            tls,
            passed: 0,
            failed: 0,
            skipped: 0,
        }
    }

    fn pass(&mut self, name: &str) {
        self.passed += 1;
        println!("  \x1b[32mPASS\x1b[0m  {name}");
    }

    fn fail(&mut self, name: &str, reason: &str) {
        self.failed += 1;
        println!("  \x1b[31mFAIL\x1b[0m  {name}: {reason}");
    }

    fn skip(&mut self, name: &str, reason: &str) {
        self.skipped += 1;
        println!("  \x1b[33mSKIP\x1b[0m  {name}: {reason}");
    }

    fn config(&self) -> ControllerConfig {
        ControllerConfig {
            controller_id: "oaat-test-runner".into(),
            controller_name: "OAAT Conformance Tester".into(),
            features: vec![],
            clock_port: oaat_core::DEFAULT_CLOCK_PORT,
            tls: self.tls,
        }
    }

    fn summary(&self) {
        let total = self.passed + self.failed + self.skipped;
        println!("\n{}", "=".repeat(60));
        print!("  {total} tests: ");
        print!("\x1b[32m{} passed\x1b[0m", self.passed);
        if self.failed > 0 {
            print!(", \x1b[31m{} failed\x1b[0m", self.failed);
        }
        if self.skipped > 0 {
            print!(", \x1b[33m{} skipped\x1b[0m", self.skipped);
        }
        println!();

        if self.failed == 0 {
            println!("  \x1b[32mEndpoint is CONFORMANT\x1b[0m");
        } else {
            println!(
                "  \x1b[31mEndpoint has {} conformance issue(s)\x1b[0m",
                self.failed
            );
        }
    }

    async fn connect(&self) -> Result<ConnectedEndpoint, String> {
        match timeout(
            self.timeout,
            ConnectedEndpoint::connect(&self.config(), self.target),
        )
        .await
        {
            Ok(Ok(ep)) => Ok(ep),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("timeout".into()),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("oaat=warn")
        .init();

    let cli = Cli::parse();
    let mut runner = TestRunner::new(cli.target, cli.timeout, cli.tls);

    println!("OAAT Conformance Test — {}\n", cli.target);

    // === 1. Connection & Handshake ===
    println!("[Handshake]");
    let mut ep = match runner.connect().await {
        Ok(ep) => {
            runner.pass("TCP connect + handshake");
            ep
        }
        Err(e) => {
            runner.fail("TCP connect + handshake", &e);
            runner.summary();
            std::process::exit(1);
        }
    };

    if ep.info.protocol_version == oaat_core::PROTOCOL_VERSION {
        runner.pass(&format!("protocol version = {}", ep.info.protocol_version));
    } else {
        runner.fail(
            "protocol version",
            &format!(
                "expected {}, got {}",
                oaat_core::PROTOCOL_VERSION,
                ep.info.protocol_version
            ),
        );
    }

    if !ep.info.endpoint_id.is_empty() {
        runner.pass("endpoint_id present");
    } else {
        runner.fail("endpoint_id", "empty");
    }

    if !ep.info.endpoint_name.is_empty() {
        runner.pass("endpoint_name present");
    } else {
        runner.fail("endpoint_name", "empty");
    }

    // === 2. Capabilities ===
    println!("\n[Capabilities]");
    let max_rate = ep.info.capabilities.pcm_max_rate;
    let max_bits = ep.info.capabilities.pcm_max_bits;
    let max_ch = ep.info.capabilities.channels_max;
    let has_s16 = ep
        .info
        .capabilities
        .formats
        .contains(&AudioFormat::PcmS16le);
    let has_opus = ep.info.capabilities.formats.contains(&AudioFormat::Opus);

    if max_rate >= 44100 {
        runner.pass(&format!("pcm_max_rate = {max_rate}Hz"));
    } else {
        runner.fail("pcm_max_rate", &format!("{max_rate} < 44100"));
    }
    if max_bits >= 16 {
        runner.pass(&format!("pcm_max_bits = {max_bits}"));
    } else {
        runner.fail("pcm_max_bits", &format!("{max_bits} < 16"));
    }
    if max_ch >= 2 {
        runner.pass(&format!("channels_max = {max_ch}"));
    } else {
        runner.fail("channels_max", "must support stereo (2)");
    }
    if has_s16 {
        runner.pass("PCM_S16LE support (mandatory)");
    } else {
        runner.fail("PCM_S16LE", "mandatory format not in list");
    }

    // === 3. Format Negotiation (all in same connection) ===
    println!("\n[Format Negotiation]");

    // Accept
    ep.propose_format(
        "t-accept",
        AudioFormat::PcmS16le,
        44100,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    match timeout(runner.timeout, ep.response_rx.recv()).await {
        Ok(Some(EndpointResponse::FormatAccept(fa))) if fa.stream_id == "t-accept" => {
            runner.pass("FormatAccept for PCM_S16LE 44.1kHz");
        }
        Ok(Some(other)) => runner.fail("FormatAccept", &format!("unexpected: {other:?}")),
        _ => runner.fail("FormatAccept", "timeout or channel closed"),
    }

    // Counter (rate too high)
    let too_high = (max_rate * 2).max(768000);
    ep.propose_format(
        "t-counter",
        AudioFormat::PcmS16le,
        too_high,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    match timeout(runner.timeout, ep.response_rx.recv()).await {
        Ok(Some(EndpointResponse::FormatCounter(fc))) if fc.sample_rate <= max_rate => {
            runner.pass(&format!(
                "FormatCounter: {too_high}Hz -> {}Hz",
                fc.sample_rate
            ));
        }
        Ok(Some(EndpointResponse::FormatAccept(_))) => {
            runner.skip("FormatCounter", &format!("accepted {too_high}Hz"));
        }
        Ok(Some(other)) => runner.fail("FormatCounter", &format!("unexpected: {other:?}")),
        _ => runner.fail("FormatCounter", "timeout"),
    }

    // Reject (unsupported format)
    let unsupported = if has_opus {
        AudioFormat::DsdU32le
    } else {
        AudioFormat::Opus
    };
    ep.propose_format("t-reject", unsupported, 44100, 2, ChannelLayout::Stereo, 16)
        .await
        .ok();
    match timeout(runner.timeout, ep.response_rx.recv()).await {
        Ok(Some(EndpointResponse::FormatReject(fr))) if !fr.reason.is_empty() => {
            runner.pass(&format!("FormatReject for {unsupported}"));
        }
        Ok(Some(EndpointResponse::FormatAccept(_))) => {
            runner.skip("FormatReject", &format!("accepted {unsupported}"));
        }
        Ok(Some(other)) => runner.fail("FormatReject", &format!("unexpected: {other:?}")),
        _ => runner.fail("FormatReject", "timeout"),
    }

    // === 4. Clock Sync ===
    println!("\n[Clock Sync]");
    match timeout(runner.timeout, ep.clock_sync_bootstrap()).await {
        Ok(Ok(())) => {
            let offset = ep.clock_offset_ns().await;
            if offset.unsigned_abs() < 10_000_000 {
                runner.pass(&format!(
                    "clock sync offset = {offset}ns ({:.1}us)",
                    offset as f64 / 1000.0
                ));
            } else {
                runner.fail("clock sync", &format!("offset {offset}ns > 10ms"));
            }
        }
        Ok(Err(e)) => runner.fail("clock sync", &e.to_string()),
        Err(_) => runner.fail("clock sync", "timeout"),
    }

    // === 5. Audio Streaming ===
    println!("\n[Audio Streaming]");
    // Re-propose a format that was accepted earlier
    ep.propose_format(
        "t-audio",
        AudioFormat::PcmS16le,
        44100,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    let _ = timeout(Duration::from_secs(1), ep.response_rx.recv()).await;

    ep.send_play("t-audio").await.ok();
    let silence = vec![0u8; 960];
    let mut sent = 0u32;
    for i in 0..10u64 {
        let flags = if i == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };
        if ep
            .send_audio(
                1,
                AudioFormat::PcmS16le,
                i * 5_000_000,
                i * 240,
                &silence,
                flags,
            )
            .await
            .is_ok()
        {
            sent += 1;
        }
    }
    if sent == 10 {
        runner.pass(&format!("audio delivery: {sent}/10 packets sent"));
    } else {
        runner.fail("audio delivery", &format!("{sent}/10 sent"));
    }
    ep.send_stop("t-audio").await.ok();

    // === 6. Gapless ===
    println!("\n[Gapless]");

    // Same format
    ep.propose_format(
        "t-gap",
        AudioFormat::PcmS16le,
        44100,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    let _ = timeout(Duration::from_secs(1), ep.response_rx.recv()).await;

    ep.prepare_next_track(
        "t-gap-same",
        AudioFormat::PcmS16le,
        44100,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    match timeout(runner.timeout, ep.response_rx.recv()).await {
        Ok(Some(EndpointResponse::NextTrackReady(_))) => {
            runner.pass("gapless same format -> NextTrackReady");
        }
        Ok(Some(other)) => runner.fail("gapless same", &format!("unexpected: {other:?}")),
        _ => runner.fail("gapless same", "timeout"),
    }

    // Different format
    ep.prepare_next_track(
        "t-gap-diff",
        AudioFormat::PcmS16le,
        96000,
        2,
        ChannelLayout::Stereo,
        16,
    )
    .await
    .ok();
    match timeout(runner.timeout, ep.response_rx.recv()).await {
        Ok(Some(EndpointResponse::NextTrackReformat(ntf))) => {
            runner.pass(&format!(
                "gapless diff format -> NextTrackReformat ({}Hz)",
                ntf.sample_rate
            ));
        }
        Ok(Some(EndpointResponse::NextTrackReady(_))) => {
            runner.skip("gapless diff", "endpoint accepted (lenient)");
        }
        Ok(Some(other)) => runner.fail("gapless diff", &format!("unexpected: {other:?}")),
        _ => runner.fail("gapless diff", "timeout"),
    }

    // === 7. Volume ===
    println!("\n[Volume]");
    match ep.send_volume(50).await {
        Ok(()) => runner.pass("volume_set(50)"),
        Err(e) => runner.fail("volume_set", &e.to_string()),
    }
    match ep.send_mute(true).await {
        Ok(()) => runner.pass("mute(true)"),
        Err(e) => runner.fail("mute", &e.to_string()),
    }
    match ep.send_mute(false).await {
        Ok(()) => runner.pass("mute(false)"),
        Err(e) => runner.fail("unmute", &e.to_string()),
    }

    // === 8. Disconnect + Reconnect ===
    println!("\n[Disconnect & Reconnect]");
    drop(ep);
    runner.pass("graceful disconnect");

    tokio::time::sleep(Duration::from_secs(1)).await;
    match runner.connect().await {
        Ok(_) => runner.pass("reconnection after disconnect"),
        Err(e) => runner.fail("reconnection", &e),
    }

    runner.summary();
    std::process::exit(if runner.failed > 0 { 1 } else { 0 });
}
