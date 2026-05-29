#!/bin/bash
# OAAT Endpoint setup for Raspberry Pi 4 + Audiophonics ESS 9038Q2M DAC
# Run as root: sudo bash setup.sh

set -e

echo "=== OAAT Endpoint — Raspberry Pi Setup ==="
echo ""

# 1. System deps
echo "[1/6] Installing system dependencies..."
apt-get update -qq
apt-get install -y --no-install-recommends \
    libasound2 libasound2-dev alsa-utils \
    curl git build-essential pkg-config

# 2. DAC overlay (Audiophonics ESS 9038Q2M uses hifiberry-dacplus compatible I2S)
echo "[2/6] Configuring DAC overlay..."
CONFIG=/boot/firmware/config.txt
[ -f /boot/config.txt ] && CONFIG=/boot/config.txt

# Remove onboard audio, add DAC
if ! grep -q "dtoverlay=hifiberry-dacplus" "$CONFIG"; then
    sed -i 's/^dtparam=audio=on/#dtparam=audio=on/' "$CONFIG"
    echo "" >> "$CONFIG"
    echo "# Audiophonics ESS 9038Q2M DAC (I2S)" >> "$CONFIG"
    echo "dtoverlay=hifiberry-dacplus" >> "$CONFIG"
    echo "  -> Added hifiberry-dacplus overlay to $CONFIG"
    echo "  -> REBOOT REQUIRED after setup completes"
    NEEDS_REBOOT=1
else
    echo "  -> DAC overlay already configured"
fi

# 3. ALSA config
echo "[3/6] Configuring ALSA..."
cat > /etc/asound.conf << 'ALSA'
# OAAT Endpoint — Audiophonics ESS 9038Q2M
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
ALSA
echo "  -> /etc/asound.conf written (hw:0,0 S32_LE)"

# 4. Install Rust (if not present)
echo "[4/6] Checking Rust toolchain..."
if ! command -v cargo &>/dev/null; then
    echo "  -> Installing Rust..."
    su - "$(logname)" -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
    source "/home/$(logname)/.cargo/env"
else
    echo "  -> Rust already installed: $(rustc --version)"
fi

# 5. Build OAAT
echo "[5/6] Building OAAT endpoint (release)..."
OAAT_DIR=/opt/oaat
mkdir -p "$OAAT_DIR"
if [ ! -d "$OAAT_DIR/src" ]; then
    git clone https://github.com/renesenses/oaat.git "$OAAT_DIR/src"
else
    cd "$OAAT_DIR/src" && git pull
fi

cd "$OAAT_DIR/src"
su - "$(logname)" -c "cd $OAAT_DIR/src && source ~/.cargo/env && cargo build --release --bin oaat"
cp target/release/oaat "$OAAT_DIR/oaat"
cp dist/rpi/endpoint.toml "$OAAT_DIR/endpoint.toml"
chown -R "$(logname)":"$(logname)" "$OAAT_DIR"
echo "  -> Binary: $OAAT_DIR/oaat ($(du -h $OAAT_DIR/oaat | cut -f1))"

# 6. Systemd service
echo "[6/6] Installing systemd service..."
cat > /etc/systemd/system/oaat-endpoint.service << 'SVC'
[Unit]
Description=OAAT Audio Endpoint (Audiophonics ESS 9038)
After=network-online.target sound.target
Wants=network-online.target

[Service]
Type=simple
User=pi
ExecStart=/opt/oaat/oaat endpoint --daemon --config /opt/oaat/endpoint.toml
Restart=always
RestartSec=3
Environment=RUST_LOG=oaat=info

[Install]
WantedBy=multi-user.target
SVC

# Adjust user if not 'pi'
REAL_USER=$(logname)
sed -i "s/User=pi/User=$REAL_USER/" /etc/systemd/system/oaat-endpoint.service

systemctl daemon-reload
systemctl enable oaat-endpoint
echo "  -> Service enabled: oaat-endpoint"

echo ""
echo "=== Setup complete ==="
echo ""
echo "Binary:  $OAAT_DIR/oaat"
echo "Config:  $OAAT_DIR/endpoint.toml"
echo "Service: systemctl start oaat-endpoint"
echo ""

if [ "${NEEDS_REBOOT:-0}" = "1" ]; then
    echo "*** REBOOT REQUIRED for DAC overlay to take effect ***"
    echo "Run: sudo reboot"
    echo "Then: sudo systemctl start oaat-endpoint"
else
    echo "Starting endpoint now..."
    systemctl start oaat-endpoint
    sleep 2
    systemctl status oaat-endpoint --no-pager
fi
