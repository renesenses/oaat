# Transformer un Raspberry Pi en endpoint audio OAAT

Guide complet pour configurer un Raspberry Pi 3B+ ou 4 comme endpoint audio bit-perfect avec le protocole OAAT.

## Sommaire

1. [Pourquoi OAAT sur Raspberry Pi ?](#pourquoi-oaat-sur-raspberry-pi-)
2. [Matériel nécessaire](#matériel-nécessaire)
3. [Flasher la carte SD](#flasher-la-carte-sd)
4. [Installation](#installation)
5. [Configuration](#configuration)
6. [Vérification et premier son](#vérification-et-premier-son)
7. [Utilisation avec Tune](#utilisation-avec-tune)
8. [Troubleshooting](#troubleshooting)
9. [Aller plus loin](#aller-plus-loin)

---

## Pourquoi OAAT sur Raspberry Pi ?

OAAT (Open Advanced Audio Transport) est un protocole de transport audio réseau open-source, conçu comme alternative ouverte au RAAT propriétaire de Roon. Il offre :

- **Audio bit-perfect** : PCM jusqu'à 768 kHz / 32 bits, DSD natif jusqu'à DSD512
- **Synchronisation multi-room** : < 1 ms entre endpoints, via horloge PTP
- **Négociation de format** : le contrôleur s'adapte automatiquement aux capacités du DAC
- **Gapless** : transitions sans coupure, même lors d'un changement de format
- **Zéro configuration** : découverte automatique via mDNS/DNS-SD

| | OAAT | RAAT (Roon) | DLNA/UPnP | AirPlay 2 |
|---|---|---|---|---|
| Licence | Apache 2.0 | Propriétaire | UPnP Forum | Apple |
| Bit-perfect | Oui | Oui | Variable | Non |
| DSD natif | Oui | Oui | DoP seulement | Non |
| Multi-room sync | < 1 ms | < 1 ms | Aucun | ~Apple only |
| Open source | Oui | Non | Oui | Non |

Le Raspberry Pi est le compagnon idéal :

- **Prix** : 35-75 EUR selon le modèle, un endpoint audio haut de gamme pour le prix d'un repas
- **Silence** : pas de ventilateur, zéro bruit mécanique
- **I2S natif** : connexion directe au DAC via le bus I2S du GPIO, sans passer par USB — le chemin le plus court et le plus pur vers le DAC
- **Compact** : se glisse derrière un ampli ou dans un boîtier Audiophonics

---

## Matériel nécessaire

### Le Raspberry Pi

| Modèle | PCM max | RAM | Prix indicatif | Recommandation |
|--------|---------|-----|----------------|----------------|
| **RPi 4B** (2 ou 4 Go) | 384 kHz / 32 bits | 2-8 Go | ~55-75 EUR | Recommandé |
| **RPi 3B+** | 192 kHz / 32 bits | 1 Go | ~35-45 EUR | Budget / occasion |
| RPi 5 | 384 kHz / 32 bits | 4-8 Go | ~70-90 EUR | Fonctionne aussi |

> **Note** : Le RPi 3B+ est limité à 192 kHz par son bus I2S. Pour le hi-res au-delà (352.8 / 384 kHz), préférez le RPi 4.

### Le DAC I2S (HAT)

OAAT supporte tous les DAC I2S compatibles Raspberry Pi. Voici les plus courants :

| DAC | Chipset | PCM max | Prix | Overlay Linux |
|-----|---------|---------|------|---------------|
| **Audiophonics ESS 9038Q2M** | ESS ES9038Q2M | 384 kHz / 32 bits | ~120 EUR | `i-sabre-q2m` ([driver](https://github.com/audiophonics/I-Sabre_9038Q2M)) |
| **HifiBerry DAC2 HD** | PCM1796 | 192 kHz / 24 bits | ~65 EUR | `hifiberry-dacplushd` |
| **HifiBerry DAC+ Pro** | PCM5122 | 192 kHz / 32 bits | ~45 EUR | `hifiberry-dacplus` |
| **Allo Boss** | PCM5122 | 384 kHz / 32 bits | ~50 EUR | `allo-boss-dac-pcm512x-audio` |
| **IQaudio DAC+** | PCM5122 | 192 kHz / 32 bits | ~30 EUR | `iqaudio-dacplus` |
| **JustBoom DAC HAT** | PCM5122 | 384 kHz / 32 bits | ~35 EUR | `justboom-dac` |

> **Important** : L'overlay Linux doit correspondre exactement à votre DAC. Un mauvais overlay = pas de son (erreur ALSA `-121` ou `-22`).

### Accessoires

- **Carte microSD** : 8 Go minimum (16 Go recommandé)
- **Alimentation** : officielle RPi ou 5V/3A USB-C (RPi 4) / micro-USB (RPi 3)
- **Câble Ethernet** : recommandé pour la stabilité audio (le WiFi fonctionne mais ajoute ~50 ms de jitter sur la clock sync)
- **Boîtier** (optionnel) : Audiophonics propose des boîtiers intégrés Pi + DAC

---

## Flasher la carte SD

### Étape 1 : Télécharger Raspberry Pi Imager

- **macOS** : `brew install --cask raspberry-pi-imager` ou [télécharger](https://www.raspberrypi.com/software/)
- **Windows** : [télécharger l'installeur](https://www.raspberrypi.com/software/)
- **Linux** : `sudo apt install rpi-imager`

### Étape 2 : Configurer et flasher

1. Ouvrir **Raspberry Pi Imager**
2. **Choisir l'appareil** : Raspberry Pi 3 ou 4 selon votre modèle
3. **Choisir l'OS** : `Raspberry Pi OS (other)` → **Raspberry Pi OS Lite (64-bit)**
   - Pas besoin du Desktop, on veut un système minimal
4. **Choisir le stockage** : votre carte microSD
5. **Avant de flasher**, cliquer sur l'engrenage (⚙️) pour configurer :

| Paramètre | Valeur recommandée |
|-----------|-------------------|
| Hostname | `oaat-endpoint` |
| Activer SSH | Oui, avec mot de passe |
| Nom d'utilisateur | `pi` (ou votre choix) |
| Mot de passe | un mot de passe solide |
| WiFi | configurer si pas d'Ethernet |
| Fuseau horaire | `Europe/Paris` |

6. **Flasher** — compter 2-3 minutes

### Étape 3 : Premier démarrage

1. Insérer la carte SD dans le Pi
2. Brancher le DAC HAT sur le GPIO (si pas déjà monté)
3. Brancher Ethernet + alimentation
4. Attendre 1-2 minutes (premier boot un peu plus long)
5. Se connecter en SSH :

```bash
ssh pi@oaat-endpoint.local
# ou avec l'adresse IP si .local ne fonctionne pas
ssh pi@192.168.1.XX
```

> **Astuce** : Pour trouver l'IP du Pi, consultez l'interface de votre box/routeur, ou utilisez `arp -a | grep raspberry` depuis votre ordinateur.

---

## Installation

### Installation automatique (recommandée)

Une seule commande installe tout :

```bash
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

Le script vous demande de choisir votre DAC, puis :

1. Installe les dépendances système (ALSA, outils de build)
2. Configure l'overlay du DAC dans `/boot/config.txt`
3. Configure ALSA (`/etc/asound.conf`)
4. Installe Rust et compile OAAT depuis les sources
5. Installe le service systemd

**Temps de compilation** :
- RPi 4 : **~8 minutes** en `--release`
- RPi 3B+ : **~20 minutes** en `--release`

> **Note** : La compilation Rust est gourmande en RAM. Sur RPi 3B+ (1 Go), le build peut nécessiter un fichier swap. Le script le gère automatiquement.

Après l'installation :

```bash
sudo reboot
```

Le reboot est nécessaire pour activer l'overlay du DAC. Après redémarrage, le service OAAT démarre automatiquement.

### Installation manuelle (étape par étape)

Si vous préférez contrôler chaque étape :

#### 1. Dépendances système

```bash
sudo apt-get update
sudo apt-get install -y libasound2 libasound2-dev alsa-utils \
    curl git build-essential pkg-config
```

#### 2. Configurer le DAC

Éditer `/boot/firmware/config.txt` (ou `/boot/config.txt` selon la version de l'OS) :

```bash
sudo nano /boot/firmware/config.txt
```

Commenter la sortie audio embarquée et ajouter l'overlay de votre DAC :

```ini
# Désactiver l'audio embarqué
#dtparam=audio=on

# Votre DAC — décommentez UNE SEULE ligne :
dtoverlay=i-sabre-q2m              # Audiophonics ESS 9038Q2M
#dtoverlay=hifiberry-dacplus        # HifiBerry DAC+ / DAC+ Pro
#dtoverlay=hifiberry-dacplushd      # HifiBerry DAC2 HD
#dtoverlay=allo-boss-dac-pcm512x-audio  # Allo Boss
#dtoverlay=iqaudio-dacplus          # IQaudio DAC+
#dtoverlay=justboom-dac             # JustBoom DAC HAT
```

#### 3. Configurer ALSA

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

#### 4. Installer Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
```

#### 5. Compiler OAAT

```bash
sudo mkdir -p /opt/oaat
sudo chown $USER:$USER /opt/oaat
git clone https://github.com/renesenses/oaat.git /opt/oaat/src
cd /opt/oaat/src
cargo build --release --bin oaat
cp target/release/oaat /opt/oaat/oaat
```

#### 6. Configurer l'endpoint

```bash
cp dist/rpi/endpoint.toml /opt/oaat/endpoint.toml
nano /opt/oaat/endpoint.toml
```

Adapter le nom et les capacités à votre DAC (voir section [Configuration](#configuration)).

#### 7. Installer le service systemd

```bash
sudo cp dist/oaat-endpoint.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable oaat-endpoint
```

#### 8. Rebooter

```bash
sudo reboot
```

---

## Configuration

Le fichier de configuration se trouve dans `/opt/oaat/endpoint.toml`.

### Référence complète

```toml
[endpoint]
# Nom affiché dans Tune et lors de la découverte mDNS
name = "Salon DAC"

# Port TCP pour le contrôle (défaut : 9740)
port = 9740

# Périphérique audio ALSA (défaut : "default", utilise /etc/asound.conf)
# audio_device = "hw:1,0"

# TLS (désactivé par défaut, pas nécessaire en LAN)
# tls = false

[capabilities]
# Fréquence d'échantillonnage maximale en Hz
pcm_max_rate = 384000

# Résolution maximale en bits
pcm_max_bits = 32

# Nombre de canaux max
channels_max = 2

# Support DSD natif (la plupart des DAC I2S via RPi ne supportent pas le DSD natif)
dsd = false

# Compression FLAC pour le transport (réduit la bande passante de ~50%)
flac = true

[logging]
# Niveau de log : error, warn, info, debug, trace
level = "info"
```

### Exemples par DAC

#### Audiophonics ESS 9038Q2M

Le ES9038Q2M supporte nativement S16_LE et S32_LE, des taux d'échantillonnage jusqu'à 384 kHz (voire 1.536 MHz en mode DSD), et dispose de contrôles ALSA avancés :

- **Volume numérique** : 0-100 (-100 dB à 0 dB)
- **Filtre FIR** : 7 types (brick wall, minimum phase, linear phase)
- **Sélection entrée** : I2S / SPDIF

Le [driver Linux](https://github.com/audiophonics/I-Sabre_9038Q2M) utilise l'adresse I2C `0x48`. Sur Raspberry Pi OS Bookworm récent, l'overlay `i-sabre-q2m` est généralement inclus dans le noyau. Pour un noyau plus ancien, il faut compiler le module depuis le dépôt Audiophonics.

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

### Conseils

- **`pcm_max_rate`** : ne déclarez pas plus que ce que votre DAC supporte réellement. OAAT négocie automatiquement vers le bas si la source dépasse.
- **`flac = true`** : recommandé. Réduit la bande passante réseau de ~50-60% sans aucune perte de qualité (FLAC est lossless). Particulièrement utile en WiFi.
- **`name`** : choisissez un nom parlant, c'est ce qui apparaît dans l'interface de Tune comme zone de lecture.

---

## Vérification et premier son

### 1. Vérifier que le DAC est détecté

```bash
aplay -l
```

Vous devez voir votre DAC comme carte 0 :

```
**** Liste des Périphériques Matériels PLAYBACK ****
carte 0: sndrpies9038q2m [snd_rpi_es9038q2m], périphérique 0: ES9038Q2M HiFi es9038q2m-hifi-0 []
  Sous-périphériques: 1/1
```

Si le DAC n'apparaît pas, vérifiez l'overlay dans `/boot/firmware/config.txt` et redémarrez.

### 2. Tester le son directement (sans OAAT)

```bash
speaker-test -D hw:0,0 -c 2 -t sine -f 440
```

Vous devez entendre un la 440 Hz. `Ctrl+C` pour arrêter.

### 3. Vérifier le service OAAT

```bash
sudo systemctl status oaat-endpoint
```

Sortie attendue :

```
● oaat-endpoint.service - OAAT Audio Endpoint (...)
     Active: active (running) since ...
```

### 4. Vérifier la découverte mDNS

Depuis un autre appareil sur le réseau :

```bash
# macOS
dns-sd -B _oaat._tcp

# Linux
avahi-browse -r _oaat._tcp
```

Vous devez voir votre endpoint avec son nom et ses capacités.

### 5. Premier son via OAAT

Depuis votre ordinateur (avec le CLI oaat installé) :

```bash
# Sinusoïde 440 Hz pendant 5 secondes
oaat controller --target <ip-du-pi>:9740 --freq 440 --duration 5
```

Si vous entendez le la 440 Hz, votre endpoint OAAT est fonctionnel et bit-perfect.

### 6. Test de conformité complet

```bash
oaat-test <ip-du-pi>:9740
```

Résultat attendu :

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

## Utilisation avec Tune

[Tune](https://mozaiklabs.fr) est un serveur de musique auto-hébergé qui intègre nativement OAAT.

### Découverte automatique

Si Tune tourne sur le même réseau local que votre RPi, l'endpoint OAAT apparaît automatiquement comme zone de lecture dans l'interface web. Aucune configuration supplémentaire n'est nécessaire.

L'endpoint OAAT est prioritaire sur DLNA et AirPlay dans Tune, car il offre le chemin audio le plus direct et le plus fidèle.

### Lecture

1. Ouvrir l'interface web de Tune
2. Sélectionner la zone correspondant au nom de votre endpoint (ex: "Salon DAC")
3. Lancer la lecture d'un album ou d'une playlist
4. Tune négocie automatiquement le meilleur format supporté par votre DAC

### Formats supportés

La négociation est automatique :

- Si votre source est en 24/96 et le DAC supporte 384/32 → lecture en 24/96 (pas d'upsampling inutile)
- Si votre source est en 24/192 et le DAC supporte max 96 → Tune downsample à 24/96 (même famille 48 kHz)
- Si votre source est en DSD et le DAC ne supporte pas le DSD → conversion PCM transparente

---

## Troubleshooting

### Pas de son

| Symptôme | Cause probable | Solution |
|----------|---------------|----------|
| `aplay -l` ne liste aucune carte | Overlay DAC manquant ou incorrect | Vérifier `/boot/firmware/config.txt`, corriger l'overlay, rebooter |
| `speaker-test` donne une erreur `-121` | Mauvais overlay pour ce DAC | Essayer un autre overlay (voir tableau DACs) |
| `speaker-test` fonctionne mais pas OAAT | Service pas démarré ou mauvaise config | `systemctl status oaat-endpoint` + `journalctl -u oaat-endpoint` |
| Son qui grésille ou saute | WiFi instable ou buffer trop petit | Passer en Ethernet, ou augmenter le buffer dans les logs |

### Le service ne démarre pas

```bash
# Voir les logs détaillés
journalctl -u oaat-endpoint -f

# Erreurs courantes :
# "No such device" → DAC pas détecté, vérifier overlay + reboot
# "Address already in use" → un autre processus utilise le port 9740
# "Permission denied" → vérifier le User dans le fichier service
```

### L'endpoint n'est pas découvert par Tune

- Vérifier que Tune et le RPi sont sur le **même sous-réseau**
- Si vous utilisez un VPN (NordVPN, etc.) : activer `lan-discovery` (`nordvpn set lan-discovery on`)
- Vérifier le pare-feu : les ports 9740 (TCP), 9741 (UDP), 9742 (UDP) et 5353 (mDNS) doivent être ouverts
- Tester la découverte mDNS manuellement (voir section Vérification)

### Audiophonics ESS 9038Q2M : overlay introuvable

Sur certaines versions de Raspberry Pi OS, l'overlay `i-sabre-q2m` n'est pas inclus dans le noyau. Symptôme :

```
dtoverlay: failed to apply overlay 'i-sabre-q2m'
```

Solution : compiler le driver depuis les sources Audiophonics :

```bash
sudo apt-get install -y raspberrypi-kernel-headers
git clone https://github.com/audiophonics/I-Sabre_9038Q2M.git /tmp/i-sabre
cd /tmp/i-sabre
make
sudo make install
sudo reboot
```

Après reboot, vérifier :

```bash
aplay -l
# Doit afficher : card X: DAC [I-Sabre Q2M DAC], device 0: ...
```

### RPi 3 vs RPi 4

| Aspect | RPi 3B+ | RPi 4B |
|--------|---------|--------|
| PCM max via I2S | 192 kHz | 384 kHz |
| Temps de compilation | ~20 min | ~8 min |
| RAM | 1 Go (swap recommandé) | 2-8 Go |
| Ethernet | 100 Mbps (via USB) | Gigabit natif |
| WiFi | 2.4/5 GHz | 2.4/5 GHz, meilleur |

Le RPi 3B+ fonctionne parfaitement pour du 16/44.1 (CD) jusqu'au 24/192 (hi-res). Si vous écoutez principalement du FLAC CD ou du Qobuz 24/96, un RPi 3 d'occasion à 25 EUR fait parfaitement l'affaire.

---

## Aller plus loin

### Mise à jour de l'endpoint

```bash
cd /opt/oaat/src
git pull
cargo build --release --bin oaat
cp target/release/oaat /opt/oaat/oaat
sudo systemctl restart oaat-endpoint
```

### Multi-room synchronisé

Deux (ou plus) Raspberry Pi sur le même réseau peuvent être synchronisés à < 1 ms :

```
                    ┌─────────────────┐
                    │   Tune Server   │
                    │   (contrôleur)  │
                    └────────┬────────┘
                             │
                    ┌────────┴────────┐
                    │                 │
              ┌─────┴─────┐    ┌─────┴─────┐
              │  RPi #1   │    │  RPi #2   │
              │  Salon    │    │  Cuisine  │
              │  ESS 9038 │    │  HifiBerry│
              └───────────┘    └───────────┘
                    ▲                ▲
                    │  même PTS      │
                    │  < 1 ms sync   │
                    └────────────────┘
```

Chaque endpoint reçoit les mêmes paquets audio avec le même timestamp de présentation (PTS). La synchronisation PTP corrige automatiquement les différences d'horloge entre les Pi.

Dans Tune, il suffit de créer une zone groupant plusieurs endpoints pour activer le multi-room.

### Headless complet (sans écran ni clavier)

Le setup décrit ici est déjà 100% headless. Une fois flashé et démarré, le Pi est autonome :

- Le service OAAT démarre au boot
- Redémarrage automatique en cas de crash (systemd `Restart=always`)
- Reconnexion automatique si le contrôleur se déconnecte
- Mise à jour possible en SSH

### Performances réseau

| Transport | Bande passante | Latence |
|-----------|---------------|---------|
| PCM 16/44.1 stéréo | 1.41 Mbps | - |
| PCM 24/192 stéréo | 9.22 Mbps | - |
| PCM 32/384 stéréo | 24.58 Mbps | - |
| FLAC 24/192 stéréo | ~4 Mbps | +5 ms décompression |
| Ethernet 100 Mbps (RPi 3) | OK pour tout | < 1 ms |
| WiFi 5 GHz | OK pour tout | ~50 ms clock offset |

> **Recommandation** : Ethernet pour le multi-room synchronisé. WiFi pour un endpoint solo, c'est très bien.

---

## Liens

- [Code source OAAT](https://github.com/renesenses/oaat) (Apache 2.0)
- [Spécification RFC](https://mozaiklabs.fr/docs/oaat)
- [Tune — serveur de musique](https://mozaiklabs.fr)
- [Forum MozAIk Labs](https://mozaiklabs.fr/forum)
- [Audiophonics](https://www.audiophonics.fr) — DAC I2S et boîtiers RPi

---

*Guide rédigé par MozAIk Labs — dernière mise à jour : mai 2026*
