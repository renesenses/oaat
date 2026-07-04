# RFC: OAAT -- Open Advanced Audio Transport

**Version**: 0.3.0 (Draft)
**Date**: 2026-07-04
**Author**: Bertrand Clech / MozAIk Labs
**Status**: Draft
**License**: Business Source License 1.1 (converts to Apache 2.0 four years after each version's publication; free for non-commercial and internal production use)

> Changes in 0.3.0: FEC is fully specified on the wire (group size, index,
> length-XOR recovery); FormatAccept now signals device readiness; clock sync
> is normatively endpoint-initiated with a mandatory controller responder;
> PTS-scheduled playback start and drift compensation are normative;
> bit-perfect output path requirements clarified.

---

## Abstract

This document specifies OAAT (Open Advanced Audio Transport), an open-source network audio streaming protocol designed for bit-perfect multi-room audio. OAAT provides the transport, synchronization, and control layers needed to stream high-resolution audio from a server to one or more endpoints on a local network, with sub-millisecond synchronization accuracy.

OAAT is designed to be an open alternative to Roon's proprietary RAAT protocol, while being simpler to implement, free of DRM, and friendly to both hardware manufacturers and open-source software projects.

---

## Table of Contents

1. Goals and Non-Goals
2. Architecture
3. Discovery
4. Transport Layer
5. Audio Streaming
6. Synchronization
7. Control Protocol
8. Security
9. Wire Format
10. Endpoint SDK
11. Comparison Table
12. Implementation Phases

---

## 1. Goals and Non-Goals

### 1.1 Goals

- **Bit-perfect audio transport**: PCM data arrives at the DAC identical to the source, with no resampling, dithering, or modification unless explicitly requested by the user.
- **Multi-room synchronization**: Multiple endpoints play the same audio stream in sync, with perceptible alignment (target: < 1 ms drift between any two endpoints on the same LAN).
- **Format negotiation**: Server and endpoint agree on the best common format (sample rate, bit depth, channel layout, encoding) without user intervention.
- **Open standard**: Fully documented, no patents, no licensing fees, no certification costs. Any project or manufacturer can implement OAAT.
- **Rust-first design**: Zero-copy semantics, async I/O, no garbage collector dependency. A conforming basic endpoint is implementable in under 2000 lines of Rust.
- **Embeddable**: Protocol is lightweight enough for resource-constrained devices (Raspberry Pi, ESP32 with external DAC, FPGA-based streamers).
- **Coexistence**: OAAT endpoints can coexist on the same network as DLNA, AirPlay, Chromecast, and OpenHome devices. An OAAT server MAY also serve those protocols simultaneously.
- **Low latency**: From play-press to sound output in < 200 ms for a single endpoint, < 500 ms for synchronized multi-room.

### 1.2 Non-Goals

- **DRM / content protection**: OAAT does not implement any form of digital rights management. If a streaming service requires encrypted transport to the endpoint, that is outside OAAT's scope. The server decrypts before handing off to OAAT.
- **Internet streaming**: OAAT is a LAN protocol. WAN streaming introduces jitter, packet loss, and latency that require fundamentally different approaches (adaptive bitrate, forward error correction at scale). A future extension MAY address this.
- **Audio processing / DSP**: OAAT transports audio. Equalization, room correction, crossfeed, and other DSP are server-side concerns. The protocol carries the processed result.
- **User interface**: OAAT defines no UI. Control is via the wire protocol; rendering a UI is the server application's responsibility.
- **Replacing USB Audio**: OAAT is a network protocol. Direct USB/I2S/SPDIF connections between a computer and a DAC are out of scope.

### 1.3 Positioning

OAAT occupies the space between "works everywhere but sounds mediocre" (DLNA) and "sounds perfect but costs money and is closed" (RAAT). It aims to be the TCP/IP of high-resolution network audio: boring, reliable, open, and ubiquitous.

---

## 2. Architecture

### 2.1 Roles

OAAT defines two roles:

- **Controller** (also called "server"): The entity that holds the audio library, manages streaming service connections, performs decoding and optional DSP, and orchestrates playback across endpoints. A network has one or more Controllers. A Controller MAY also act as an Endpoint (e.g., a Raspberry Pi that both serves and plays).
- **Endpoint** (also called "renderer"): The entity that receives audio data over the network and outputs it to a DAC. An Endpoint is typically a dedicated hardware device or a software player running on a general-purpose computer.

### 2.2 Zones

A **Zone** is a logical grouping of one or more Endpoints that play the same audio stream in sync. Zones are created, modified, and destroyed by the Controller. An Endpoint belongs to at most one Zone at any time.

Zone operations:
- `zone.create(endpoint_ids[])` -- create a zone with initial members
- `zone.add(endpoint_id)` -- add an endpoint to an existing zone (supports late-join during playback)
- `zone.remove(endpoint_id)` -- remove an endpoint from a zone (graceful, notifies remaining members)
- `zone.destroy()` -- dissolve a zone, all endpoints become idle

A **Zone Manager** is the centralized component that manages all zones. It provides:
- Zone lifecycle (create, dissolve, list, snapshot)
- Endpoint movement between zones
- Event stream for zone membership changes (endpoint joined, left, failed)
- Health monitoring of connected endpoints

### 2.3 Session Lifecycle

```
1. Discovery     -- Endpoint announces itself via mDNS
2. Handshake     -- Controller connects, capabilities exchanged
3. Idle          -- Endpoint awaits commands
4. Negotiation   -- Controller proposes format, endpoint accepts/counters
5. Streaming     -- Audio data flows, control messages interleaved
6. Paused        -- Audio flow suspended, session alive
7. Stopped       -- Audio flow terminated, session alive
8. Disconnected  -- TCP control connection closed, endpoint returns to Idle
```

State transitions are Controller-initiated except for error conditions and voluntary endpoint departure.

### 2.4 Connection Model

Each Controller-Endpoint pair maintains:

- **One TCP connection** for the control channel (persistent, bidirectional)
- **One UDP flow** for audio data (Controller to Endpoint, unidirectional)
- **One UDP flow** for clock synchronization (bidirectional)

The TCP connection is the session anchor. If it drops, the Endpoint MUST stop playback within 500 ms and return to Idle. The Controller MAY reconnect and resume.

---

## 3. Discovery

### 3.1 Service Type

OAAT uses DNS-SD (RFC 6763) over mDNS (RFC 6762) for zero-configuration discovery.

Service type: `_oaat._tcp`

### 3.2 Endpoint Announcement

An Endpoint registers the following DNS-SD service:

```
Instance: <human-readable name>
Service:  _oaat._tcp
Domain:   local.
Port:     <control port, default 9740>
```

The port number 9740 is the default. Endpoints MAY use any available port.

### 3.3 TXT Records

TXT records carry endpoint metadata for the Controller to use before establishing a TCP connection:

| Key | Required | Example | Description |
|-----|----------|---------|-------------|
| `v` | YES | `1` | OAAT protocol version |
| `id` | YES | `a3f8...c7` | Unique endpoint ID (128-bit, hex-encoded) |
| `name` | YES | `Living Room DAC` | Human-readable display name |
| `model` | NO | `StreamerX Pro` | Hardware model name |
| `vendor` | NO | `Acme Audio` | Manufacturer name |
| `caps` | YES | `pcm:768/32,dsd:256` | Compact capability string (see 5.2) |
| `ch` | YES | `2` | Maximum channel count |
| `vol` | NO | `hw` | Volume control type: `hw`, `sw`, `fixed`, `none` |
| `fw` | NO | `2.1.0` | Firmware/software version |

### 3.4 Controller Announcement (Optional)

A Controller MAY announce itself for auto-discovery by Endpoints or companion apps:

Service type: `_oaat-ctrl._tcp`

TXT records: `v`, `id`, `name`, `zones` (current zone count).

### 3.5 Discovery Timing

- Endpoints MUST announce within 2 seconds of boot.
- Controllers MUST browse for `_oaat._tcp` continuously.
- Endpoints MUST respond to unicast queries within 200 ms.
- Goodbye packets (TTL=0) MUST be sent on graceful shutdown.

---

## 4. Transport Layer

### 4.1 Why TCP for Control

The control channel carries:
- Session management (handshake, capabilities, negotiation)
- Playback commands (play, pause, seek, stop)
- Metadata (track info, artwork references)
- Zone management (group, ungroup)
- Clock sync bootstrapping
- Error reporting

These messages are small, infrequent, and require reliable delivery. TCP provides ordering, retransmission, and congestion control. The overhead of TCP is irrelevant for control traffic.

### 4.2 Why UDP for Audio

Audio data is:
- High-bandwidth (stereo 24/192 PCM = 9.2 Mbit/s, DSD256 = 22.6 Mbit/s)
- Latency-sensitive (buffering masks loss, but retransmission adds latency)
- Tolerant of rare packet loss when buffered adequately

UDP gives the protocol control over:
- Packet pacing (smooth jitter by controlling send rate)
- Buffer management (endpoint decides how much to buffer)
- No head-of-line blocking (one lost packet does not stall the stream)

OAAT does NOT use raw UDP blindly. It adds sequence numbers, timestamps, and optional FEC (Forward Error Correction) headers -- similar to RTP but without the full RTP/RTCP stack overhead.

### 4.3 Port Allocation

| Channel | Protocol | Default Port | Notes |
|---------|----------|-------------|-------|
| Control | TCP | 9740 | Announced via mDNS |
| Audio | UDP | 9741 | Communicated during negotiation |
| Clock Sync | UDP | 9742 | Communicated during handshake |

Endpoints MAY use different ports. The actual ports are exchanged during the handshake phase over the TCP control channel.

### 4.4 Network Requirements

- LAN: Gigabit Ethernet or 802.11ac/ax Wi-Fi recommended.
- Multicast: REQUIRED for mDNS discovery. NOT used for audio (unicast only).
- MTU: Standard 1500-byte Ethernet MTU assumed. Jumbo frames supported but not required.
- QoS: Endpoints and Controllers SHOULD mark audio UDP packets with DSCP EF (Expedited Forwarding, 0x2E) for switch/router prioritization.

---

## 5. Audio Streaming

### 5.1 Supported Formats

| Format | Description | Use Case |
|--------|-------------|----------|
| `PCM_S16LE` | Signed 16-bit little-endian PCM | CD quality |
| `PCM_S24LE` | Signed 24-bit little-endian PCM (packed, 3 bytes/sample) | Hi-res |
| `PCM_S24LE4` | Signed 24-bit in 32-bit container, LE | Hi-res (aligned) |
| `PCM_S32LE` | Signed 32-bit little-endian PCM | Studio |
| `PCM_F32LE` | 32-bit IEEE float, little-endian | DSP interchange |
| `DSD_U8` | DSD raw, 1-bit samples packed 8 per byte, MSB first | DSD64-DSD512 |
| `DSD_U16LE` | DSD raw, 16-bit words, LE | DSD (word-aligned) |
| `DSD_U32LE` | DSD raw, 32-bit words, LE | DSD (double-word-aligned) |
| `FLAC` | FLAC compressed frames | Bandwidth savings |
| `OPUS` | Opus compressed | Low-bandwidth/preview |

**Little-endian is the canonical byte order.** Big-endian formats are not supported on the wire. Endpoints with big-endian DAC interfaces perform the swap locally.

### 5.2 Capability String

The compact capability string in TXT records uses the format:

```
pcm:<max_rate_khz>/<max_bits>[,dsd:<max_multiplier>][,flac][,opus]
```

Examples:
- `pcm:192/24` -- supports PCM up to 192 kHz / 24-bit
- `pcm:768/32,dsd:256` -- supports PCM up to 768 kHz / 32-bit and DSD up to DSD256
- `pcm:96/24,flac` -- supports PCM up to 96 kHz / 24-bit, accepts FLAC frames

### 5.3 Format Negotiation

During session setup, the Controller sends a `FORMAT_PROPOSE` message:

```json
{
  "type": "format_propose",
  "stream_id": "abc123",
  "format": "PCM_S24LE",
  "sample_rate": 192000,
  "channels": 2,
  "channel_layout": "stereo",
  "bits_per_sample": 24,
  "dsd_rate": null
}
```

The Endpoint responds with one of:

- `FORMAT_ACCEPT` -- ready to receive in the proposed format.
- `FORMAT_COUNTER` -- proposes an alternative (e.g., lower sample rate if the DAC cannot handle 192 kHz).
- `FORMAT_REJECT` -- cannot play this stream at all (e.g., DSD to a PCM-only endpoint).

The Controller MUST NOT send audio data until it receives `FORMAT_ACCEPT`.

**Readiness semantics**: `FORMAT_ACCEPT` means the Endpoint is *ready to
render* — its audio device is open and configured for the proposed format,
not merely that the format is acceptable. Opening an audio device can take
hundreds of milliseconds to seconds on some hosts; an accept sent before the
device is ready silently consumes the Controller's play-delay lead time and
defeats PTS-scheduled starts (§6.4). Endpoints SHOULD defer the accept until
the device is open, with a bounded fail-open (RECOMMENDED: 5 s) so a stuck
device cannot deadlock the negotiation. `FORMAT_COUNTER` and `FORMAT_REJECT`
involve no device setup and are sent immediately.

**Bit-perfect guarantee**: When the Endpoint accepts a format, it commits to delivering those exact samples to the DAC with no modification. If the Endpoint needs to resample (e.g., DAC fixed at 48 kHz), it MUST counter-propose the native rate so the Controller can perform the conversion server-side with a high-quality resampler, rather than having the Endpoint do it with potentially inferior quality.

Bit-perfection constrains the entire output path, not just the wire:

- The Endpoint MUST buffer and deliver samples in the negotiated format's
  native representation. Intermediate float normalization is only
  acceptable when the scaling is a power of two (bijective for the source
  bit depth) — arbitrary scale factors like `i32::MAX` are not.
- Sample width adaptation MUST be limited to exact operations (e.g. 24-bit
  samples left-shifted into a 32-bit device slot).
- Software volume at any setting other than unity breaks bit-perfection;
  endpoints advertising `vol: hw` or `fixed` (§3.3) preserve it. Endpoints
  SHOULD expose whether the active output path is bit-exact.
- Lossless compressed transport (FLAC) preserves the guarantee provided
  decode and delivery follow the rules above.

### 5.4 Channel Layouts

| Name | Channels | Mapping |
|------|----------|---------|
| `mono` | 1 | C |
| `stereo` | 2 | L, R |
| `2.1` | 3 | L, R, LFE |
| `quad` | 4 | FL, FR, RL, RR |
| `5.1` | 6 | FL, FR, FC, LFE, RL, RR |
| `7.1` | 8 | FL, FR, FC, LFE, RL, RR, SL, SR |

Stereo is the baseline. All Endpoints MUST support stereo.

### 5.5 Sample Rate Families

- **44.1 kHz family**: 44100, 88200, 176400, 352800, 705600 Hz
- **48 kHz family**: 48000, 96000, 192000, 384000, 768000 Hz

When the Endpoint counter-proposes, it SHOULD stay within the same family to avoid introducing non-integer resampling artifacts.

### 5.6 DSD Transport

DSD is transported as raw bitstream data (not DoP). The `dsd_rate` field specifies the DSD multiplier:

| Multiplier | Bitstream Rate | Common Name |
|------------|---------------|-------------|
| 64 | 2.8224 MHz | DSD64 |
| 128 | 5.6448 MHz | DSD128 |
| 256 | 11.2896 MHz | DSD256 |
| 512 | 22.5792 MHz | DSD512 |

### 5.7 Compressed Transport (FLAC)

For bandwidth-constrained links (Wi-Fi, multiple simultaneous streams), the Controller MAY propose FLAC-encoded audio:

- Frames are standard FLAC frames, each self-contained and independently decodable.
- Frame size: 1024-4096 samples (negotiable).
- The Endpoint decodes to PCM locally. Bit-perfection is maintained because FLAC is lossless.

---

## 6. Synchronization

### 6.1 Overview

Multi-room synchronization requires all Endpoints in a Zone to play the same audio sample at the same wall-clock time. This demands:

1. A shared clock reference across all Endpoints.
2. Known, compensated network latency for each Endpoint.
3. Sufficient buffer depth to absorb jitter.

OAAT uses a PTP-inspired (IEEE 1588) clock synchronization mechanism, simplified for LAN use.

### 6.2 Clock Sync Protocol

The Controller is the **clock master**. Clock sync is **endpoint-initiated**
and runs over UDP:

```
Endpoint                          Controller
   |                                  |
   |--- SYNC_REQUEST { t1 } -------->|
   |                                  |
   |<-- SYNC_RESPONSE { t1,t2,t3 } --|
   |                                  |
   t4 = local receive time            |
```

The Endpoint computes:
- **Round-trip delay**: `d = (t4 - t1) - (t3 - t2)` (clamped to 0:
  measurement noise on a near-zero RTT can make it transiently negative)
- **Clock offset**: `offset = ((t2 - t1) + (t3 - t4)) / 2`

The Controller MUST run a clock sync responder on the clock port announced
in its `hello` message — without it, Endpoints can never learn their offset
and PTS scheduling (§6.4) is impossible. The Controller MAY additionally
poll Endpoints with its own exchanges for health monitoring (§7.5.6);
Endpoints MUST answer those with t2/t3 stamping. Endpoints SHOULD reject
sync responses older than 1 second (stale datagrams poison the filter).

Timestamps are nanoseconds. Implementations SHOULD use a clock source that
is not stepped by NTP adjustments; a wall-clock step degrades sync until the
filter re-converges.

### 6.3 Sync Cadence

- During initial handshake: 10 rapid exchanges at 100 ms (bootstrap,
  alpha = 0.5).
- Steady state: adaptive on measured jitter — 5 s when jitter < 100 µs,
  2 s below 1 ms, 500 ms above.
- Offset is filtered through an exponential moving average (alpha = 0.125).
- Jitter MUST be estimated with a forgetting statistic (e.g. exponentially
  weighted variance), not a cumulative one: slow clock drift would otherwise
  inflate the jitter estimate forever and pin the cadence at its fastest.

### 6.4 Audio Timestamps and Scheduled Start

Every audio packet carries a **presentation timestamp** (PTS) in the
Controller's clock domain: the instant the packet's FIRST frame should hit
the DAC. The Controller stamps absolute PTS values:

```
PTS(packet) = play_start_ns + sample_offset / sample_rate * 1e9
play_start_ns = controller_clock_ns_at_play + target_play_delay_ns
```

Default target play delay: 200 ms (single endpoint), 500 ms (multi-room).
Controllers SHOULD make the delay configurable for endpoints with slow
device setup.

**Scheduled start (normative)**: on `play`, the Endpoint MUST NOT start
output immediately. It buffers incoming audio and starts output at the local
instant corresponding to the stream's start:

```
head_pts = packet.pts_ns − buffered_frames_before_packet / sample_rate * 1e9
local_start = head_pts − clock_offset
```

The ring-head correction matters: UDP audio can outrun the TCP `play`
command, so the first packet observed *after* `play` is not necessarily the
first packet of the stream. Anchoring on the buffer head makes all endpoints
of a zone converge on the same absolute instant regardless of when each one
armed. Measured on a LAN reference implementation: 38–880 µs start skew
between two endpoints.

Fallbacks: if the clock is not bootstrapped or the PTS is implausible
(relative timestamps from a legacy controller), the Endpoint starts
immediately. If the deadline is already past (late join, slow device), the
Endpoint starts immediately and realigns to the controller timeline through
drift compensation (§6.6).

### 6.5 Buffer Management

Buffer size: `target_play_delay * 2` (e.g., 1 second at 500 ms multi-room delay).

Buffer states: Filling → Playing → Underrun/Overflow (with reporting).

### 6.6 Drift Compensation

The Endpoint MUST track its **content position** — frames actually consumed
by the output device *plus* the net frames skipped or duplicated so far —
and compare it against the position the clock dictates
(`(now − head_pts) × sample_rate`). Tracking raw consumed frames alone is
insufficient: applied corrections would never show up in the measured drift
and the servo would skip forever.

Correction mechanisms (in order of preference):
1. Micro-adjust output clock (hardware)
2. Skip/duplicate frames at inaudible rates (software; RECOMMENDED: a few
   frames per packet within a ±0.5 ms deadband, evaluated at ~1 Hz)
3. Report drift to Controller for PTS adjustment (universal fallback)

Two special regimes are RECOMMENDED for software correction:
- **Jump resync**: when the deficit exceeds ~25 ms (late start), drop it in
  one jump as content arrives — one audible discontinuity beats seconds of
  audible desynchronization.
- **Timeline rebase**: when catch-up stalls (the content needed to catch up
  was lost, e.g. socket overflow during a very late start), accept the
  residual latency by shifting the reference start instead of skipping
  arriving audio forever.

**Health reporting**: while streaming, Endpoints SHOULD send a periodic
`stream_stats` message (RECOMMENDED: every 5 s) so the Controller can see
buffer health, residual drift and link quality without polling:

```json
{
  "type": "stream_stats",
  "stream_id": "abc123",
  "buffer_frames": 22050,
  "drift_us": -120,
  "corrections_net_frames": 27405,
  "packets_lost": 0,
  "packets_recovered": 3,
  "bit_perfect": true
}
```

### 6.7 Sync Accuracy Target

| Scenario | Target | Acceptable |
|----------|--------|------------|
| Two endpoints, wired | < 100 us | < 1 ms |
| Multi-room (3+), wired | < 500 us | < 2 ms |
| Multi-room, Wi-Fi | < 2 ms | < 5 ms |

---

## 7. Control Protocol

### 7.1 Message Format

JSON objects over TCP, framed with 4-byte big-endian length prefix:

```
[4 bytes: message length (BE u32)] [JSON payload]
```

### 7.2 Handshake

```json
// Controller -> Endpoint: HELLO
{
  "type": "hello",
  "protocol_version": 1,
  "controller_id": "...",
  "controller_name": "Tune Server",
  "clock_port": 9742,
  "features": ["flac_transport", "dsd_native", "multi_channel"]
}

// Endpoint -> Controller: HELLO_ACK
{
  "type": "hello_ack",
  "protocol_version": 1,
  "endpoint_id": "...",
  "endpoint_name": "Living Room DAC",
  "capabilities": {
    "pcm_max_rate": 768000,
    "pcm_max_bits": 32,
    "dsd_max_rate": 256,
    "channels_max": 2,
    "formats": ["PCM_S16LE", "PCM_S24LE", "PCM_S24LE4", "PCM_S32LE", "DSD_U32LE", "FLAC"],
    "volume": { "type": "hw", "range": [0, 100], "step": 1 },
    "gapless": true,
    "seek": true
  },
  "audio_port": 9741,
  "clock_port": 9742,
  "buffer_size_ms": 1000
}
```

### 7.3 Playback Commands

| Message Type | Direction | Description |
|-------------|-----------|-------------|
| `format_propose` | C → E | Propose audio format |
| `format_accept` | E → C | Accept proposed format |
| `format_counter` | E → C | Counter-propose alternative |
| `format_reject` | E → C | Cannot play this format |
| `play` | C → E | Start/resume playback |
| `pause` | C → E | Pause |
| `stop` | C → E | Stop and flush buffer |
| `seek` | C → E | Seek to position |
| `volume_set` | C → E | Set volume (0-100 or dB) |
| `volume_get` | C → E | Query volume |
| `volume_report` | E → C | Report volume |
| `stream_stats` | E → C | Periodic health report: buffer, drift, losses (§6.6) |
| `mute` | C → E | Mute/unmute |

### 7.4 Metadata

```json
{
  "type": "metadata",
  "track": {
    "title": "Time",
    "artist": "Pink Floyd",
    "album": "The Dark Side of the Moon",
    "duration_ms": 413000,
    "artwork_url": "http://192.168.1.15:8888/api/v1/artwork/ab12cd34",
    "format": "FLAC 24/96"
  }
}
```

### 7.5 Zone Management

Zone management allows dynamic multi-device grouping during playback.

| Message Type | Direction | Description |
|-------------|-----------|-------------|
| `zone_assign` | C → E | Assign endpoint to a zone |
| `zone_update` | C → E | Zone membership changed (broadcast to all members) |
| `zone_release` | C → E | Remove endpoint from zone |
| `zone_ack` | E → C | Acknowledge zone assignment |

#### 7.5.1 Zone Assignment

When a Controller adds an Endpoint to a Zone, it sends `zone_assign`:

```json
{
  "type": "zone_assign",
  "zone_id": "zone-living-room",
  "endpoint_id": "a3f8...c7"
}
```

The Endpoint MUST respond with `zone_ack`:

```json
{
  "type": "zone_ack",
  "zone_id": "zone-living-room",
  "endpoint_id": "a3f8...c7",
  "accepted": true,
  "reason": null
}
```

An Endpoint MAY reject a zone assignment (e.g., if already in another zone from a different Controller) by setting `accepted: false` with a reason string.

#### 7.5.2 Zone Membership Updates

When zone membership changes (endpoint added or removed), the Controller broadcasts `zone_update` to ALL remaining zone members:

```json
{
  "type": "zone_update",
  "zone_id": "zone-living-room",
  "endpoint_ids": ["a3f8...c7", "b2e1...d9"]
}
```

This allows endpoints to be aware of their peers (useful for companion app UIs and diagnostics).

#### 7.5.3 Zone Release

When an Endpoint is removed from a Zone:

```json
{
  "type": "zone_release",
  "zone_id": "zone-living-room",
  "endpoint_id": "a3f8...c7"
}
```

The Endpoint MUST stop playback and return to Idle upon receiving `zone_release`. No acknowledgment is required.

#### 7.5.4 Late-Join Protocol

An Endpoint MAY join a Zone that is already streaming. The Controller performs the following sequence:

1. TCP connect + handshake (Hello/HelloAck)
2. Clock sync bootstrap (10 exchanges)
3. `zone_assign` → wait for `zone_ack`
4. `format_propose` with the current stream's format
5. `metadata` with the current track information
6. `volume_set` with the effective volume for this endpoint
7. `play` — the Endpoint begins playback
8. `zone_update` broadcast to all zone members

The late-joining Endpoint will start receiving audio packets from the current position in the stream. There is no catch-up mechanism for already-played audio.

#### 7.5.5 Per-Device Volume

Volume in a multi-device zone uses a **master + offset** model:

- **Master volume**: Zone-wide level (0-100), applied to all endpoints.
- **Per-endpoint offset**: Signed delta (-100 to +100) per endpoint.
- **Effective volume**: `clamp(master + offset, 0, 100)`

The Controller sends `volume_set` with the effective volume computed for each endpoint individually. Endpoints are unaware of the master/offset model — they only see their effective level.

This allows users to balance volume across rooms (e.g., kitchen louder than bedroom) while adjusting the overall zone volume with a single control.

#### 7.5.6 Endpoint Health

The Controller SHOULD monitor endpoint health:

- **TCP reader task**: If the TCP control connection drops, the endpoint is considered disconnected.
- **Clock sync**: If 3 consecutive clock sync exchanges fail (timeout > 1s each), the endpoint SHOULD be marked as degraded.
- **Graceful degradation**: Audio continues to healthy endpoints; failed endpoints are removed from the zone.
- **Reconnection**: The Controller MAY attempt to reconnect a failed endpoint and re-join it to the zone via the late-join protocol.

### 7.6 Gapless Playback

1. Controller sends `next_track_prepare` with next track's format.
2. Same format → `next_track_ready`, seamless audio continuation.
3. Different format → `next_track_reformat`, format boundary marker in stream.

---

## 8. Security

- **Control channel**: Optional TLS 1.3 (STARTTLS pattern). Self-signed + TOFU acceptable.
- **Audio channel**: NOT encrypted by default (LAN, not secret). Optional DTLS.
- **Authentication**: Optional PSK during handshake.
- **No DRM**: By design. Will not be modified to accommodate DRM.

---

## 9. Wire Format

### 9.1 Audio Packets (UDP)

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  Ver  | Flags |    Format     |        Sequence (u16 BE)      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     Stream ID (u32 BE)                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
|                Presentation Timestamp (u64 BE, ns)            |
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
|                    Sample Offset (u64 BE)                     |
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|        Payload Length (u16 BE)      | FEC grp size |FEC index |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|        FEC length XOR (u16 BE)      |       Reserved (u16)    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
|                     Audio Payload                             |
|                          ...                                  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

Header: 32 bytes. Senders MUST NOT exceed 1440 payload bytes (fits a single
Ethernet frame — larger payloads fragment at the IP layer and amplify
loss); receivers SHOULD accept payloads up to 8192 bytes.

**Format enum**:

| Value | Format |
|-------|--------|
| 0x01 | PCM_S16LE |
| 0x02 | PCM_S24LE |
| 0x03 | PCM_S24LE4 |
| 0x04 | PCM_S32LE |
| 0x05 | PCM_F32LE |
| 0x10 | DSD_U8 |
| 0x11 | DSD_U16LE |
| 0x12 | DSD_U32LE |
| 0x20 | FLAC |
| 0x21 | OPUS |

**Flags**: `0x01` = first packet, `0x02` = last packet, `0x04` = FEC, `0x08` = format change boundary.

#### 9.1.1 Forward Error Correction

FEC is XOR parity over groups of consecutive data packets. All FEC fields
are zero when FEC is disabled.

- **Data packets** carry `fec_group_size` (2–16, number of data packets per
  parity packet) and `fec_index` (this packet's position in its group,
  `0..group_size-1`). A group's base sequence is
  `sequence − fec_index` (wrapping).
- **Parity packets** carry the `0x04` flag, the same `fec_group_size`, and
  consume the sequence number following the group's last data packet. The
  payload is the XOR of all data payloads in the group, each zero-extended
  to the longest; `payload_len` is that longest length. `fec_len_xor` is
  the XOR of the `payload_len` of all data packets in the group.
- **Recovery**: with exactly one data packet missing and the parity
  received, the missing payload is the XOR of the parity payload with all
  received payloads (zero-extended), truncated to
  `fec_len_xor XOR (received lengths)` — the exact original length, so no
  padding is injected into the stream. The recovered packet's PTS and
  sample offset are interpolated from neighbors (exact for uniform packet
  durations). Two or more losses in a group are unrecoverable.
- **Receiver behavior**: packets within a group are reordered by index and
  emitted in order once the group completes (or recovers). Buffering one
  group adds `group_size × packet_duration` latency (≈90 ms for 8 × 480
  frames at 44.1 kHz) — receivers MUST account for this within the play
  delay. An incomplete group is flushed when the next group starts, on
  `0x02` (last packet), or on stream teardown; missing packets are counted
  as lost. Parity packets MUST NOT be delivered to the audio path.
- **Overhead**: `1/group_size` bandwidth (6–50%). Intended for Wi-Fi links
  with occasional single-packet loss; it does not replace adequate
  buffering.

### 9.2 Clock Sync Packets (UDP)

28 bytes: Ver (4 bits) + Type (4 bits) + Sequence (u16) + T1/T2/T3 (u64 each).

### 9.3 Zero-Copy Design (Rust)

```rust
#[repr(C, packed)]
struct OaatAudioHeader {
    ver_flags: u8,
    format: u8,
    sequence: u16,
    stream_id: u32,
    pts: u64,
    sample_offset: u64,
    payload_len: u16,
    fec_group_size: u8,
    fec_index: u8,
    fec_len_xor: u16,
    reserved: u16,
}
```

---

## 10. Endpoint SDK

### 10.1 Minimal Implementation (~1350 LOC Rust)

| Component | LOC | Crates |
|-----------|-----|--------|
| mDNS announcement | ~100 | `mdns-sd` |
| TCP control (JSON) | ~300 | `tokio`, `serde_json` |
| UDP audio receiver | ~200 | `tokio` |
| Clock sync | ~150 | `tokio` |
| Packet parser | ~50 | `zerocopy` |
| Ring buffer | ~100 | `ringbuf` |
| Audio output | ~200 | `cpal` |
| Format negotiation | ~100 | -- |
| State machine | ~150 | -- |

### 10.2 Hardware Abstraction Layer

```rust
trait OaatHal {
    fn configure_output(&mut self, format: AudioFormat) -> Result<()>;
    fn write_frames(&mut self, data: &[u8], frames: usize) -> Result<usize>;
    fn buffer_level(&self) -> usize;
    fn set_volume(&mut self, level: u8) -> Result<()>;
    fn actual_sample_rate(&self) -> Option<f64>;
}
```

---

## 11. Comparison Table

| Feature | OAAT | RAAT | DLNA | AirPlay 2 | Chromecast | OpenHome |
|---------|------|------|------|-----------|------------|----------|
| License | BSL 1.1* | Proprietary | UPnP Forum | Apple | Google | BSD |
| Cert cost | Free | Paid | Fee | MFi | Cast SDK | Free |
| Bit-perfect | Yes | Yes | Depends | No | No | Yes |
| Max PCM | 768/32 | 768/32 | Varies | 48/24 | 48/24 | Varies |
| DSD native | Yes | Yes | DoP only | No | No | DoP |
| Multi-room sync | < 1 ms | < 1 ms | None | Apple | Google | Limited |
| Gapless | Yes | Yes | Unreliable | Yes | Yes | Yes |
| Latency | < 200 ms | ~150 ms | 1-5 s | ~2 s | ~1 s | 1-5 s |
| DRM | None | MQA | None | FairPlay | Widevine | None |
| Endpoint LOC | ~1500 | N/A | ~5000+ | N/A | N/A | ~3000+ |
| Open source | Yes | No | Yes | Reverse-eng | No | Yes |

\* BSL 1.1: free for non-commercial and internal production use; converts to Apache 2.0 four years after each version. Commercial licensing: contact@mozaiklabs.fr.

---

## 12. Implementation Phases

### Phase 1: Foundation (MVP) ✅
Single Controller, single Endpoint, stereo PCM playback. `oaat-core`, `oaat-endpoint`, `oaat-controller` crates. CLI tools. 20 conformance tests.

### Phase 2: Multi-Room ✅
Zone support, synchronized playback across 2+ endpoints. PTP-inspired clock sync, EMA filtering.

### Phase 3: Format Coverage ✅
DSD native, FLAC transport, gapless playback, all 10 PCM/DSD/compressed formats. Format negotiation with counter-proposal.

### Phase 4: Production Hardening ✅
TLS 1.3 (TOFU), reconnection logic, `oaat-test` conformance tool, ALSA direct output, USB DAC auto-detection, daemon mode, web status UI.

### Phase 5: Dynamic Multi-Device ✅
Zone Manager with event system, dynamic join/leave during playback, per-device volume (master + offset model), zone protocol messages (ZoneAssign/ZoneUpdate/ZoneRelease/ZoneAck), endpoint health monitoring, graceful degradation, C FFI bindings.

### Phase 5.5: Sync & Resilience (v0.3) ✅
Endpoint-initiated clock sync with mandatory controller responder,
PTS-scheduled playback start (measured 38–880 µs start skew between two
LAN endpoints), content-position drift servo (fine skip/dup, jump resync,
timeline rebase), deferred FormatAccept (device readiness), FEC fully
specified on the wire and implemented end-to-end (reorder + single-loss
recovery with exact length restoration), bit-perfect native-format output
path.

### Phase 6: Ecosystem -- In progress
Tune server integration (OaatOutput/OaatMultiroomOutput), crates.io publication, Raspberry Pi deployment, outreach to Volumio/moOde/HiFiBerryOS.

### Phase 7: Advanced Features -- Post v1.0
Multi-channel, room correction exchange, mesh topology, WAN extension (Tune Cloud/Bridge), power management, IETF Internet-Draft.

---

## Appendix A: Why Not Extend an Existing Protocol?

**DLNA/UPnP AV**: HTTP-based, renderer-pulled. No server-push, no PTS, no sync primitive. XML/SOAP is verbose and fragile.

**OpenHome**: Fixes many DLNA issues but still HTTP for audio. Songcast does multicast sync but is separate from the control plane.

**AirPlay**: Apple-controlled, reverse-engineered, limited to 48 kHz.

**Chromecast**: Google-controlled, lossy codecs, resampling.

**Snapcast**: Good sync but compressed streams only, no format negotiation, no bit-perfect, no DSD.

Building OAAT from scratch allows clean design decisions (UDP audio, PTP sync, explicit negotiation) impossible to retrofit.

## Appendix B: Bandwidth Requirements

| Format | Rate | Bitrate | Packets/s |
|--------|------|---------|-----------|
| PCM 16/44.1 stereo | 44100 | 1.41 Mbit/s | 123 |
| PCM 24/96 stereo | 96000 | 4.61 Mbit/s | 400 |
| PCM 24/192 stereo | 192000 | 9.22 Mbit/s | 800 |
| PCM 32/384 stereo | 384000 | 24.58 Mbit/s | 2134 |
| DSD64 stereo | 2.8 MHz | 5.64 Mbit/s | 490 |
| DSD128 stereo | 5.6 MHz | 11.29 Mbit/s | 980 |
| DSD256 stereo | 11.3 MHz | 22.58 Mbit/s | 1960 |

## Appendix C: IANA Considerations

- Service types `_oaat._tcp` and `_oaat-ctrl._tcp` to be registered.
- Default ports 9740-9742 to be requested (currently unassigned).

## Appendix D: Glossary

| Term | Definition |
|------|-----------|
| Controller | Server managing audio sources, decoding/DSP, streaming to Endpoints |
| Endpoint | Device/software receiving audio over OAAT, outputting to DAC |
| Zone | Logical group of Endpoints playing same stream in sync |
| PTS | Presentation Timestamp -- controller clock time for sample playback |
| HAL | Hardware Abstraction Layer -- manufacturer-implemented interface |
| TOFU | Trust On First Use -- certificate acceptance pattern |
| FEC | Forward Error Correction -- redundant data for packet loss recovery |
| EMA | Exponential Moving Average -- clock offset smoothing filter |
| Zone Manager | Centralized component managing all zones, events, and endpoint health |
| Late-Join | Process of adding an endpoint to a zone that is already streaming |
| Volume Offset | Per-endpoint signed delta applied to zone master volume |
