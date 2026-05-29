# OAAT — Open Advanced Audio Transport

## What is this

Rust implementation of the OAAT protocol — an open-source, bit-perfect, multi-room audio streaming protocol. Open alternative to Roon's RAAT.

RFC spec: `docs/rfc.md`

## Workspace layout

```
crates/
  oaat-core/         Core types, wire format, protocol messages, codec, clock sync
  oaat-endpoint/     Endpoint SDK: transport, mDNS discovery, cpal audio output, HAL trait
  oaat-controller/   Controller: transport, mDNS browsing, zone manager
  oaat-cli/          CLI binary: endpoint, controller, multiroom, discover
  oaat-test/         Protocol conformance test tool (20 automated tests)
tests/               Integration tests (handshake, format negotiation, gapless, multi-room)
docs/rfc.md          Full protocol RFC
```

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## CLI usage

```bash
# Start endpoint (listens on port 9740, announces via mDNS)
cargo run --bin oaat -- endpoint --name "My DAC" --port 9740

# Stream to a single endpoint
cargo run --bin oaat -- controller --target 127.0.0.1:9740 --freq 440 --duration 5

# Multi-room: stream to N endpoints in sync
cargo run --bin oaat -- multiroom 192.168.1.10:9740 192.168.1.11:9740 --duration 10

# Discover endpoints on the network
cargo run --bin oaat -- discover --timeout 5
```

## Protocol architecture

- **Control**: TCP, length-prefixed JSON messages (port 9740)
- **Audio**: UDP, 32-byte binary header + PCM payload (port 9741)
- **Clock sync**: UDP, PTP-inspired 4-timestamp exchange (port 9742)
- **Discovery**: mDNS/DNS-SD, `_oaat._tcp` service type

## Key types

- `AudioFormat` — 10 formats: PCM (S16/S24/S24LE4/S32/F32), DSD (U8/U16/U32), FLAC, Opus
- `AudioPacketHeader` — 32-byte wire header (version, flags, format, sequence, stream_id, PTS, sample_offset, payload_len)
- `ClockSyncPacket` — 28-byte PTP exchange (t1/t2/t3 timestamps)
- `Message` — All JSON control messages (hello, format_propose/accept/counter/reject, play/pause/stop, metadata, zone_*, next_track_*)
- `SessionState` — 8-state machine (Discovery → Handshake → Idle → Negotiation → Streaming → Paused → Stopped → Disconnected)
- `Capabilities` — Parsed from mDNS TXT: `pcm:768/32,dsd:256,flac`
- `ClockState` — EMA-filtered clock offset (alpha=0.125, bootstrap alpha=0.5)
- `FrameCodec` — Buffered TCP frame decoder (handles partial reads)
- `Zone` — Multi-endpoint group with synchronized audio fan-out

## Format negotiation

Endpoint auto-responds based on capabilities:
- Format in list + rate/bits within limits → `FormatAccept`
- Format in list but rate/bits too high → `FormatCounter` (stays in same 44.1k/48k family)
- Format not in list → `FormatReject`

## Conformance testing

```bash
oaat-test <endpoint:port>    # 20 tests: handshake, caps, format nego, clock, audio, gapless, volume, reconnect
```

Exit code 0 = conformant, 1 = issues. Use to validate any OAAT endpoint implementation.

## Tune integration

Branch `fix/oaat-output` on `tune-server-rust`:
- `tune-core/src/outputs/oaat.rs` — `OaatOutput` implements `OutputTarget`
- mDNS auto-discovery of OAAT endpoints (highest priority output type)
- Feature-gated: `--features oaat`
- Streams by fetching WAV from Tune's HTTP streamer, skipping header, sending PCM via OAAT UDP

## Conventions

- Edition 2024, Rust stable
- `thiserror` for error types, `tracing` for logging, `tokio` for async
- Wire format is big-endian, audio samples are little-endian
- All ports reported by actual bind (not config) to handle port 0
- Integration tests use port 0 for all sockets, bind-then-drop to get real ports
- ConnectedEndpoint aborts its reader task on drop (clean TCP close for reconnection)
- Endpoint transport loops on accept() for reconnection (no restart needed)
