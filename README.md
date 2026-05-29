# OAAT — Open Advanced Audio Transport

[![CI](https://github.com/renesenses/oaat/actions/workflows/ci.yml/badge.svg)](https://github.com/renesenses/oaat/actions/workflows/ci.yml)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

An open-source, bit-perfect, multi-room audio streaming protocol.

OAAT is a network audio transport protocol designed as an open alternative to Roon's proprietary RAAT. It provides:

- **Bit-perfect audio** — PCM up to 768 kHz / 32-bit, native DSD up to DSD512
- **Multi-room sync** — sub-millisecond synchronization via PTP-inspired clock sync
- **Format negotiation** — automatic accept/counter/reject based on endpoint capabilities
- **Gapless playback** — seamless track transitions with format change detection
- **Zero-config discovery** — mDNS/DNS-SD `_oaat._tcp`, no manual setup
- **Rust-first** — zero-copy wire format, async I/O, ~1500 LOC for a conforming endpoint

## Status

**v0.1.0** — Protocol specification complete. Phases 1-2 fully implemented, Phase 3-4 in progress.

Read the full [RFC specification](docs/rfc.md).

## Crates

| Crate | Description |
|-------|-------------|
| `oaat-core` | Core types, wire format, protocol messages, clock sync, codec |
| `oaat-endpoint` | Endpoint SDK: transport, mDNS, HAL trait, cpal audio output |
| `oaat-controller` | Controller: transport, mDNS browsing, zone manager |
| `oaat-cli` | CLI: `oaat endpoint`, `oaat controller`, `oaat multiroom`, `oaat discover` |
| `oaat-test` | Protocol conformance test tool (20 automated tests) |

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
cargo test --workspace    # 28 tests
```

### Stream audio to an endpoint

```bash
# Terminal 1: start an endpoint with audio output
oaat endpoint --name "Living Room DAC"

# Terminal 2: stream a 440Hz sine wave
oaat controller --target 127.0.0.1:9740 --freq 440 --duration 5

# Or use mDNS auto-discovery
oaat controller --duration 10
```

### Multi-room synchronized streaming

```bash
# Stream to multiple endpoints in sync (sub-millisecond)
oaat multiroom 192.168.1.10:9740 192.168.1.11:9740 --freq 440 --duration 10
```

### Discover endpoints on the network

```bash
oaat discover --timeout 5
```

### Test endpoint conformance

```bash
oaat-test 192.168.1.50:9740

# OAAT Conformance Test — 192.168.1.50:9740
# [Handshake]       4 PASS
# [Capabilities]    4 PASS
# [Format Nego]     3 PASS  (accept, counter, reject)
# [Clock Sync]      1 PASS  (offset < 10ms)
# [Audio]           1 PASS
# [Gapless]         2 PASS  (same format, diff format)
# [Volume]          3 PASS
# [Reconnect]       2 PASS
# 20 tests: 20 passed — Endpoint is CONFORMANT
```

## Features

| Feature | Status |
|---------|--------|
| TCP control + UDP audio transport | Done |
| mDNS zero-config discovery | Done |
| Format negotiation (accept/counter/reject) | Done |
| Gapless playback (same format + reformat) | Done |
| Multi-room zones with sync | Done |
| PTP-inspired clock sync (bootstrap + steady-state) | Done |
| Volume control + mute | Done |
| cpal audio output | Done |
| Reconnection (endpoint loops on disconnect) | Done |
| Conformance test tool (20 tests) | Done |
| CI (GitHub Actions, Linux + macOS) | Done |
| Tune server integration | Done |
| DSD native transport | Planned |
| FLAC compressed transport | Planned |
| TLS / PSK auth | Planned |

## Comparison

| Feature | OAAT | RAAT | DLNA | AirPlay 2 | OpenHome |
|---------|------|------|------|-----------|----------|
| License | Apache 2.0 | Proprietary | UPnP Forum | Apple | BSD |
| Bit-perfect | Yes | Yes | Depends | No | Yes |
| DSD native | Yes | Yes | DoP only | No | DoP |
| Multi-room sync | < 1 ms | < 1 ms | None | Apple | Limited |
| Gapless | Yes | Yes | Unreliable | Yes | Yes |
| Format negotiation | Auto | Auto | Manual | Fixed | Limited |
| Open source | Yes | No | Yes | Reverse-eng | Yes |
| Endpoint LOC | ~1500 | N/A | ~5000+ | N/A | ~3000+ |
| Conformance tool | Yes | No | No | No | No |

## License

Apache 2.0 — see [LICENSE](LICENSE).

## Author

Bertrand Clech / [MozAIk Labs](https://mozaiklabs.fr)
