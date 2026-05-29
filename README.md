# OAAT — Open Advanced Audio Transport

An open-source, bit-perfect, multi-room audio streaming protocol.

OAAT is a network audio transport protocol designed as an open alternative to Roon's proprietary RAAT. It provides:

- **Bit-perfect audio** — PCM up to 768 kHz / 32-bit, native DSD up to DSD512
- **Multi-room sync** — sub-millisecond synchronization via PTP-inspired clock sync
- **Format negotiation** — automatic agreement on the best common format
- **Zero-config discovery** — mDNS/DNS-SD, no manual setup
- **Rust-first** — zero-copy wire format, async I/O, ~1500 LOC for a conforming endpoint

## Status

**Draft v0.1.0** — Protocol specification and Phase 1 (core types, wire format, messages) implementation in progress.

Read the full [RFC specification](docs/rfc.md).

## Crates

| Crate | Description |
|-------|-------------|
| `oaat-core` | Core types, wire format, protocol messages, format negotiation |
| `oaat-endpoint` | Endpoint SDK with Hardware Abstraction Layer |
| `oaat-controller` | Controller implementation (for server integration) |
| `oaat-cli` | CLI demo tool (`oaat endpoint`, `oaat controller`, `oaat discover`) |

## Architecture

```
┌─────────────┐         TCP (control)          ┌──────────────┐
│             │◄──────────────────────────────►│              │
│  Controller │         UDP (audio)            │   Endpoint   │
│   (server)  │──────────────────────────────►│  (renderer)  │
│             │         UDP (clock sync)       │              │
│             │◄──────────────────────────────►│              │
└─────────────┘                                └──────────────┘
     :9740 TCP control        :9741 UDP audio        :9742 UDP clock
```

## Quick Start

```bash
cargo build --workspace
cargo test --workspace
```

### Demo: stream a sine wave over OAAT

Terminal 1 — start an endpoint:
```bash
cargo run --bin oaat -- endpoint --name "Living Room DAC"
```

Terminal 2 — stream a 440Hz sine wave:
```bash
cargo run --bin oaat -- controller --target 127.0.0.1:9740 --freq 440 --duration 5
```

Or use mDNS auto-discovery (no `--target` needed):
```bash
cargo run --bin oaat -- controller --duration 10
```

### Discover endpoints on the network

```bash
cargo run --bin oaat -- discover --timeout 5
```

## Implementation Phases

1. **Foundation (MVP)** — Core types, wire format, single endpoint PCM playback
2. **Multi-Room** — Zone support, synchronized playback, clock sync refinement
3. **Format Coverage** — DSD native, FLAC transport, gapless playback
4. **Production Hardening** — TLS, auth, reconnection, Wi-Fi adaptation
5. **Ecosystem** — Tune integration, C FFI, Python bindings, community outreach
6. **Advanced** — Multi-channel, room correction, mesh topology, WAN extension

## Comparison

| Feature | OAAT | RAAT | DLNA | AirPlay 2 | OpenHome |
|---------|------|------|------|-----------|----------|
| License | Apache 2.0 | Proprietary | UPnP Forum | Apple | BSD |
| Bit-perfect | Yes | Yes | Depends | No | Yes |
| DSD native | Yes | Yes | DoP only | No | DoP |
| Multi-room sync | < 1 ms | < 1 ms | None | Apple | Limited |
| Open source | Yes | No | Yes | Reverse-eng | Yes |
| Endpoint LOC | ~1500 | N/A | ~5000+ | N/A | ~3000+ |

## License

Apache 2.0 — see [LICENSE](LICENSE).

## Author

Bertrand Clech / [MozAIk Labs](https://mozaiklabs.fr)
