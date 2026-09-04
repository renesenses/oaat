# OAAT — Open Advanced Audio Transport

[![CI](https://github.com/renesenses/oaat/actions/workflows/ci.yml/badge.svg)](https://github.com/renesenses/oaat/actions)
[![Spec: CC BY 4.0](https://img.shields.io/badge/Spec-CC_BY_4.0-brightgreen.svg)](docs/LICENSE-SPEC.md)
[![Implementation: BSL 1.1](https://img.shields.io/badge/Implementation-BSL_1.1-orange.svg)](LICENSE)

A bit-perfect, multi-room audio streaming protocol: an **open specification**,
free to implement, with a **source-available** reference implementation.

OAAT is a network audio transport protocol designed as an alternative to
Roon's proprietary RAAT. It provides:

- **Bit-perfect audio** — PCM up to 768 kHz / 32-bit, native DSD up to DSD512,
  native-format output path (exact integer passthrough at unity volume)
- **Multi-room sync** — PTS-scheduled playback start (measured 38–880 µs skew
  between endpoints) held by a continuous drift servo; PTP-inspired,
  endpoint-initiated clock sync
- **Loss resilience** — XOR FEC with in-order delivery and exact-length
  single-loss recovery (Wi-Fi)
- **Format negotiation** — accept/counter/reject; the accept is sent when the
  DAC is actually open, so the play-delay lead time is real
- **Gapless playback** — seamless track transitions with format change detection
- **Health reporting** — periodic `stream_stats` (buffer, drift, losses,
  bit-perfect flag) from endpoint to controller
- **Zero-config discovery** — mDNS/DNS-SD `_oaat._tcp`, no manual setup
- **Rust-first** — zero-copy wire format, async I/O, ~1500 LOC for a
  conforming endpoint

## Status

**v0.3.0 (draft)** — Phases 1–5.5 implemented: multi-room zones, DSD & FLAC
transport, TLS (TOFU), dynamic groups, PTS-scheduled starts, drift servo,
FEC end-to-end, bit-perfect output path, stream health reporting.

Read the full [RFC specification](docs/rfc.md).

## Crates

| Crate | Description |
|-------|-------------|
| `oaat-core` | Core types, wire format (incl. FEC), protocol messages, clock sync, codec |
| `oaat-endpoint` | Endpoint SDK: transport, mDNS, HAL trait, SharedClock/PtsTracker, cpal output |
| `oaat-controller` | Controller: transport, mDNS browsing, zone manager, clock responder |
| `oaat-cli` | CLI: `endpoint`, `controller`, `multiroom`, `discover` |
| `oaat-test` | Protocol conformance test tool |

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
cargo test --workspace    # 73 tests
```

```bash
# Terminal 1: start an endpoint with audio output
tune-bridge endpoint --name "Living Room DAC"

# Terminal 2: stream a 440Hz sine wave
tune-bridge controller --target 127.0.0.1:9740 --freq 440 --duration 5

# Multi-room, PTS-scheduled (sub-millisecond start skew), with FEC
tune-bridge multiroom 192.168.1.10:9740 192.168.1.11:9740 --duration 10 --fec 8

# Discover endpoints / test conformance
tune-bridge discover --timeout 5
oaat-test 192.168.1.50:9740
```

## Features

| Feature | Status |
|---------|--------|
| TCP control + UDP audio transport | Done |
| mDNS zero-config discovery | Done |
| Format negotiation (accept = device ready) | Done |
| Gapless playback (same format + reformat) | Done |
| Multi-room zones, dynamic join/leave, per-device volume | Done |
| Endpoint-initiated clock sync + controller responder | Done |
| PTS-scheduled playback start (38–880 µs measured skew) | Done |
| Drift servo (content position, jump resync, rebase) | Done |
| FEC end-to-end (XOR parity, exact-length recovery) | Done |
| Bit-perfect native-format output path | Done |
| DSD native + FLAC compressed transport | Done |
| TLS 1.3 (TOFU) / PSK auth | Done |
| stream_stats health reporting | Done |
| Conformance test tool | Done |
| CI (GitHub Actions, Linux + macOS) | Done |
| Tune server integration | Done |

## Comparison

| Feature | OAAT | RAAT | DLNA | AirPlay 2 | OpenHome |
|---------|------|------|------|-----------|----------|
| Spec license | CC BY 4.0* | Proprietary | UPnP Forum | Apple | BSD |
| Implementation license | BSL 1.1** | Proprietary | Varies | Apple | BSD |
| Bit-perfect | Yes | Yes | Depends | No | Yes |
| DSD native | Yes | Yes | DoP only | No | DoP |
| Multi-room sync | < 1 ms (measured) | < 1 ms | None | Apple | Limited |
| Gapless | Yes | Yes | Unreliable | Yes | Yes |
| Format negotiation | Auto | Auto | Manual | Fixed | Limited |
| Source available | Yes | No | Yes | Reverse-eng | Yes |
| Endpoint LOC | ~1500 | N/A | ~5000+ | N/A | ~3000+ |
| Conformance tool | Yes | No | No | No | No |

## License

**Two different things, two different licences.**

\* **The protocol specification** (`docs/rfc.md`) is an open standard:
[CC BY 4.0](docs/LICENSE-SPEC.md) with a **royalty-free patent and
implementation grant**. Anyone — including a manufacturer shipping a commercial
device — may implement OAAT. No fee, no certification cost, no separate
agreement, nobody to ask.

\*\* **The reference implementation** (`crates/`) is source-available, not open
source: Business Source License 1.1 — see [LICENSE](LICENSE). Free for
non-commercial use and for your own internal production use; each version
converts to Apache 2.0 four years after its publication. Embedding *this code*
in a commercial product requires a licence from MozAIk Labs:
contact@mozaiklabs.fr. Writing your own implementation from the specification
requires nothing.

"OAAT" and the OAAT logo are trademarks of MozAIk Labs. Implementing the
protocol does not grant the right to claim certification or endorsement; the
conformance tool is `oaat-test`.

## Author

Bertrand Clech / MozAIk Labs
