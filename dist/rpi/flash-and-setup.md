# Flash Raspberry Pi pour OAAT

Guide rapide pour préparer une carte SD depuis zéro.

## 1. Installer Raspberry Pi Imager

```bash
# macOS
brew install --cask raspberry-pi-imager

# Linux
sudo apt install rpi-imager
```

Ou télécharger depuis https://www.raspberrypi.com/software/

## 2. Flasher la carte SD

1. Ouvrir **Raspberry Pi Imager**
2. Choisir l'appareil : **Raspberry Pi 3** ou **Raspberry Pi 4**
3. Choisir l'OS : `Raspberry Pi OS (other)` → **Raspberry Pi OS Lite (64-bit)**
4. Choisir la carte SD
5. Cliquer sur l'engrenage (paramètres) :
   - Hostname : `oaat-endpoint`
   - Activer SSH : oui, avec mot de passe
   - Nom d'utilisateur / mot de passe : à votre choix
   - WiFi : configurer si pas d'Ethernet
   - Fuseau horaire : `Europe/Paris`
6. Flasher

## 3. Premier boot

1. Insérer la SD dans le Pi
2. Brancher le DAC HAT sur le GPIO
3. Brancher Ethernet + alimentation
4. Attendre 1-2 minutes

```bash
ssh <user>@oaat-endpoint.local
```

## 4. Installer OAAT

```bash
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

## 5. Reboot

```bash
sudo reboot
```

## 6. Vérifier

```bash
# Sur le Pi
aplay -l
speaker-test -D hw:0,0 -c 2 -t sine -f 440

# Depuis un autre appareil
oaat-test <ip-du-pi>:9740
oaat controller --target <ip-du-pi>:9740 --freq 440 --duration 5
```

## Guide complet

Voir [docs/howto-rpi-endpoint.md](../../docs/howto-rpi-endpoint.md) pour le guide détaillé avec configuration, troubleshooting et multi-room.
