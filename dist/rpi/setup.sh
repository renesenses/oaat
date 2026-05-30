#!/bin/bash
# OAAT Endpoint setup for Raspberry Pi 3B+ / 4 / 5
# Supports multiple I2S DAC HATs
# Run as root: sudo bash setup.sh [--dac <model>]
#
# Supported DAC models:
#   ess9038      Audiophonics ESS 9038Q2M
#   hifiberry    HifiBerry DAC+ / DAC+ Pro
#   hifiberry-hd HifiBerry DAC2 HD
#   boss         Allo Boss
#   iqaudio      IQaudio DAC+
#   justboom     JustBoom DAC HAT

set -e

echo "=== OAAT Endpoint — Raspberry Pi Setup ==="
echo ""

# --- DAC selection ---

declare -A DAC_OVERLAYS=(
    [ess9038]="i-sabre-q2m"
    [hifiberry]="hifiberry-dacplus"
    [hifiberry-hd]="hifiberry-dacplushd"
    [boss]="allo-boss-dac-pcm512x-audio"
    [iqaudio]="iqaudio-dacplus"
    [justboom]="justboom-dac"
)

declare -A DAC_NAMES=(
    [ess9038]="Audiophonics ESS 9038Q2M"
    [hifiberry]="HifiBerry DAC+ / DAC+ Pro"
    [hifiberry-hd]="HifiBerry DAC2 HD"
    [boss]="Allo Boss"
    [iqaudio]="IQaudio DAC+"
    [justboom]="JustBoom DAC HAT"
)

declare -A DAC_MAX_RATE=(
    [ess9038]=384000
    [hifiberry]=192000
    [hifiberry-hd]=192000
    [boss]=384000
    [iqaudio]=192000
    [justboom]=384000
)

declare -A DAC_MAX_BITS=(
    [ess9038]=32
    [hifiberry]=32
    [hifiberry-hd]=24
    [boss]=32
    [iqaudio]=24
    [justboom]=32
)

DAC_MODEL=""

# Parse --dac argument
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dac)
            DAC_MODEL="$2"
            shift 2
            ;;
        *)
            echo "Usage: sudo bash setup.sh [--dac <model>]"
            echo "Models: ${!DAC_OVERLAYS[*]}"
            exit 1
            ;;
    esac
done

# Interactive selection if not provided
if [ -z "$DAC_MODEL" ]; then
    echo "Quel DAC I2S utilisez-vous ?"
    echo ""
    echo "  1) Audiophonics ESS 9038Q2M"
    echo "  2) HifiBerry DAC+ / DAC+ Pro"
    echo "  3) HifiBerry DAC2 HD"
    echo "  4) Allo Boss"
    echo "  5) IQaudio DAC+"
    echo "  6) JustBoom DAC HAT"
    echo ""
    read -p "Votre choix [1-6] : " choice
    case "$choice" in
        1) DAC_MODEL="ess9038" ;;
        2) DAC_MODEL="hifiberry" ;;
        3) DAC_MODEL="hifiberry-hd" ;;
        4) DAC_MODEL="boss" ;;
        5) DAC_MODEL="iqaudio" ;;
        6) DAC_MODEL="justboom" ;;
        *)
            echo "Choix invalide."
            exit 1
            ;;
    esac
fi

# Validate
if [ -z "${DAC_OVERLAYS[$DAC_MODEL]}" ]; then
    echo "Modèle de DAC inconnu: $DAC_MODEL"
    echo "Modèles supportés: ${!DAC_OVERLAYS[*]}"
    exit 1
fi

DAC_OVERLAY="${DAC_OVERLAYS[$DAC_MODEL]}"
DAC_NAME="${DAC_NAMES[$DAC_MODEL]}"
MAX_RATE="${DAC_MAX_RATE[$DAC_MODEL]}"
MAX_BITS="${DAC_MAX_BITS[$DAC_MODEL]}"

echo "DAC sélectionné : $DAC_NAME"
echo "  Overlay : $DAC_OVERLAY"
echo "  PCM max : $((MAX_RATE / 1000)) kHz / ${MAX_BITS} bits"
echo ""

# 1. System deps
echo "[1/7] Installation des dépendances système..."
apt-get update -qq
apt-get install -y --no-install-recommends \
    libasound2 libasound2-dev alsa-utils \
    curl git build-essential pkg-config

# 2. Swap (for RPi 3 with 1GB RAM)
TOTAL_MEM=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)
if [ "$TOTAL_MEM" -lt 1500 ]; then
    echo "[2/7] Configuration du swap (RAM < 1.5 Go)..."
    if [ ! -f /swapfile ] || [ "$(stat -c%s /swapfile 2>/dev/null)" -lt 1073741824 ]; then
        swapoff /swapfile 2>/dev/null || true
        dd if=/dev/zero of=/swapfile bs=1M count=1024 status=progress
        chmod 600 /swapfile
        mkswap /swapfile
        swapon /swapfile
        grep -q '/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
        echo "  -> Swap 1 Go activé"
    else
        echo "  -> Swap déjà configuré"
    fi
else
    echo "[2/7] Swap — non nécessaire (${TOTAL_MEM} Mo de RAM)"
fi

# 3. DAC overlay
echo "[3/7] Configuration de l'overlay DAC..."
CONFIG=/boot/firmware/config.txt
[ -f /boot/config.txt ] && ! [ -f "$CONFIG" ] && CONFIG=/boot/config.txt

# Remove onboard audio and any existing DAC overlay
if ! grep -q "dtoverlay=$DAC_OVERLAY" "$CONFIG"; then
    sed -i 's/^dtparam=audio=on/#dtparam=audio=on/' "$CONFIG"
    # Remove any previous OAAT DAC overlay
    sed -i '/# OAAT DAC/d' "$CONFIG"
    for overlay in i-sabre-q2m hifiberry-dacplus hifiberry-dacplushd allo-boss-dac-pcm512x-audio iqaudio-dacplus justboom-dac; do
        sed -i "/^dtoverlay=$overlay$/d" "$CONFIG"
    done
    echo "" >> "$CONFIG"
    echo "# OAAT DAC — $DAC_NAME" >> "$CONFIG"
    echo "dtoverlay=$DAC_OVERLAY" >> "$CONFIG"
    echo "  -> Overlay $DAC_OVERLAY ajouté dans $CONFIG"
    NEEDS_REBOOT=1
else
    echo "  -> Overlay $DAC_OVERLAY déjà configuré"
fi

# 4. ALSA config
echo "[4/7] Configuration ALSA..."
cat > /etc/asound.conf << 'ALSA'
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
echo "  -> /etc/asound.conf (hw:0,0 S32_LE)"

# 5. Install Rust (if not present)
echo "[5/7] Vérification de Rust..."
REAL_USER=$(logname 2>/dev/null || echo "${SUDO_USER:-pi}")
REAL_HOME=$(eval echo "~$REAL_USER")

if ! su - "$REAL_USER" -c 'command -v cargo' &>/dev/null; then
    echo "  -> Installation de Rust..."
    su - "$REAL_USER" -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
else
    RUST_VER=$(su - "$REAL_USER" -c 'rustc --version' 2>/dev/null)
    echo "  -> Rust déjà installé : $RUST_VER"
fi

# 6. Build OAAT
echo "[6/7] Compilation de OAAT (release)..."
OAAT_DIR=/opt/oaat
mkdir -p "$OAAT_DIR"
if [ ! -d "$OAAT_DIR/src" ]; then
    git clone https://github.com/renesenses/oaat.git "$OAAT_DIR/src"
else
    cd "$OAAT_DIR/src" && git pull
fi

cd "$OAAT_DIR/src"
echo "  -> cargo build --release (cela peut prendre 8-20 min selon le modèle)..."
su - "$REAL_USER" -c "cd $OAAT_DIR/src && source ~/.cargo/env && cargo build --release --bin oaat"
cp target/release/oaat "$OAAT_DIR/oaat"

# Generate config for this DAC
cat > "$OAAT_DIR/endpoint.toml" << EOF
[endpoint]
name = "$DAC_NAME"
port = 9740

[capabilities]
pcm_max_rate = $MAX_RATE
pcm_max_bits = $MAX_BITS
channels_max = 2
dsd = false
flac = true

[logging]
level = "info"
EOF

chown -R "$REAL_USER":"$REAL_USER" "$OAAT_DIR"
BINARY_SIZE=$(du -h "$OAAT_DIR/oaat" | cut -f1)
echo "  -> Binaire : $OAAT_DIR/oaat ($BINARY_SIZE)"
echo "  -> Config  : $OAAT_DIR/endpoint.toml"

# 7. Systemd service
echo "[7/7] Installation du service systemd..."
cat > /etc/systemd/system/oaat-endpoint.service << EOF
[Unit]
Description=OAAT Audio Endpoint ($DAC_NAME)
After=network-online.target sound.target
Wants=network-online.target

[Service]
Type=simple
User=$REAL_USER
ExecStart=/opt/oaat/oaat endpoint --daemon --config /opt/oaat/endpoint.toml
Restart=always
RestartSec=3
Environment=RUST_LOG=oaat=info

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable oaat-endpoint
echo "  -> Service oaat-endpoint activé"

echo ""
echo "=== Installation terminée ==="
echo ""
echo "DAC      : $DAC_NAME"
echo "Overlay  : $DAC_OVERLAY"
echo "Binaire  : $OAAT_DIR/oaat ($BINARY_SIZE)"
echo "Config   : $OAAT_DIR/endpoint.toml"
echo "Service  : systemctl {start|stop|status} oaat-endpoint"
echo ""

if [ "${NEEDS_REBOOT:-0}" = "1" ]; then
    echo "╔══════════════════════════════════════════════════════╗"
    echo "║  REBOOT NÉCESSAIRE pour activer l'overlay du DAC    ║"
    echo "║                                                      ║"
    echo "║  sudo reboot                                         ║"
    echo "║                                                      ║"
    echo "║  Après redémarrage, le service OAAT démarre          ║"
    echo "║  automatiquement.                                    ║"
    echo "╚══════════════════════════════════════════════════════╝"
else
    echo "Démarrage du endpoint..."
    systemctl start oaat-endpoint
    sleep 2
    systemctl status oaat-endpoint --no-pager
fi
