# OAAT — Open Advanced Audio Transport

[![CI](https://github.com/renesenses/oaat/actions/workflows/ci.yml/badge.svg)](https://github.com/renesenses/oaat/actions)
[![Spec: Community Specification License 1.0](https://img.shields.io/badge/Spec-Community_Spec_1.0-brightgreen.svg)](LICENSE.md)
[![Implementation: Apache 2.0](https://img.shields.io/badge/Implementation-Apache_2.0-brightgreen.svg)](LICENSE)

A bit-perfect, multi-room audio streaming protocol: an **open specification**,
free to implement, with an **open-source** reference implementation.

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
| Spec license | Community Spec 1.0* | Proprietary | UPnP Forum | Apple | BSD |
| Implementation license | Apache 2.0** | Proprietary | Varies | Apple | BSD |
| Bit-perfect | Yes | Yes | Depends | No | Yes |
| DSD native | Yes | Yes | DoP only | No | DoP |
| Multi-room sync | < 1 ms (measured) | < 1 ms | None | Apple | Limited |
| Gapless | Yes | Yes | Unreliable | Yes | Yes |
| Format negotiation | Auto | Auto | Manual | Fixed | Limited |
| Open source | Yes | No | Yes | Reverse-eng | Yes |
| Endpoint LOC | ~1500 | N/A | ~5000+ | N/A | ~3000+ |
| Conformance tool | Yes | No | No | No | No |

## License

**Two different things, two different licences.**

\* **The protocol specification** (`docs/rfc.md`) is an open standard under
the [Community Specification License 1.0](LICENSE.md)
([full text](Community_Specification_License-v1.md)), the licence also used by
Sendspin, developed through the Joint Development Foundation. It grants, free of
charge and royalty-free:

- the right to copy, adapt and **translate** the text, with attribution;
- a **patent licence from every contributor** for implementations of the
  specification, within the [Scope](Scope.md);
- a **reciprocal patent licence between implementers**, so that no implementer
  can use its own patents against another.

Anyone — including a manufacturer shipping a commercial device — may implement
OAAT. No fee, no certification cost, nobody to ask. To benefit from the patent
licences, include the [licence](Community_Specification_License-v1.md) with
your implementation (in the root of a source distribution, or in the
documentation or legal notices of a product), or add your name to
[Notices.md](Notices.md) by pull request (Section 2.1.3).

MozAIk Labs — Bertrand Clech contributes the specification as written to date
under this licence. Maintainer and Editor, in the sense of the
[Governance](Governance.md): Bertrand Clech (@renesenses). Contributions to the
specification are made under the
[Contributor License Agreement](CONTRIBUTOR-LICENSE-AGREEMENT.md); see
[Contributing.md](Contributing.md). Earlier versions (up to RFC 0.3.0) were
published under CC BY 4.0 with a royalty-free patent grant, which remains in
effect for them — see [docs/LICENSE-SPEC.md](docs/LICENSE-SPEC.md).

\*\* **The reference implementation** (`crates/`) is open source under the
[Apache License 2.0](LICENSE). Embed it, modify it, ship it inside a product you
sell — there is nothing to negotiate, no fee, and nobody to notify. Writing your
own implementation from the specification instead requires nothing either.

It was under the Business Source License until September 2026. That licence
restricted commercial use of *this code*, which no patent grant on the
specification could offset: a manufacturer weighing two protocols compares the
cost of integrating them, and rewriting the endpoint rather than importing it is
a cost. The restriction protected nothing that existed, so it went.

"OAAT", "Open Advanced Audio Transport" and the OAAT logo are trademarks of
MozAIk Labs. Neither licence grants trademark rights: see
[TRADEMARKS.md](TRADEMARKS.md) for what you may do without asking and what
needs permission. The conformance tool is `oaat-test`.

## Author

Bertrand Clech / MozAIk Labs
