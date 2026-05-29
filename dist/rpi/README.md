# OAAT Endpoint — Raspberry Pi 4 + Audiophonics ESS 9038Q2M

## Quick setup

```bash
# SSH into the Pi
ssh pi@<pi-ip>

# Download and run setup
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

## What the setup does

1. Installs ALSA + build tools
2. Configures `hifiberry-dacplus` overlay for the ESS 9038 I2S DAC
3. Sets ALSA default to hw:0,0 with S32_LE format
4. Installs Rust and builds oaat from source
5. Creates `/opt/oaat/` with binary + config
6. Installs + enables systemd service

## Manual steps

After setup, if the DAC overlay was just added:
```bash
sudo reboot
# After reboot:
sudo systemctl start oaat-endpoint
```

## Verify

```bash
# Check service
sudo systemctl status oaat-endpoint

# Check DAC is detected
aplay -l

# Test audio directly
speaker-test -D hw:0,0 -c 2 -t sine -f 440

# Run conformance test from another machine
oaat-test <pi-ip>:9740
```

## Config

Edit `/opt/oaat/endpoint.toml`:
```toml
[endpoint]
name = "Audiophonics ESS 9038"
port = 9740

[capabilities]
pcm_max_rate = 384000   # ESS9038 supports up to 384kHz
pcm_max_bits = 32       # 32-bit capable
channels_max = 2
flac = true
```

## Troubleshooting

**No sound?**
- `aplay -l` — verify DAC is listed as card 0
- `cat /proc/asound/cards` — check ALSA cards
- Check `/boot/firmware/config.txt` has `dtoverlay=hifiberry-dacplus`

**Service won't start?**
- `journalctl -u oaat-endpoint -f` — check logs
- Verify ALSA works: `speaker-test -D hw:0,0 -c 2`

**Wrong DAC overlay?**
Some Audiophonics boards need `dtoverlay=allo-boss-dac-pcm512x-audio` or `dtoverlay=es9038q2m`. Try these if hifiberry-dacplus doesn't work.
