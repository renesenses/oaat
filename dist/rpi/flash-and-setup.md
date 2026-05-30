# Flash Raspberry Pi 4 pour OAAT + Audiophonics ESS 9038

## 1. Télécharger Raspberry Pi Imager

Si pas déjà installé :
```bash
brew install --cask raspberry-pi-imager
```

## 2. Flasher la carte SD

1. Ouvrir **Raspberry Pi Imager**
2. Choisir l'OS : **Raspberry Pi OS Lite (64-bit)** (pas Desktop, on n'a pas besoin de GUI)
3. Choisir la carte SD
4. **Cliquer sur l'engrenage** (settings) avant de flasher :
   - Hostname : `oaat-endpoint`
   - Enable SSH : **oui**, mot de passe
   - Username : `bertrand`
   - Password : `FSJhdbcy`
   - WiFi : configurer si pas en Ethernet
   - Locale : Europe/Paris, clavier FR
5. Flasher

## 3. Premier boot

1. Insérer la SD dans le Pi
2. Brancher Ethernet + alimentation
3. Attendre 1-2 minutes
4. Depuis le Mac :

```bash
ssh bertrand@oaat-endpoint.local
# ou
ssh bertrand@192.168.1.42
```

## 4. Installer OAAT

```bash
curl -sL https://raw.githubusercontent.com/renesenses/oaat/main/dist/rpi/setup.sh | sudo bash
```

## 5. Reboot (pour activer le DAC)

```bash
sudo reboot
```

## 6. Vérifier

```bash
# Depuis le Pi
aplay -l                    # Le DAC doit apparaître
speaker-test -D hw:0 -c 2  # Test son direct

# Depuis le Mac
oaat-test 192.168.1.42:9740         # 20 tests de conformité
oaat controller --target 192.168.1.42:9740 --freq 440 --duration 5  # Sinusoïde !
```
