# Scope

This Working Group develops the OAAT (Open Advanced Audio Transport) specification, published in this repository as `docs/rfc.md`: a protocol for streaming audio bit-perfectly from a controller (server) to one or more endpoints (players) on a local IP network, with sample-accurate synchronization between endpoints.

## In scope

Everything an implementer needs to build a conforming OAAT controller or endpoint, including:

- discovery: the mDNS/DNS-SD service type `_oaat._tcp`, endpoint and controller announcements, TXT record keys and their values, discovery timing;
- the transport model: TCP control channel, UDP audio and clock channels, port allocation, network requirements and packet marking;
- the control protocol: message format, handshake, capability announcement and format negotiation, playback commands, metadata, zone management (assignment, membership updates, release, late join), per-device volume, endpoint health and playback statistics, gapless playback;
- audio streaming: the supported sample formats and how they are carried, channel layouts and channel order, sample rate families, format boundaries, native DSD transport, compressed (FLAC) transport;
- synchronization: the clock synchronization exchange and its packets, sync cadence, audio timestamps and scheduled start, buffer management, drift compensation, and the accuracy targets;
- the wire format: audio packet and clock sync packet layouts, and forward error correction (FEC);
- the security options the specification defines: TLS 1.3 on the control channel with trust on first use, and pre-shared key authentication during the handshake;
- the conformance requirements of the specification;
- any future version of the specification, and any message, field or feature, adopted into this repository.

## Out of scope

- The codecs, sample formats and bitstream formats the specification references (PCM, DSD, FLAC, Opus). The specification names them and defines how they are carried; it does not define them.
- The underlying standards the specification relies on (IP, TCP, UDP, mDNS/DNS-SD, TLS, IEEE 1588/PTP, DSCP). The specification uses them; it does not define them.
- Audio processing before or after transport: decoding, resampling, DSP, volume algorithms, digital-to-analogue conversion, and audio driver or hardware interfaces.
- Content sourcing, libraries, playback queues and user interfaces of controllers and endpoints.
- Software that implements the specification. The reference implementation (`crates/`) and the conformance tool `oaat-test` are licensed separately, under the Apache License 2.0.
- The names and logos, which are trademarks; see `TRADEMARKS.md`.

Any changes of Scope are not retroactive.
