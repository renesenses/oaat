# Turning a Raspberry Pi into an OAAT audio endpoint

*Ce guide existe aussi en [français](howto-rpi-endpoint.fr.md).*

A complete guide to setting up a Raspberry Pi 3B+ or 4 as a bit-perfect audio endpoint using the OAAT protocol.

## Contents

1. [Why OAAT on a Raspberry Pi?](#why-oaat-on-a-raspberry-pi)
2. [What you need](#what-you-need)
3. [Flashing the SD card](#flashing-the-sd-card)
4. [Installation](#installation)
5. [Configuration](#configuration)
6. [Checks and first sound](#checks-and-first-sound)
7. [Using it with Tune](#using-it-with-tune)
8. [Troubleshooting](#troubleshooting)
9. [Going further](#going-further)
10. [Licence](#licence)

---

## Why OAAT on a Raspberry Pi?

OAAT (Open Advanced Audio Transport) is a network audio transport protocol whose specification is an open, royalty-free standard, designed as an alternative to Roon's proprietary RAAT. The reference implementation is open source under the Apache License 2.0 — see the [Licence](#licence) section. It offers:

- **Bit-perfect audio**: PCM up to 768 kHz / 32 bits, native DSD up to DSD512
- **Multi-room synchronisation**: < 1 ms between endpoints, over a PTP clock
- **Format negotiation**: the controller adapts automatically to the DAC's capabilities
- **Gapless**: seamless transitions, even across a format change
- **Zero configuration**: automatic discovery over mDNS/DNS-SD

| | OAAT | RAAT (Roon) | DLNA/UPnP | AirPlay 2 |
|---|---|---|---|---|
| Specification licence | Community Spec 1.0\* | Proprietary | UPnP Forum | Apple |
| Implementation licence | Apache 2.0\*\* | Proprietary | Varies | Apple |
| Bit-perfect | Yes | Yes | Depends | No |
| Native DSD | Yes | Yes | DoP only | No |
| Multi-room sync | < 1 ms | < 1 ms | None | Apple only |
| Open source | Yes | No | Yes | No |

The Raspberry Pi is the ideal companion:

- **Price**: EUR 35-75 depending on the model — a high-end audio endpoint for the price of a meal
- **Silence**: no fan, zero mechanical noise
- **Native I2S**: a direct connection to the DAC over the GPIO's I2S bus, bypassing USB — the shortest and cleanest path to the DAC
- **Compact**: it slips behind an amplifier, or into an Audiophonics case

---

## What you need

### The Raspberry Pi

| Model | Max PCM | RAM | Typical price | Recommendation |
|-------|---------|-----|---------------|----------------|
| **RPi 4B** (2 or 4 GB) | 384 kHz / 32 bits | 2-8 GB | ~EUR 55-75 | Recommended |
| **RPi 3B+** | 192 kHz / 32 bits | 1 GB | ~EUR 35-45 | Budget / second-hand |
| RPi 5 | 384 kHz / 32 bits | 4-8 GB | ~EUR 70-90 | Works too |

> **Note**: the RPi 3B+ is limited to 192 kHz by its I2S bus. For hi-res beyond that (352.8 / 384 kHz), prefer the RPi 4.

### The I2S DAC (HAT)

OAAT supports every Raspberry Pi-compatible I2S DAC. Here are the most common ones:

| DAC | Chipset | Max PCM | Price | Linux overlay |
|-----|---------|---------|-------|---------------|
| **Audiophonics ESS 9038Q2M** | ESS ES9038Q2M | 384 kHz / 32 bits | ~EUR 120 | `i-sabre-q2m` ([driver](https://github.com/audiophonics/I-Sabre_9038Q2M)) |
| **HifiBerry DAC2 HD** | PCM1796 | 192 kHz / 24 bits | ~EUR 65 | `hifiberry-dacplushd` |
| **HifiBerry DAC+ Pro** | PCM5122 | 192 kHz / 32 bits | ~EUR 45 | `hifiberry-dacplus` |
| **Allo Boss** | PCM5122 | 384 kHz / 32 bits | ~EUR 50 | `allo-boss-dac-pcm512x-audio` |
| **IQaudio DAC+** | PCM5122 | 192 kHz / 32 bits | ~EUR 30 | `iqaudio-dacplus` |
| **JustBoom DAC HAT** | PCM5122 | 384 kHz / 32 bits | ~EUR 35 | `justboom-dac` |

> **Important**: the Linux overlay must match your DAC exactly. The wrong overlay means no sound (ALSA error `-121` or `-22`).

### Accessories

- **microSD card**: 8 GB minimum, 16 GB recommended
- **Power supply**: the official RPi one, or 5V/3A USB-C (RPi 4) / micro-USB (RPi 3)
- **Ethernet cable**: recommended for audio stability (Wi-Fi works, but adds ~50 ms of jitter to the clock sync)
- **Case** (optional): Audiophonics sell integrated Pi + DAC enclosures

---

## Flashing the SD card

### Step 1 — Download Raspberry Pi Imager

- **macOS**: `brew install --cask raspberry-pi-imager`, or [download it](https://www.raspberrypi.com/software/)
- **Windows**: [download the installer](https://www.raspberrypi.com/software/)
- **Linux**: `sudo apt install rpi-imager`

### Step 2 — Configure and flash

1. Open **Raspberry Pi Imager**
2. **Choose device**: Raspberry Pi 3 or 4, depending on your model
3. **Choose OS**: `Raspberry Pi OS (other)` → **Raspberry Pi OS Lite (64-bit)**
   - The Desktop is not needed; a minimal system is what you want
4. **Choose storage**: your microSD card
5. **Before flashing**, click the gear icon (⚙️) to configure:

| Setting | Recommended value |
|---------|-------------------|
| Hostname | `oaat-endpoint` |
| Enable SSH | Yes, with a password |
| Username | `pi` (or your choice) |
| Password | a strong one |
| Wi-Fi | configure it if you have no Ethernet |
| Time zone | your own |

6. **Flash** — allow 2-3 minutes

### Step 3 — First boot

1. Insert the SD card into the Pi
2. Fit the DAC HAT onto the GPIO header, if it isn't already
3. Connect Ethernet and power
4. Wait 1-2 minutes — the first boot takes a little longer
5. Connect over SSH:

```bash
ssh pi@oaat-endpoint.local
# or by IP address, if .local does not resolve
ssh pi@192.168.1.XX
```

> **Tip**: to find the Pi's IP address, check your router's interface, or run `arp -a | grep raspberry` from your computer.

---

## Installation

### Automatic installation (recommended)

A single command installs everything:

```bash
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

The script asks you to pick your DAC, then:

1. Installs the system dependencies (ALSA, build tools)
2. Configures the DAC overlay in `/boot/config.txt`
3. Configures ALSA (`/etc/asound.conf`)
4. Installs Rust and builds OAAT from source
5. Installs the systemd service

**Build time**:
- RPi 4: **~8 minutes** in `--release`
- RPi 3B+: **~20 minutes** in `--release`

> **Note**: building Rust is memory-hungry. On an RPi 3B+ (1 GB), the build may need a swap file. The script handles that for you.

Once installed:

```bash
sudo reboot
```

The reboot is required to activate the DAC overlay. After restarting, the OAAT service starts automatically.

### Manual installation, step by step

If you would rather control each step:

#### 1. System dependencies

```bash
sudo apt-get update
sudo apt-get install -y libasound2 libasound2-dev alsa-utils \
    curl git build-essential pkg-config
```

#### 2. Configure the DAC

Edit `/boot/firmware/config.txt` (or `/boot/config.txt`, depending on your OS version):

```bash
sudo nano /boot/firmware/config.txt
```

Comment out the on-board audio output and add your DAC's overlay:

```ini
# Disable on-board audio
#dtparam=audio=on

# Your DAC — uncomment ONE line only:
dtoverlay=i-sabre-q2m              # Audiophonics ESS 9038Q2M
#dtoverlay=hifiberry-dacplus        # HifiBerry DAC+ / DAC+ Pro
#dtoverlay=hifiberry-dacplushd      # HifiBerry DAC2 HD
#dtoverlay=allo-boss-dac-pcm512x-audio  # Allo Boss
#dtoverlay=iqaudio-dacplus          # IQaudio DAC+
#dtoverlay=justboom-dac             # JustBoom DAC HAT
```

#### 3. Configure ALSA

```bash
sudo tee /etc/asound.conf << 'EOF'
pcm.!default {
    type hw
    card 0
    device 0
    format S32_LE
}

ctl.!default {
    type hw
    card 0
}
EOF
```

#### 4. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
```

#### 5. Build OAAT

```bash
sudo mkdir -p /opt/oaat
sudo chown $USER:$USER /opt/oaat
git clone https://github.com/renesenses/oaat.git /opt/oaat/src
cd /opt/oaat/src
cargo build --release --bin oaat
cp target/release/oaat /opt/oaat/oaat
```

#### 6. Configure the endpoint

```bash
cp dist/rpi/endpoint.toml /opt/oaat/endpoint.toml
nano /opt/oaat/endpoint.toml
```

Adjust the name and the capabilities to match your DAC — see the [Configuration](#configuration) section.

#### 7. Install the systemd service

```bash
sudo cp dist/oaat-endpoint.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable oaat-endpoint
```

#### 8. Reboot

```bash
sudo reboot
```

---

## Configuration

The configuration file lives at `/opt/oaat/endpoint.toml`.

### Full reference

```toml
[endpoint]
# Name shown in Tune and during mDNS discovery
name = "Living room DAC"

# TCP control port (default: 9740)
port = 9740

# ALSA audio device (default: "default", which uses /etc/asound.conf)
# audio_device = "hw:1,0"

# TLS (off by default; not needed on a LAN)
# tls = false

[capabilities]
# Maximum sample rate, in Hz
pcm_max_rate = 384000

# Maximum bit depth
pcm_max_bits = 32

# Maximum channel count
channels_max = 2

# Native DSD support (most I2S DACs on a RPi do not support native DSD)
dsd = false

# FLAC compression for transport (cuts bandwidth by roughly half)
flac = true

[logging]
# Log level: error, warn, info, debug, trace
level = "info"
```

### Examples, DAC by DAC

#### Audiophonics ESS 9038Q2M

The ES9038Q2M natively supports S16_LE and S32_LE, sample rates up to 384 kHz (and up to 1.536 MHz in DSD mode), and exposes advanced ALSA controls:

- **Digital volume**: 0-100 (-100 dB to 0 dB)
- **FIR filter**: 7 types (brick wall, minimum phase, linear phase)
- **Input selection**: I2S / SPDIF

The [Linux driver](https://github.com/audiophonics/I-Sabre_9038Q2M) uses I2C address `0x48`. On a recent Raspberry Pi OS Bookworm, the `i-sabre-q2m` overlay is generally shipped with the kernel. On an older kernel you will need to build the module from the Audiophonics repository.

```toml
[endpoint]
name = "Audiophonics ESS 9038"

[capabilities]
pcm_max_rate = 384000
pcm_max_bits = 32
channels_max = 2
flac = true
```

#### HifiBerry DAC+ Pro / DAC2 HD

```toml
[endpoint]
name = "HifiBerry DAC"

[capabilities]
pcm_max_rate = 192000
pcm_max_bits = 24
channels_max = 2
flac = true
```

#### Allo Boss

```toml
[endpoint]
name = "Allo Boss DAC"

[capabilities]
pcm_max_rate = 384000
pcm_max_bits = 32
channels_max = 2
flac = true
```

#### IQaudio DAC+

```toml
[endpoint]
name = "IQaudio DAC+"

[capabilities]
pcm_max_rate = 192000
pcm_max_bits = 24
channels_max = 2
flac = true
```

### Advice

- **`pcm_max_rate`**: do not declare more than your DAC genuinely supports. OAAT negotiates downwards automatically when the source exceeds it.
- **`flac = true`**: recommended. It cuts network bandwidth by 50-60% with no loss of quality whatsoever — FLAC is lossless. Particularly useful over Wi-Fi.
- **`name`**: pick something meaningful. This is what appears in Tune as a playback zone.

---

## Checks and first sound

### 1. Check the DAC is detected

```bash
aplay -l
```

Your DAC should appear as card 0:

```
**** List of PLAYBACK Hardware Devices ****
card 0: sndrpies9038q2m [snd_rpi_es9038q2m], device 0: ES9038Q2M HiFi es9038q2m-hifi-0 []
  Subdevices: 1/1
```

If the DAC does not show up, check the overlay in `/boot/firmware/config.txt` and reboot.

### 2. Test sound directly, without OAAT

```bash
speaker-test -D hw:0,0 -c 2 -t sine -f 440
```

You should hear a 440 Hz tone. `Ctrl+C` to stop.

### 3. Check the OAAT service

```bash
sudo systemctl status oaat-endpoint
```

Expected output:

```
● oaat-endpoint.service - OAAT Audio Endpoint (...)
     Active: active (running) since ...
```

### 4. Check mDNS discovery

From another device on the network:

```bash
# macOS
dns-sd -B _oaat._tcp

# Linux
avahi-browse -r _oaat._tcp
```

You should see your endpoint, with its name and its capabilities.

### 5. First sound over OAAT

From your computer, with the `oaat` CLI installed:

```bash
# A 440 Hz sine wave for 5 seconds
oaat controller --target <pi-ip>:9740 --freq 440 --duration 5
```

If you hear the 440 Hz tone, your OAAT endpoint is working and bit-perfect.

### 6. Full conformance test

```bash
oaat-test <pi-ip>:9740
```

Expected result:

```
OAAT Conformance Test — 192.168.1.42:9740
[Handshake]       4 PASS
[Capabilities]    4 PASS
[Format Nego]     3 PASS  (accept, counter, reject)
[Clock Sync]      1 PASS  (offset < 10ms)
[Audio]           1 PASS
[Gapless]         2 PASS  (same format, diff format)
[Volume]          3 PASS
[Reconnect]       2 PASS
20 tests: 20 passed — Endpoint is CONFORMANT
```

---

## Using it with Tune

[Tune](https://mozaiklabs.fr) is a self-hosted music server with native OAAT support.

### Automatic discovery

If Tune runs on the same local network as your RPi, the OAAT endpoint appears as a playback zone in the web interface on its own. No further configuration is needed.

Tune prefers the OAAT endpoint over DLNA and AirPlay, because it offers the most direct and most faithful audio path.

### Playback

1. Open Tune's web interface
2. Select the zone matching your endpoint's name — "Living room DAC", say
3. Start playing an album or a playlist
4. Tune negotiates the best format your DAC supports, automatically

### Supported formats

Negotiation is automatic:

- Source at 24/96 and a DAC that handles 384/32 → playback at 24/96, with no pointless upsampling
- Source at 24/192 and a DAC capped at 96 → Tune downsamples to 24/96, staying in the same 48 kHz family
- Source in DSD and a DAC without DSD support → transparent PCM conversion

---

## Troubleshooting

### No sound

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `aplay -l` lists no card | DAC overlay missing or wrong | Check `/boot/firmware/config.txt`, correct the overlay, reboot |
| `speaker-test` returns error `-121` | Wrong overlay for this DAC | Try another overlay — see the DAC table |
| `speaker-test` works but OAAT does not | Service not started, or misconfigured | `systemctl status oaat-endpoint` and `journalctl -u oaat-endpoint` |
| Crackling or skipping | Unstable Wi-Fi, or too small a buffer | Switch to Ethernet, or raise the buffer |

### The service will not start

```bash
# Read the detailed logs
journalctl -u oaat-endpoint -f

# Common errors:
# "No such device"        → DAC not detected; check the overlay and reboot
# "Address already in use" → another process holds port 9740
# "Permission denied"      → check the User directive in the service file
```

### Tune does not discover the endpoint

- Check that Tune and the RPi sit on the **same subnet**
- If you use a VPN such as NordVPN, enable LAN discovery: `nordvpn set lan-discovery on`
- Check the firewall: ports 9740 (TCP), 9741 (UDP), 9742 (UDP) and 5353 (mDNS) must be open
- Test mDNS discovery by hand — see the checks section above

### Audiophonics ESS 9038Q2M: overlay not found

On some Raspberry Pi OS versions the `i-sabre-q2m` overlay is not shipped with the kernel. The symptom:

```
dtoverlay: failed to apply overlay 'i-sabre-q2m'
```

The fix is to build the driver from the Audiophonics sources:

```bash
sudo apt-get install -y raspberrypi-kernel-headers
git clone https://github.com/audiophonics/I-Sabre_9038Q2M.git /tmp/i-sabre
cd /tmp/i-sabre
make
sudo make install
sudo reboot
```

After rebooting, check:

```bash
aplay -l
# should show: card X: DAC [I-Sabre Q2M DAC], device 0: ...
```

### RPi 3 versus RPi 4

| Aspect | RPi 3B+ | RPi 4B |
|--------|---------|--------|
| Max PCM over I2S | 192 kHz | 384 kHz |
| Build time | ~20 min | ~8 min |
| RAM | 1 GB (swap recommended) | 2-8 GB |
| Ethernet | 100 Mbps, over USB | Native gigabit |
| Wi-Fi | 2.4/5 GHz | 2.4/5 GHz, better |

The RPi 3B+ is perfectly good from 16/44.1 (CD) up to 24/192 (hi-res). If you mostly listen to CD-quality FLAC or Qobuz at 24/96, a second-hand RPi 3 at EUR 25 does the job well.

---

## Going further

### Updating the endpoint

```bash
cd /opt/oaat/src
git pull
cargo build --release --bin oaat
cp target/release/oaat /opt/oaat/oaat
sudo systemctl restart oaat-endpoint
```

### Synchronised multi-room

Two or more Raspberry Pis on the same network can be synchronised to under a millisecond:

```
                    ┌─────────────────┐
                    │   Tune Server   │
                    │   (controller)  │
                    └────────┬────────┘
                             │
                    ┌────────┴────────┐
                    │                 │
              ┌─────┴─────┐    ┌─────┴─────┐
              │  RPi #1   │    │  RPi #2   │
              │  Living   │    │  Kitchen  │
              │  ESS 9038 │    │  HifiBerry│
              └───────────┘    └───────────┘
                    ▲                ▲
                    │  same PTS      │
                    │  < 1 ms sync   │
                    └────────────────┘
```

Every endpoint receives the same audio packets carrying the same presentation timestamp (PTS). PTP synchronisation corrects the clock differences between the Pis automatically.

In Tune, creating a zone that groups several endpoints is all it takes to turn multi-room on.

### Fully headless

The setup described here is already 100% headless. Once flashed and booted, the Pi runs on its own:

- The OAAT service starts at boot
- It restarts automatically after a crash (systemd `Restart=always`)
- It reconnects automatically if the controller drops
- It can be updated over SSH

### Network performance

| Transport | Bandwidth | Latency |
|-----------|-----------|---------|
| PCM 16/44.1 stereo | 1.41 Mbps | - |
| PCM 24/192 stereo | 9.22 Mbps | - |
| PCM 32/384 stereo | 24.58 Mbps | - |
| FLAC 24/192 stereo | ~4 Mbps | +5 ms decoding |
| Ethernet 100 Mbps (RPi 3) | Fine for everything | < 1 ms |
| Wi-Fi 5 GHz | Fine for everything | ~50 ms clock offset |

> **Recommendation**: Ethernet for synchronised multi-room. Wi-Fi is perfectly fine for a single endpoint.

---

## Licence

**Two different things, two different licences.**

\* **The protocol specification** (`docs/rfc.md`) is an open standard:
the [Community Specification License 1.0](LICENSE-SPEC.md), with
**royalty-free patent licences** from every contributor and between
implementers. Anyone — including a manufacturer shipping a commercial device — may
implement OAAT. No fee, no certification cost, no separate agreement to
negotiate.

\*\* **The reference implementation** (`crates/`) is open source under the
[Apache License 2.0](../LICENSE). Embed it, modify it, ship it in a product you
sell — there is nothing to negotiate and nobody to notify.

Nothing in this guide requires permission from anyone.

---

## Links

- [OAAT source code](https://github.com/renesenses/oaat) (implementation under Apache 2.0)
- [RFC specification](https://mozaiklabs.fr/docs/oaat)
- [Tune — music server](https://mozaiklabs.fr)
- [MozAIk Labs forum](https://mozaiklabs.fr/forum)
- [Audiophonics](https://www.audiophonics.fr) — I2S DACs and RPi cases

---

*Written by MozAIk Labs — last updated: September 2026*
