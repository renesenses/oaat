use std::f64::consts::PI;
use std::net::SocketAddr;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use oaat_controller::{ConnectedEndpoint, ControllerConfig, ControllerDiscovery};
use oaat_core::ChannelLayout;
use oaat_core::format::AudioFormat;
use oaat_core::message::{EndpointCapabilities, TrackMetadata};
use oaat_core::wire::PacketFlags;
use oaat_endpoint::discovery::EndpointAnnouncement;
use oaat_endpoint::transport::{PlaybackCommand, VolumeCommand};
#[cfg(target_os = "linux")]
use oaat_endpoint::{AlsaDirectOutput, EndpointConfig, EndpointEvent, EndpointTransport};
#[cfg(not(target_os = "linux"))]
use oaat_endpoint::{CpalOutput, EndpointConfig, EndpointEvent, EndpointTransport};
#[cfg(feature = "web-ui")]
use oaat_endpoint::web_ui;

mod config;
use config::EndpointFileConfig;

const PID_FILE: &str = "/tmp/tune-bridge.pid";
const DEFAULT_WEB_UI_PORT: u16 = 9741;

#[derive(Parser)]
#[command(
    name = "tune-bridge",
    about = "Tune Bridge — lightweight audio bridge for USB DACs",
    long_about = "Tune Bridge receives audio streams from a Tune server via the OAAT protocol \
                  and outputs them to a local USB DAC. Think Roon Bridge, but open-source.",
    version = env!("CARGO_PKG_VERSION"),
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start Tune Bridge endpoint (receiver/renderer) with audio output
    Endpoint {
        /// Bridge display name (default: system hostname)
        #[arg(short, long)]
        name: Option<String>,
        /// TCP control port (default: 9740)
        #[arg(long)]
        port: Option<u16>,
        /// Path to TOML config file
        #[arg(short, long)]
        config: Option<String>,
        /// Run as a background daemon (write PID to /tmp/tune-bridge.pid)
        #[arg(long)]
        daemon: bool,
        /// Select audio output device by name (default: auto-detect USB DAC)
        #[arg(long)]
        audio_device: Option<String>,
        /// List available audio output devices and exit
        #[arg(long)]
        list_devices: bool,
        /// Enable TLS 1.3 on the control channel (self-signed cert, TOFU)
        #[arg(long)]
        tls: bool,
        /// Disable the web status UI
        #[arg(long)]
        no_web_ui: bool,
        /// Web UI port (default: 9741)
        #[arg(long)]
        web_port: Option<u16>,
    },

    /// Start an OAAT controller and stream audio (file or sine wave)
    Controller {
        /// Controller display name
        #[arg(short, long, default_value = "OAAT Controller")]
        name: String,
        /// Connect directly to this address instead of using mDNS
        #[arg(short, long)]
        target: Option<SocketAddr>,
        /// Audio file to stream (WAV or FLAC)
        #[arg(short, long)]
        file: Option<String>,
        /// Sine wave frequency in Hz (used when no --file)
        #[arg(long, default_value = "440")]
        freq: f64,
        /// Duration in seconds (sine wave only)
        #[arg(long, default_value = "5")]
        duration: u64,
        /// Enable TLS 1.3 on the control channel (TOFU client)
        #[arg(long)]
        tls: bool,
    },

    /// Multi-room: stream synchronized audio to multiple endpoints
    Multiroom {
        /// Endpoint addresses (e.g. 192.168.1.10:9740 192.168.1.11:9740)
        #[arg(required = true)]
        targets: Vec<SocketAddr>,
        /// Sine wave frequency in Hz
        #[arg(long, default_value = "440")]
        freq: f64,
        /// Duration in seconds
        #[arg(long, default_value = "5")]
        duration: u64,
    },

    /// Discover OAAT endpoints on the network
    Discover {
        /// Timeout in seconds
        #[arg(short, long, default_value = "5")]
        timeout: u64,
    },
}

/// Write PID file for daemon mode.
fn write_pid_file() {
    let pid = std::process::id();
    if let Err(e) = std::fs::write(PID_FILE, pid.to_string()) {
        warn!(error = %e, path = PID_FILE, "failed to write PID file");
    } else {
        info!(pid, path = PID_FILE, "PID file written");
    }
}

/// Remove PID file on shutdown.
fn remove_pid_file() {
    if std::path::Path::new(PID_FILE).exists()
        && let Err(e) = std::fs::remove_file(PID_FILE)
    {
        warn!(error = %e, "failed to remove PID file");
    }
}

/// Get the system hostname for the default bridge name.
fn system_hostname() -> String {
    hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oaat=info,tune_bridge=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Endpoint {
            name,
            port,
            config,
            daemon,
            audio_device,
            list_devices,
            tls,
            no_web_ui,
            web_port,
        } => {
            if list_devices {
                #[cfg(not(target_os = "linux"))]
                {
                    let devices = oaat_endpoint::CpalOutput::list_devices();
                    let default = oaat_endpoint::CpalOutput::default_device_name();
                    println!("Available audio output devices:\n");
                    for d in &devices {
                        let marker = if Some(d.as_str()) == default.as_deref() { " (default)" } else { "" };
                        println!("  {d}{marker}");
                    }
                    if devices.is_empty() {
                        println!("  (no devices found)");
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    let devices = AlsaDirectOutput::list_devices();
                    println!("Available audio output devices (ALSA):\n");
                    for d in &devices {
                        println!("  {d}");
                    }
                    if devices.is_empty() {
                        println!("  (no devices found — check `aplay -l`)");
                    }
                    println!("\nUsable names for --audio-device:");
                    for d in &devices {
                        if let Some(card) = d.split(':').nth(0) {
                            if let Some(_num) = card.strip_prefix("card ").or_else(|| card.strip_prefix("carte ")) {
                                if let Some(short_name) = d.split(':').nth(1).map(|s| s.trim().split_whitespace().next()).flatten() {
                                    println!("  sysdefault:CARD={short_name}");
                                }
                            }
                        }
                    }
                }
                std::process::exit(0);
            }

            // Daemon mode: fork to background on Unix
            if daemon {
                #[cfg(unix)]
                {
                    // Fork to background using double-fork technique
                    unsafe {
                        let pid = libc::fork();
                        if pid < 0 {
                            eprintln!("fork failed");
                            std::process::exit(1);
                        }
                        if pid > 0 {
                            // Parent: print child PID and exit
                            println!("Tune Bridge started in background (PID: {})", pid);
                            std::process::exit(0);
                        }
                        // Child: create new session
                        libc::setsid();
                    }
                }
                write_pid_file();
            }

            let file_config = EndpointFileConfig::load(config.as_deref()).unwrap_or_else(|e| {
                eprintln!("config error: {e}");
                std::process::exit(1);
            });

            // CLI args override config file values
            let ep_name = name.unwrap_or_else(|| {
                let cfg_name = &file_config.endpoint.name;
                if cfg_name == "OAAT Endpoint" {
                    // Default was not overridden in config -- use hostname
                    system_hostname()
                } else {
                    cfg_name.clone()
                }
            });
            let ep_port = port.unwrap_or(file_config.endpoint.port);

            // Audio device: CLI > config > auto-detect USB DAC
            let ep_audio_device = audio_device.or(file_config.endpoint.audio_device).or_else(|| {
                #[cfg(not(target_os = "linux"))]
                { CpalOutput::auto_detect_usb_dac() }
                #[cfg(target_os = "linux")]
                { AlsaDirectOutput::auto_detect_usb_dac() }
            });

            let ep_tls = tls || file_config.endpoint.tls;
            let ep_web_port = web_port.unwrap_or(DEFAULT_WEB_UI_PORT);
            let ep_web_ui = !no_web_ui;

            run_endpoint(
                ep_name,
                ep_port,
                ep_audio_device,
                daemon,
                ep_tls,
                &file_config.capabilities,
                &file_config.dac,
                ep_web_ui,
                ep_web_port,
            )
            .await;

            if daemon {
                remove_pid_file();
            }
        }
        Command::Controller {
            name,
            target,
            file,
            freq,
            duration,
            tls,
        } => {
            if let Some(ref path) = file {
                run_controller_file(name, target, path, tls).await
            } else {
                run_controller(name, target, freq, duration, tls).await
            }
        }
        Command::Multiroom {
            targets,
            freq,
            duration,
        } => run_multiroom(targets, freq, duration).await,
        Command::Discover { timeout } => run_discover(timeout),
    }
}

fn load_or_create_endpoint_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    let id_file = std::path::PathBuf::from(format!("/var/tmp/oaat-{slug}.id"));

    if let Ok(id) = std::fs::read_to_string(&id_file) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            tracing::info!(id = %id, path = %id_file.display(), "loaded persistent endpoint ID");
            return id;
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = std::fs::write(&id_file, &id) {
        tracing::warn!(error = %e, path = %id_file.display(), "could not persist endpoint ID");
    } else {
        tracing::info!(id = %id, path = %id_file.display(), "created persistent endpoint ID");
    }
    id
}

#[allow(clippy::too_many_arguments)]
async fn run_endpoint(
    name: String,
    port: u16,
    audio_device: Option<String>,
    daemon: bool,
    tls: bool,
    caps_config: &config::CapabilitiesSection,
    dac_config: &config::DacSection,
    web_ui_enabled: bool,
    web_ui_port: u16,
) {
    let endpoint_id = load_or_create_endpoint_id(&name);
    let control_addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
    let audio_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let clock_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();

    // List available audio output devices at startup for diagnostics
    let available_devices;
    {
        #[cfg(target_os = "linux")]
        { available_devices = AlsaDirectOutput::list_devices(); }
        #[cfg(not(target_os = "linux"))]
        { available_devices = CpalOutput::list_devices(); }
        for (i, dname) in available_devices.iter().enumerate() {
            info!(index = i, device = %dname, "audio_device_available");
        }
        #[cfg(target_os = "linux")]
        let default_name = AlsaDirectOutput::default_device_name().unwrap_or_else(|| "(none)".into());
        #[cfg(not(target_os = "linux"))]
        let default_name = CpalOutput::default_device_name().unwrap_or_else(|| "(none)".into());
        info!(default = %default_name, "audio_device_default");
        if let Some(ref pref) = audio_device {
            info!(preferred = %pref, "audio_device_configured");
        }
    }

    // Build capabilities from config
    let mut formats = vec![
        AudioFormat::PcmS16le,
        AudioFormat::PcmS24le,
        AudioFormat::PcmS32le,
    ];
    if caps_config.flac {
        formats.push(AudioFormat::Flac);
    }
    if caps_config.dsd {
        formats.push(AudioFormat::DsdU8);
        formats.push(AudioFormat::DsdU16le);
        formats.push(AudioFormat::DsdU32le);
    }

    let capabilities = EndpointCapabilities {
        pcm_max_rate: caps_config.pcm_max_rate,
        pcm_max_bits: caps_config.pcm_max_bits,
        dsd_max_rate: if caps_config.dsd { Some(64) } else { None },
        channels_max: caps_config.channels_max,
        formats,
        volume: None,
        gapless: true,
        seek: true,
    };

    // Build mDNS capabilities string
    let mdns_caps = oaat_core::capability::Capabilities {
        pcm_max_rate_khz: caps_config.pcm_max_rate / 1000,
        pcm_max_bits: caps_config.pcm_max_bits,
        dsd_max_multiplier: if caps_config.dsd { Some(64) } else { None },
        flac: caps_config.flac,
        opus: false,
    };

    // Bind TCP first to get actual port
    let tcp = tokio::net::TcpListener::bind(control_addr).await.unwrap();
    let actual_port = tcp.local_addr().unwrap().port();
    drop(tcp);

    let control_addr: SocketAddr = format!("0.0.0.0:{actual_port}").parse().unwrap();

    // Register mDNS
    let mdns = mdns_sd::ServiceDaemon::new().expect("failed to create mDNS daemon");
    let announcement = EndpointAnnouncement {
        instance_name: name.clone(),
        port: actual_port,
        endpoint_id: endpoint_id.clone(),
        capabilities: mdns_caps,
        channels_max: caps_config.channels_max,
        volume_type: Some(if dac_config.hardware_volume { "hw" } else { "sw" }.into()),
        model: None,
        vendor: Some("MozAIk Labs".into()),
        firmware: Some(env!("CARGO_PKG_VERSION").into()),
    };
    if let Err(e) = announcement.register(&mdns) {
        warn!(error = %e, "mDNS registration failed, continuing without discovery");
    }

    // Start web UI
    #[cfg(feature = "web-ui")]
    let (status_handle, device_switch_rx) = {
        let initial_device = audio_device
            .as_deref()
            .unwrap_or("system default")
            .to_string();
        let initial_status = web_ui::BridgeStatus {
            bridge_name: name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            current_device: initial_device,
            available_devices: available_devices.clone(),
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
        };
        let (handle, _reader) = web_ui::BridgeStatusHandle::new(initial_status);
        let (switch_tx, switch_rx) = mpsc::channel::<String>(8);
        if web_ui_enabled {
            web_ui::start_web_ui(web_ui_port, handle.clone(), switch_tx).await;
        }
        (handle, switch_rx)
    };
    #[cfg(not(feature = "web-ui"))]
    let (_, mut device_switch_rx) = mpsc::channel::<String>(1);

    #[cfg(feature = "web-ui")]
    let mut device_switch_rx = device_switch_rx;

    if daemon {
        info!(
            name = %name, port = actual_port, id = %endpoint_id,
            web_ui = web_ui_enabled, web_port = web_ui_port,
            "Tune Bridge started (daemon mode)"
        );
    } else {
        println!();
        println!("  Tune Bridge '{name}'");
        println!("  OAAT port: {actual_port}  |  ID: {endpoint_id}");
        if web_ui_enabled {
            println!("  Web UI: http://localhost:{web_ui_port}");
        }
        if let Some(ref dev) = audio_device {
            println!("  Audio device: {dev}");
        } else {
            println!("  Audio device: auto-detect");
        }
        println!();
        println!("  Waiting for Tune server connection...");
        println!();
    }

    let ep_config = EndpointConfig {
        endpoint_id,
        endpoint_name: name,
        control_addr,
        audio_addr,
        clock_addr,
        capabilities,
        buffer_size_ms: 1000,
        tls,
    };

    let (event_tx, mut event_rx) = mpsc::channel(256);
    let (_ctrl_tx, ctrl_rx) = mpsc::channel(32);

    tokio::spawn(async move {
        if let Err(e) = EndpointTransport::run(ep_config, event_tx, ctrl_rx).await {
            error!(error = %e, "endpoint transport error");
        }
    });

    #[cfg(target_os = "linux")]
    let mut audio = AlsaDirectOutput::new();
    #[cfg(not(target_os = "linux"))]
    let mut audio = CpalOutput::new();
    let mut packet_count: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut current_audio_device = audio_device;

    // Initialize hardware DAC mixer (Linux only)
    #[cfg(target_os = "linux")]
    let alsa_mixer = if dac_config.hardware_volume {
        let mixer = oaat_endpoint::AlsaMixer::new(dac_config.card);
        mixer.init(dac_config.fir_filter.as_deref());
        Some(mixer)
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let alsa_mixer: Option<()> = None;

    // Graceful shutdown: listen for SIGTERM/SIGINT
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");
    let sigint = tokio::signal::ctrl_c();
    tokio::pin!(sigint);

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else { break };
                match event {
                    EndpointEvent::Connected {
                        controller_id,
                        controller_name,
                    } => {
                        #[cfg(feature = "web-ui")]
                        {
                            let ctrl_name = controller_name.clone();
                            status_handle.update(|s| {
                                s.connected = true;
                                s.controller_name = Some(ctrl_name);
                            }).await;
                        }
                        if daemon {
                            info!(controller = %controller_name, id = %controller_id, "controller connected");
                        } else {
                            println!("Connected to controller '{controller_name}' ({controller_id})");
                        }
                    }
                    EndpointEvent::FormatAccepted { stream_id } => {
                        if daemon {
                            info!(stream_id, "format accepted");
                        } else {
                            println!("Format accepted: {stream_id}");
                        }
                    }
                    EndpointEvent::FormatRejected { stream_id, reason } => {
                        if daemon {
                            warn!(stream_id, reason, "format rejected");
                        } else {
                            eprintln!("Format rejected: {stream_id} -- {reason}");
                        }
                    }
                    EndpointEvent::FormatProposed(fp) => {
                        if daemon {
                            info!(
                                format = %fp.format,
                                sample_rate = fp.sample_rate,
                                channels = fp.channels,
                                bits = fp.bits_per_sample,
                                "format proposed"
                            );
                        } else {
                            println!(
                                "Format: {} {}Hz {}ch {}bit",
                                fp.format, fp.sample_rate, fp.channels, fp.bits_per_sample
                            );
                        }
                        match audio.configure_with_device(fp.format, fp.sample_rate, fp.channels, current_audio_device.as_deref()) {
                            Ok(()) => {
                                #[cfg(feature = "web-ui")]
                                {
                                    let dev_name = audio.current_device_name().unwrap_or("unknown").to_string();
                                    let fmt_s = fp.format.to_string();
                                    let sr = fp.sample_rate;
                                    let bits = fp.bits_per_sample;
                                    let ch = fp.channels;
                                    status_handle.update(move |s| {
                                        s.current_device = dev_name;
                                        s.stream_format = Some(fmt_s);
                                        s.stream_sample_rate = Some(sr);
                                        s.stream_bits = Some(bits);
                                        s.stream_channels = Some(ch);
                                    }).await;
                                }
                                if daemon {
                                    info!("audio output configured");
                                } else {
                                    println!("Audio output configured");
                                }
                            }
                            Err(e) => {
                                if daemon {
                                    error!(error = %e, "audio output configuration failed");
                                } else {
                                    eprintln!("Audio output error: {e}");
                                }
                            }
                        }
                    }
                    EndpointEvent::AudioPacket { header, payload } => {
                        packet_count += 1;
                        total_bytes += payload.len() as u64;
                        if !payload.is_empty() {
                            audio.write_audio(&payload);
                        }
                        if !daemon && (packet_count.is_multiple_of(200) || header.flags.contains(PacketFlags::FIRST_PACKET)) {
                            println!(
                                "  [{packet_count}] seq={} buf={}",
                                header.sequence,
                                audio.buffer_level(),
                            );
                        }
                    }
                    EndpointEvent::Playback(cmd) => match cmd {
                        PlaybackCommand::Play(id) => {
                            if daemon {
                                info!(stream_id = %id, "play");
                            } else {
                                println!("Play: {id}");
                            }
                            audio.play();
                        }
                        PlaybackCommand::Pause(id) => {
                            if daemon {
                                info!(stream_id = %id, "pause");
                            } else {
                                println!("Pause: {id}");
                            }
                            audio.pause();
                        }
                        PlaybackCommand::Stop(id) => {
                            if daemon {
                                info!(stream_id = %id, packets = packet_count, bytes = total_bytes, "stop");
                            } else {
                                println!("Stop: {id}");
                                println!(
                                    "Session: {packet_count} packets, {:.1} KB",
                                    total_bytes as f64 / 1024.0
                                );
                            }
                            audio.stop();
                            #[cfg(feature = "web-ui")]
                            status_handle.update(|s| {
                                s.stream_format = None;
                                s.stream_sample_rate = None;
                                s.stream_bits = None;
                                s.stream_channels = None;
                                s.track_title = None;
                                s.track_artist = None;
                                s.track_album = None;
                                s.artwork_url = None;
                                s.playing = false;
                            }).await;
                        }
                        PlaybackCommand::Seek(id, pos) => {
                            if daemon {
                                info!(stream_id = %id, position_ms = pos, "seek");
                            } else {
                                println!("Seek: {id} -> {pos}ms");
                            }
                        }
                    },
                    EndpointEvent::Metadata(m) => {
                        if daemon {
                            info!(
                                artist = %m.track.artist,
                                title = %m.track.title,
                                album = %m.track.album,
                                "now playing"
                            );
                        } else {
                            println!(
                                "Now playing: {} -- {} [{}]",
                                m.track.artist, m.track.title, m.track.album
                            );
                            if let Some(ref fmt) = m.track.format {
                                println!("  Format: {fmt}");
                            }
                        }
                        #[cfg(feature = "web-ui")]
                        {
                            let title = m.track.title.clone();
                            let artist = m.track.artist.clone();
                            let album = m.track.album.clone();
                            let artwork = m.track.artwork_url.clone();
                            status_handle.update(move |s| {
                                s.track_title = Some(title);
                                s.track_artist = Some(artist);
                                s.track_album = Some(album);
                                s.artwork_url = artwork;
                                s.playing = true;
                            }).await;
                        }
                    }
                    EndpointEvent::NextTrackReady { stream_id } => {
                        if daemon {
                            info!(stream_id, "gapless ready");
                        } else {
                            println!("Gapless ready: {stream_id} (same format, seamless transition)");
                        }
                    }
                    EndpointEvent::NextTrackReformat {
                        stream_id,
                        format,
                        sample_rate,
                    } => {
                        if daemon {
                            info!(stream_id, format = %format, sample_rate, "reformat for next track");
                        } else {
                            println!(
                                "Reformat needed: {stream_id} -> {format} {sample_rate}Hz (reconfiguring output)"
                            );
                        }
                        match audio.configure_with_device(format, sample_rate, 2, current_audio_device.as_deref()) {
                            Ok(()) => {
                                #[cfg(feature = "web-ui")]
                                {
                                    let dev_name = audio.current_device_name().unwrap_or("unknown").to_string();
                                    let fmt_s = format.to_string();
                                    status_handle.update(move |s| {
                                        s.current_device = dev_name;
                                        s.stream_format = Some(fmt_s);
                                        s.stream_sample_rate = Some(sample_rate);
                                        s.stream_channels = Some(2);
                                    }).await;
                                }
                                if !daemon {
                                    println!("Audio output reconfigured for next track");
                                }
                            }
                            Err(e) => {
                                if daemon {
                                    error!(error = %e, "audio reconfigure failed");
                                } else {
                                    eprintln!("Audio reconfigure error: {e}");
                                }
                            }
                        }
                    }
                    EndpointEvent::Volume(cmd) => match cmd {
                        VolumeCommand::Set(level) => {
                            #[cfg(target_os = "linux")]
                            if let Some(ref mixer) = alsa_mixer {
                                mixer.set_volume(level);
                            } else {
                                audio.set_volume(level);
                            }
                            #[cfg(not(target_os = "linux"))]
                            audio.set_volume(level);
                            if daemon {
                                info!(level, hw = alsa_mixer.is_some(), "volume set");
                            } else {
                                println!("Volume: {level}%");
                            }
                        }
                        VolumeCommand::Get => {}
                        VolumeCommand::Mute(muted) => {
                            #[cfg(target_os = "linux")]
                            if let Some(ref mixer) = alsa_mixer {
                                mixer.set_mute(muted);
                            } else {
                                audio.set_mute(muted);
                            }
                            #[cfg(not(target_os = "linux"))]
                            audio.set_mute(muted);
                            if daemon {
                                info!(muted, "mute toggled");
                            } else {
                                println!("Mute: {muted}");
                            }
                        }
                    },
                    EndpointEvent::ZoneAssigned { zone_id } => {
                        if daemon {
                            info!(zone_id = %zone_id, "assigned to zone");
                        } else {
                            println!("Zone assigned: {zone_id}");
                        }
                    }
                    EndpointEvent::ZoneUpdated { zone_id, endpoint_ids } => {
                        if daemon {
                            info!(zone_id = %zone_id, members = endpoint_ids.len(), "zone updated");
                        } else {
                            println!("Zone {zone_id}: {} member(s)", endpoint_ids.len());
                        }
                    }
                    EndpointEvent::ZoneReleased { zone_id } => {
                        if daemon {
                            info!(zone_id = %zone_id, "released from zone");
                        } else {
                            println!("Zone released: {zone_id}");
                        }
                    }
                    EndpointEvent::Disconnected => {
                        audio.stop();
                        #[cfg(feature = "web-ui")]
                        status_handle.update(|s| {
                            s.connected = false;
                            s.controller_name = None;
                            s.stream_format = None;
                            s.stream_sample_rate = None;
                            s.stream_bits = None;
                            s.stream_channels = None;
                        }).await;
                        if daemon {
                            info!(packets = packet_count, bytes = total_bytes, "controller disconnected");
                        } else {
                            println!("\nDisconnected. {packet_count} packets, {total_bytes} bytes total.");
                        }
                        // In daemon mode, don't break — keep waiting for the next connection
                        if !daemon {
                            break;
                        }
                    }
                    EndpointEvent::Error(e) => {
                        error!(error = %e, "endpoint error");
                    }
                }
            }
            // Handle device switch requests from the web UI
            Some(new_device) = device_switch_rx.recv() => {
                info!(device = %new_device, "switching audio device via web UI");
                current_audio_device = Some(new_device.clone());
                #[cfg(feature = "web-ui")]
                status_handle.update(|s| {
                    s.current_device = new_device;
                }).await;
            }
            _ = &mut sigint => {
                info!("Tune Bridge shutting down gracefully");
                audio.stop();
                let _ = mdns.shutdown();
                remove_pid_file();
                break;
            }
            _ = sigterm.recv() => {
                info!("Tune Bridge shutting down gracefully (SIGTERM)");
                audio.stop();
                let _ = mdns.shutdown();
                remove_pid_file();
                break;
            }
        }
    }
}

async fn run_controller(
    name: String,
    target: Option<SocketAddr>,
    freq: f64,
    duration: u64,
    tls: bool,
) {
    let controller_id = uuid::Uuid::new_v4().to_string();
    let endpoint_addr = match target {
        Some(addr) => {
            println!("Connecting directly to {addr}...");
            addr
        }
        None => {
            println!("Discovering OAAT endpoints via mDNS...");
            let discovery = ControllerDiscovery::new().expect("failed to create mDNS");
            match discovery.find_first(Duration::from_secs(10)) {
                Some(ep) => {
                    println!("Found endpoint '{}' at {}", ep.name, ep.addr);
                    if let Some(ref caps) = ep.capabilities {
                        println!("  Capabilities: {caps}");
                    }
                    ep.addr
                }
                None => {
                    eprintln!("No OAAT endpoints found. Use --target to connect directly.");
                    return;
                }
            }
        }
    };

    let config = ControllerConfig {
        controller_id,
        controller_name: name.clone(),
        features: vec![],
        clock_port: oaat_core::DEFAULT_CLOCK_PORT,
        tls,
    };

    println!("Connecting{}...", if tls { " (TLS)" } else { "" });
    let mut endpoint = match ConnectedEndpoint::connect(&config, endpoint_addr).await {
        Ok(ep) => ep,
        Err(e) => {
            eprintln!("Connection failed: {e}");
            return;
        }
    };

    println!(
        "Connected to '{}' ({})",
        endpoint.info.endpoint_name, endpoint.info.endpoint_id
    );
    println!(
        "  PCM max: {}Hz/{}bit, {}ch",
        endpoint.info.capabilities.pcm_max_rate,
        endpoint.info.capabilities.pcm_max_bits,
        endpoint.info.capabilities.channels_max
    );

    // Clock sync
    println!("\nClock sync...");
    if let Err(e) = endpoint.clock_sync_bootstrap().await {
        warn!(error = %e, "clock sync failed");
    }
    let offset = endpoint.clock_offset_ns().await;
    println!("  Offset: {offset}ns\n");

    let sample_rate = 44100u32;
    let channels = 2u8;
    let bits = 16u8;
    let format = AudioFormat::PcmS16le;
    let stream_id = "sine-demo";

    // Format negotiation
    println!("Format: {format} {sample_rate}Hz {channels}ch {bits}bit");
    endpoint
        .propose_format(
            stream_id,
            format,
            sample_rate,
            channels,
            ChannelLayout::Stereo,
            bits,
        )
        .await
        .unwrap();

    // Metadata
    endpoint
        .send_metadata(TrackMetadata {
            title: format!("{freq}Hz Sine Wave"),
            artist: "OAAT Demo".into(),
            album: "Protocol Test".into(),
            duration_ms: duration * 1000,
            artwork_url: None,
            format: Some(format!("PCM {bits}/{}", sample_rate / 1000)),
        })
        .await
        .unwrap();

    // Play
    endpoint.send_play(stream_id).await.unwrap();
    println!("Streaming {freq}Hz for {duration}s...\n");

    // Generate and send
    let samples_per_packet = 480;
    let bytes_per_sample = 2 * channels as usize;
    let total_samples = sample_rate as u64 * duration;
    let mut sample_offset: u64 = 0;
    let start = std::time::Instant::now();

    while sample_offset < total_samples {
        let chunk = samples_per_packet.min((total_samples - sample_offset) as usize);
        let mut payload = Vec::with_capacity(chunk * bytes_per_sample);

        for i in 0..chunk {
            let t = (sample_offset + i as u64) as f64 / sample_rate as f64;
            let sample = (0.8 * (2.0 * PI * freq * t).sin() * i16::MAX as f64) as i16;
            payload.extend_from_slice(&sample.to_le_bytes());
            payload.extend_from_slice(&sample.to_le_bytes());
        }

        let pts_ns = (sample_offset as f64 / sample_rate as f64 * 1e9) as u64;
        let flags = if sample_offset == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };

        endpoint
            .send_audio(1, format, pts_ns, sample_offset, &payload, flags)
            .await
            .unwrap();

        sample_offset += chunk as u64;

        let expected =
            Duration::from_nanos((sample_offset as f64 / sample_rate as f64 * 1e9) as u64);
        let elapsed = start.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }

        if sample_offset.is_multiple_of(sample_rate as u64) || sample_offset >= total_samples {
            let secs = sample_offset / sample_rate as u64;
            let pct = (sample_offset as f64 / total_samples as f64 * 100.0) as u32;
            println!("  {secs}s / {duration}s ({pct}%)");
        }
    }

    // -- Gapless transition: prepare a second tone (880Hz) with the same format --
    let freq2 = 880.0;
    let gapless_stream_id = "sine-gapless";
    println!("\nPreparing gapless transition to {freq2}Hz...");
    endpoint
        .prepare_next_track(
            gapless_stream_id,
            format,
            sample_rate,
            channels,
            ChannelLayout::Stereo,
            bits,
        )
        .await
        .unwrap();

    // Give endpoint time to respond (NextTrackReady expected for same format)
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Drain response from endpoint
    if let Ok(Some(resp)) =
        tokio::time::timeout(Duration::from_millis(500), endpoint.response_rx.recv()).await
    {
        match resp {
            oaat_controller::EndpointResponse::NextTrackReady(ntr) => {
                println!("Endpoint ready for gapless: {}", ntr.stream_id);
            }
            oaat_controller::EndpointResponse::NextTrackReformat(ntf) => {
                println!(
                    "Endpoint needs reformat: {} -> {} {}Hz",
                    ntf.stream_id, ntf.format, ntf.sample_rate
                );
            }
            other => {
                println!("Unexpected response: {other:?}");
            }
        }
    }

    // Stream the second tone seamlessly (shorter duration: 2s)
    let duration2 = 2u64;
    let total_samples2 = sample_rate as u64 * duration2;
    let mut sample_offset2: u64 = 0;
    let start2 = std::time::Instant::now();

    endpoint
        .send_metadata(TrackMetadata {
            title: format!("{freq2}Hz Sine Wave"),
            artist: "OAAT Demo".into(),
            album: "Gapless Test".into(),
            duration_ms: duration2 * 1000,
            artwork_url: None,
            format: Some(format!("PCM {bits}/{}", sample_rate / 1000)),
        })
        .await
        .unwrap();

    println!("Streaming {freq2}Hz for {duration2}s (gapless)...\n");

    while sample_offset2 < total_samples2 {
        let chunk = samples_per_packet.min((total_samples2 - sample_offset2) as usize);
        let mut payload = Vec::with_capacity(chunk * bytes_per_sample);

        for i in 0..chunk {
            // Continue the sample timeline from the first track for seamless audio
            let t = (sample_offset + sample_offset2 + i as u64) as f64 / sample_rate as f64;
            let sample = (0.8 * (2.0 * PI * freq2 * t).sin() * i16::MAX as f64) as i16;
            payload.extend_from_slice(&sample.to_le_bytes());
            payload.extend_from_slice(&sample.to_le_bytes());
        }

        let pts_ns = ((sample_offset + sample_offset2) as f64 / sample_rate as f64 * 1e9) as u64;
        let flags = if sample_offset2 == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };

        endpoint
            .send_audio(
                2,
                format,
                pts_ns,
                sample_offset + sample_offset2,
                &payload,
                flags,
            )
            .await
            .unwrap();

        sample_offset2 += chunk as u64;

        let expected =
            Duration::from_nanos((sample_offset2 as f64 / sample_rate as f64 * 1e9) as u64);
        let elapsed = start2.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }

        if sample_offset2.is_multiple_of(sample_rate as u64) || sample_offset2 >= total_samples2 {
            let secs = sample_offset2 / sample_rate as u64;
            let pct = (sample_offset2 as f64 / total_samples2 as f64 * 100.0) as u32;
            println!("  {secs}s / {duration2}s ({pct}%)");
        }
    }

    let total_offset = sample_offset + sample_offset2;
    endpoint
        .send_audio(
            2,
            format,
            (total_offset as f64 / sample_rate as f64 * 1e9) as u64,
            total_offset,
            &[],
            PacketFlags::LAST_PACKET,
        )
        .await
        .unwrap();

    endpoint.send_stop(gapless_stream_id).await.unwrap();
    println!(
        "\nDone. {} + {} = {} total samples sent (gapless).",
        sample_offset, sample_offset2, total_offset
    );
}

async fn run_controller_file(name: String, target: Option<SocketAddr>, path: &str, tls: bool) {
    let file_data = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("Cannot read {path}: {e}");
        std::process::exit(1);
    });

    let is_wav =
        file_data.len() > 44 && &file_data[0..4] == b"RIFF" && &file_data[8..12] == b"WAVE";
    if !is_wav {
        eprintln!("Only WAV files are supported for now (got: {path})");
        std::process::exit(1);
    }

    // Parse WAV header
    let channels = u16::from_le_bytes([file_data[22], file_data[23]]) as u8;
    let sample_rate =
        u32::from_le_bytes([file_data[24], file_data[25], file_data[26], file_data[27]]);
    let bits_per_sample = u16::from_le_bytes([file_data[34], file_data[35]]) as u8;

    // Find data chunk
    let mut data_offset = 12;
    let mut data_len = 0usize;
    while data_offset + 8 < file_data.len() {
        let chunk_id = &file_data[data_offset..data_offset + 4];
        let chunk_size = u32::from_le_bytes([
            file_data[data_offset + 4],
            file_data[data_offset + 5],
            file_data[data_offset + 6],
            file_data[data_offset + 7],
        ]) as usize;
        if chunk_id == b"data" {
            data_offset += 8;
            data_len = chunk_size.min(file_data.len() - data_offset);
            break;
        }
        data_offset += 8 + chunk_size;
        if !chunk_size.is_multiple_of(2) {
            data_offset += 1;
        }
    }

    if data_len == 0 {
        eprintln!("No data chunk found in WAV file");
        std::process::exit(1);
    }

    let pcm_data = &file_data[data_offset..data_offset + data_len];
    let bytes_per_sample = (bits_per_sample as usize / 8) * channels as usize;
    let total_samples = data_len / bytes_per_sample;
    let duration_s = total_samples as f64 / sample_rate as f64;

    let format = match bits_per_sample {
        16 => AudioFormat::PcmS16le,
        24 => AudioFormat::PcmS24le,
        32 => AudioFormat::PcmS32le,
        _ => {
            eprintln!("Unsupported bit depth: {bits_per_sample}");
            std::process::exit(1);
        }
    };

    let filename = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);

    println!("File: {path}");
    println!("  {format} {sample_rate}Hz {channels}ch {bits_per_sample}bit");
    println!("  Duration: {duration_s:.1}s ({total_samples} samples)\n");

    // Connect
    let controller_id = uuid::Uuid::new_v4().to_string();
    let endpoint_addr = match target {
        Some(addr) => {
            println!("Connecting directly to {addr}...");
            addr
        }
        None => {
            println!("Discovering OAAT endpoints via mDNS...");
            let discovery = ControllerDiscovery::new().expect("failed to create mDNS");
            match discovery.find_first(Duration::from_secs(10)) {
                Some(ep) => {
                    println!("Found endpoint '{}' at {}", ep.name, ep.addr);
                    ep.addr
                }
                None => {
                    eprintln!("No OAAT endpoints found. Use --target to connect directly.");
                    return;
                }
            }
        }
    };

    let config = ControllerConfig {
        controller_id,
        controller_name: name,
        features: vec![],
        clock_port: oaat_core::DEFAULT_CLOCK_PORT,
        tls,
    };

    let mut endpoint = match ConnectedEndpoint::connect(&config, endpoint_addr).await {
        Ok(ep) => ep,
        Err(e) => {
            eprintln!("Connection failed: {e}");
            return;
        }
    };

    println!(
        "Connected to '{}' ({})\n",
        endpoint.info.endpoint_name, endpoint.info.endpoint_id
    );

    // Clock sync
    if let Err(e) = endpoint.clock_sync_bootstrap().await {
        warn!(error = %e, "clock sync failed");
    }

    let stream_id = "file-stream";
    endpoint
        .propose_format(
            stream_id,
            format,
            sample_rate,
            channels,
            ChannelLayout::Stereo,
            bits_per_sample,
        )
        .await
        .unwrap();

    endpoint
        .send_metadata(TrackMetadata {
            title: filename.to_string(),
            artist: "OAAT File Player".into(),
            album: String::new(),
            duration_ms: (duration_s * 1000.0) as u64,
            artwork_url: None,
            format: Some(format!("PCM {bits_per_sample}/{}", sample_rate / 1000)),
        })
        .await
        .unwrap();

    endpoint.send_play(stream_id).await.unwrap();
    println!("Streaming {filename}...\n");

    let samples_per_packet = 360;
    let packet_bytes = samples_per_packet * bytes_per_sample;
    let mut offset = 0usize;
    let mut sample_offset: u64 = 0;
    let start = std::time::Instant::now();
    let mut last_print = 0u64;

    while offset < data_len {
        let chunk_bytes = packet_bytes.min(data_len - offset);
        let chunk_samples = chunk_bytes / bytes_per_sample;
        let payload = &pcm_data[offset..offset + chunk_bytes];

        let pts_ns = (sample_offset as f64 / sample_rate as f64 * 1e9) as u64;
        let flags = if offset == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };

        endpoint
            .send_audio(1, format, pts_ns, sample_offset, payload, flags)
            .await
            .unwrap();

        offset += chunk_bytes;
        sample_offset += chunk_samples as u64;

        // Real-time pacing: sleep until the audio clock catches up
        let expected =
            Duration::from_nanos((sample_offset as f64 / sample_rate as f64 * 1e9) as u64);
        let elapsed = start.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }

        let secs = sample_offset / sample_rate as u64;
        let total_secs = duration_s as u64;
        if secs >= last_print + 5 || offset >= data_len {
            last_print = secs;
            let pct = (offset as f64 / data_len as f64 * 100.0) as u32;
            println!("  {secs}s / {total_secs}s ({pct}%)");
        }
    }

    endpoint
        .send_audio(
            1,
            format,
            (sample_offset as f64 / sample_rate as f64 * 1e9) as u64,
            sample_offset,
            &[],
            PacketFlags::LAST_PACKET,
        )
        .await
        .unwrap();

    endpoint.send_stop(stream_id).await.unwrap();
    println!("\nDone. {sample_offset} samples sent ({duration_s:.1}s).");
}

async fn run_multiroom(targets: Vec<SocketAddr>, freq: f64, duration: u64) {
    use oaat_controller::{ControllerConfig, Zone};

    println!("Multi-room: streaming to {} endpoints\n", targets.len());

    let config = ControllerConfig {
        controller_id: uuid::Uuid::new_v4().to_string(),
        controller_name: "OAAT Multi-Room".into(),
        features: vec![],
        clock_port: oaat_core::DEFAULT_CLOCK_PORT,
        tls: false,
    };

    let mut zone = Zone::new("zone-1".into(), "Demo Zone".into(), config);

    for addr in &targets {
        print!("  Connecting to {addr}... ");
        match zone.add_endpoint(*addr).await {
            Ok(id) => println!("OK ({id})"),
            Err(e) => {
                eprintln!("FAILED: {e}");
                return;
            }
        }
    }

    let n = zone.endpoint_count();
    println!(
        "\nZone '{}': {} endpoint(s), delay={}ms\n",
        zone.name,
        n,
        zone.play_delay_ms()
    );

    let sample_rate = 44100u32;
    let format = AudioFormat::PcmS16le;
    let channels = 2u8;
    let bits = 16u8;
    let stream_id = "multiroom-demo";

    zone.propose_format_all(
        stream_id,
        format,
        sample_rate,
        channels,
        ChannelLayout::Stereo,
        bits,
    )
    .await
    .unwrap();
    println!("Format proposed: {format} {sample_rate}Hz {channels}ch {bits}bit");

    zone.send_metadata_all(TrackMetadata {
        title: format!("{freq}Hz Sine — {n} endpoints"),
        artist: "OAAT Multi-Room".into(),
        album: "Sync Demo".into(),
        duration_ms: duration * 1000,
        artwork_url: None,
        format: Some(format!("PCM {bits}/{}", sample_rate / 1000)),
    })
    .await
    .unwrap();

    zone.play_all(stream_id).await.unwrap();
    println!("Streaming {freq}Hz for {duration}s across {n} endpoints...\n");

    let samples_per_packet = 480;
    let bytes_per_sample = 2 * channels as usize;
    let total_samples = sample_rate as u64 * duration;
    let mut sample_offset: u64 = 0;
    let play_delay_ns = zone.play_delay_ms() * 1_000_000;
    let start = std::time::Instant::now();
    let start_ns = now_ns() + play_delay_ns;

    while sample_offset < total_samples {
        let chunk = samples_per_packet.min((total_samples - sample_offset) as usize);
        let mut payload = Vec::with_capacity(chunk * bytes_per_sample);

        for i in 0..chunk {
            let t = (sample_offset + i as u64) as f64 / sample_rate as f64;
            let sample = (0.8 * (2.0 * PI * freq * t).sin() * i16::MAX as f64) as i16;
            payload.extend_from_slice(&sample.to_le_bytes());
            payload.extend_from_slice(&sample.to_le_bytes());
        }

        // PTS in controller clock domain — all endpoints adjust via their own clock offset
        let pts_ns = start_ns + (sample_offset as f64 / sample_rate as f64 * 1e9) as u64;
        let flags = if sample_offset == 0 {
            PacketFlags::FIRST_PACKET
        } else {
            PacketFlags::empty()
        };

        zone.send_audio_all(1, format, pts_ns, sample_offset, &payload, flags)
            .await
            .unwrap();

        sample_offset += chunk as u64;

        let expected =
            Duration::from_nanos((sample_offset as f64 / sample_rate as f64 * 1e9) as u64);
        let elapsed = start.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }

        if sample_offset.is_multiple_of(sample_rate as u64) || sample_offset >= total_samples {
            let secs = sample_offset / sample_rate as u64;
            let pct = (sample_offset as f64 / total_samples as f64 * 100.0) as u32;
            println!("  {secs}s / {duration}s ({pct}%)");
        }
    }

    zone.send_audio_all(
        1,
        format,
        start_ns + (duration as f64 * 1e9) as u64,
        sample_offset,
        &[],
        PacketFlags::LAST_PACKET,
    )
    .await
    .unwrap();

    zone.stop_all(stream_id).await.unwrap();
    println!("\nDone. {sample_offset} samples sent to {n} endpoints in sync.");
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn run_discover(timeout: u64) {
    println!("Discovering OAAT endpoints for {timeout}s...\n");
    let discovery = ControllerDiscovery::new().expect("failed to create mDNS");
    let endpoints = discovery.find_all(Duration::from_secs(timeout));

    if endpoints.is_empty() {
        println!("No OAAT endpoints found.");
    } else {
        println!("Found {} endpoint(s):\n", endpoints.len());
        for (i, ep) in endpoints.iter().enumerate() {
            println!("  {}. {} ({})", i + 1, ep.name, ep.endpoint_id);
            println!("     Address: {}", ep.addr);
            if let Some(ref caps) = ep.capabilities {
                println!("     Capabilities: {caps}");
            }
            if let Some(ref model) = ep.model {
                println!("     Model: {model}");
            }
            if let Some(ref vendor) = ep.vendor {
                println!("     Vendor: {vendor}");
            }
            println!();
        }
    }

    let _ = discovery.shutdown();
}
