# OAAT Endpoint — Raspberry Pi

Setup scripts for running an OAAT audio endpoint on Raspberry Pi 3B+, 4, or 5 with an I2S DAC HAT.

## Quick setup

```bash
ssh pi@oaat-endpoint.local
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

The script prompts for your DAC model and handles everything automatically.

## Supported DACs

| DAC | Overlay | PCM max |
|-----|---------|---------|
| Audiophonics ESS 9038Q2M | `i-sabre-q2m` | 384 kHz / 32 bits |
| HifiBerry DAC+ / DAC+ Pro | `hifiberry-dacplus` | 192 kHz / 32 bits |
| HifiBerry DAC2 HD | `hifiberry-dacplushd` | 192 kHz / 24 bits |
| Allo Boss | `allo-boss-dac-pcm512x-audio` | 384 kHz / 32 bits |
| IQaudio DAC+ | `iqaudio-dacplus` | 192 kHz / 24 bits |
| JustBoom DAC HAT | `justboom-dac` | 384 kHz / 32 bits |

## Non-interactive install

```bash
sudo bash setup.sh --dac ess9038
```

## Full guide (FR)

See [docs/howto-rpi-endpoint.md](../../docs/howto-rpi-endpoint.md) for the complete step-by-step guide in French, including manual installation, configuration reference, troubleshooting, and multi-room setup.

## Files

- `setup.sh` — Automated setup script (interactive DAC selection)
- `endpoint.toml` — Default endpoint config (Audiophonics ESS 9038)

## After setup

```bash
sudo reboot                              # Activate DAC overlay
sudo systemctl status oaat-endpoint      # Check service
aplay -l                                 # Verify DAC detection
speaker-test -D hw:0,0 -c 2 -t sine     # Test audio directly
oaat-test <pi-ip>:9740                   # Conformance test (from another machine)
```
