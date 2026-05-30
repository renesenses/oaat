#!/usr/bin/env python3
"""Tune Now Playing display for Raspberry Pi framebuffer (800x480 DSI touchscreen).

Polls the Tune server API and renders album art, track info, progress bar,
and endpoint status directly to /dev/fb0. No X11 required.
"""

import io
import json
import os
import signal
import struct
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# ── Config ────────────────────────────────────────────────────────────────────

TUNE_SERVER = os.environ.get("TUNE_SERVER", "http://192.168.1.15:8888")
ZONE_NAME = os.environ.get("TUNE_ZONE", "")  # empty = auto-detect OAAT or first playing
POLL_INTERVAL = float(os.environ.get("POLL_INTERVAL", "2"))
FB_DEVICE = os.environ.get("FB_DEVICE", "/dev/fb0")

# ── Display constants ─────────────────────────────────────────────────────────

WIDTH, HEIGHT = 800, 480
BPP = 16  # RGB565

# Colors (RGB)
BG = (12, 12, 20)
TEXT_PRIMARY = (255, 255, 255)
TEXT_SECONDARY = (160, 160, 175)
TEXT_DIM = (100, 100, 115)
ACCENT = (34, 211, 238)  # cyan
ACCENT_GREEN = (80, 200, 120)
PROGRESS_BG = (40, 40, 55)
PROGRESS_FG = ACCENT
VOLUME_FG = ACCENT_GREEN
DIVIDER = (40, 40, 55)

# Layout
ART_SIZE = 320
ART_X, ART_Y = 40, 50
INFO_X = ART_X + ART_SIZE + 40
INFO_Y = 70
PROGRESS_Y = 410
PROGRESS_H = 6
PROGRESS_X = 40
PROGRESS_W = WIDTH - 80
STATUS_Y = 445

# ── Fonts ─────────────────────────────────────────────────────────────────────

FONT_DIR = "/usr/share/fonts/truetype/dejavu"

def load_fonts():
    try:
        return {
            "title": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans-Bold.ttf", 28),
            "artist": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans.ttf", 22),
            "album": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans.ttf", 18),
            "format": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans.ttf", 15),
            "time": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans.ttf", 14),
            "status": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans-Bold.ttf", 13),
            "big": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans-Bold.ttf", 48),
            "logo": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans-Bold.ttf", 16),
        }
    except OSError:
        return {k: ImageFont.load_default() for k in
                ["title", "artist", "album", "format", "time", "status", "big", "logo"]}

# ── Framebuffer ───────────────────────────────────────────────────────────────

def rgb_to_565(r, g, b):
    return ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)

def image_to_fb(img):
    """Convert PIL RGB image to RGB565 bytes for framebuffer."""
    pixels = img.tobytes()
    fb_data = bytearray(WIDTH * HEIGHT * 2)
    for i in range(0, len(pixels), 3):
        px = i // 3
        val = rgb_to_565(pixels[i], pixels[i+1], pixels[i+2])
        fb_data[px*2] = val & 0xFF
        fb_data[px*2+1] = (val >> 8) & 0xFF
    return bytes(fb_data)

def write_fb(data):
    with open(FB_DEVICE, "wb") as fb:
        fb.write(data)

# ── API ───────────────────────────────────────────────────────────────────────

def fetch_zones():
    try:
        req = urllib.request.Request(f"{TUNE_SERVER}/api/zones", headers={"Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=5) as resp:
            return json.loads(resp.read())
    except Exception:
        return None

def fetch_cover(cover_path):
    if not cover_path:
        return None
    try:
        url = f"{TUNE_SERVER}/{cover_path}"
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=5) as resp:
            return Image.open(io.BytesIO(resp.read())).convert("RGB")
    except Exception:
        return None

def find_zone(zones):
    """Find the best zone to display: OAAT > playing > first."""
    if not zones:
        return None

    # Prefer zone matching TUNE_ZONE env
    if ZONE_NAME:
        for z in zones:
            if ZONE_NAME.lower() in z.get("name", "").lower():
                return z

    # Prefer OAAT endpoint
    for z in zones:
        if z.get("output_type") == "oaat":
            return z

    # Prefer currently playing
    for z in zones:
        if z.get("state") == "playing" and z.get("current_track"):
            return z

    # First with a track
    for z in zones:
        if z.get("current_track"):
            return z

    return zones[0] if zones else None

# ── Rendering ─────────────────────────────────────────────────────────────────

def format_time(ms):
    if not ms or ms <= 0:
        return "--:--"
    s = ms // 1000
    return f"{s // 60}:{s % 60:02d}"

def truncate_text(draw, text, font, max_width):
    if not text:
        return ""
    bbox = draw.textbbox((0, 0), text, font=font)
    if bbox[2] - bbox[0] <= max_width:
        return text
    while len(text) > 3:
        text = text[:-1]
        bbox = draw.textbbox((0, 0), text + "...", font=font)
        if bbox[2] - bbox[0] <= max_width:
            return text + "..."
    return text

def render_idle(draw, fonts):
    """Render idle/no connection screen."""
    draw.text((WIDTH // 2, HEIGHT // 2 - 40), "TUNE", font=fonts["big"],
              fill=ACCENT, anchor="mm")
    draw.text((WIDTH // 2, HEIGHT // 2 + 20), "En attente de lecture...",
              font=fonts["artist"], fill=TEXT_DIM, anchor="mm")
    draw.text((WIDTH // 2, HEIGHT - 30), f"Serveur: {TUNE_SERVER}",
              font=fonts["time"], fill=TEXT_DIM, anchor="mm")

def render_no_connection(draw, fonts):
    """Render connection error screen."""
    draw.text((WIDTH // 2, HEIGHT // 2 - 40), "TUNE", font=fonts["big"],
              fill=(180, 60, 60), anchor="mm")
    draw.text((WIDTH // 2, HEIGHT // 2 + 20), "Connexion au serveur...",
              font=fonts["artist"], fill=TEXT_DIM, anchor="mm")
    draw.text((WIDTH // 2, HEIGHT - 30), TUNE_SERVER,
              font=fonts["time"], fill=TEXT_DIM, anchor="mm")

def render_now_playing(img, draw, fonts, zone, cover_img):
    """Render Now Playing screen."""
    track = zone.get("current_track", {})
    if not track:
        render_idle(draw, fonts)
        return

    title = track.get("title", "")
    artist = track.get("artist_name", "")
    album = track.get("album_title", "")
    duration_ms = track.get("duration_ms", 0)
    position_ms = zone.get("position_ms", 0)
    state = zone.get("state", "stopped")
    volume = zone.get("volume", 0.5)
    output_type = zone.get("output_type", "")
    zone_name = zone.get("name", "")

    max_text_w = WIDTH - INFO_X - 40

    # ── Album art ──
    if cover_img:
        art = cover_img.resize((ART_SIZE, ART_SIZE), Image.LANCZOS)
        # Rounded corner mask
        mask = Image.new("L", (ART_SIZE, ART_SIZE), 0)
        mask_draw = ImageDraw.Draw(mask)
        mask_draw.rounded_rectangle([(0, 0), (ART_SIZE-1, ART_SIZE-1)], radius=16, fill=255)
        img.paste(art, (ART_X, ART_Y), mask)
    else:
        draw.rounded_rectangle(
            [(ART_X, ART_Y), (ART_X + ART_SIZE, ART_Y + ART_SIZE)],
            radius=16, fill=(30, 30, 45)
        )
        draw.text((ART_X + ART_SIZE // 2, ART_Y + ART_SIZE // 2), "♪",
                  font=fonts["big"], fill=TEXT_DIM, anchor="mm")

    # ── Track info ──
    y = INFO_Y

    # Title
    t = truncate_text(draw, title, fonts["title"], max_text_w)
    draw.text((INFO_X, y), t, font=fonts["title"], fill=TEXT_PRIMARY)
    y += 40

    # Artist
    t = truncate_text(draw, artist, fonts["artist"], max_text_w)
    draw.text((INFO_X, y), t, font=fonts["artist"], fill=ACCENT)
    y += 32

    # Album
    t = truncate_text(draw, album, fonts["album"], max_text_w)
    draw.text((INFO_X, y), t, font=fonts["album"], fill=TEXT_SECONDARY)
    y += 35

    # Divider
    draw.line([(INFO_X, y), (INFO_X + max_text_w, y)], fill=DIVIDER, width=1)
    y += 15

    # Source & format info
    source = track.get("source", "")
    source_label = source.upper() if source else ""
    if output_type:
        source_label += f"  ·  {output_type.upper()}"
    draw.text((INFO_X, y), source_label, font=fonts["format"], fill=TEXT_DIM)
    y += 22

    # Zone name
    zn = truncate_text(draw, zone_name, fonts["format"], max_text_w)
    draw.text((INFO_X, y), zn, font=fonts["format"], fill=TEXT_DIM)

    # ── State icon ──
    state_x = INFO_X + max_text_w
    if state == "playing":
        draw.text((state_x, INFO_Y), "▶", font=fonts["artist"], fill=ACCENT_GREEN, anchor="ra")
    elif state == "paused":
        draw.text((state_x, INFO_Y), "❚❚", font=fonts["format"], fill=TEXT_DIM, anchor="ra")

    # ── Progress bar ──
    draw.rounded_rectangle(
        [(PROGRESS_X, PROGRESS_Y), (PROGRESS_X + PROGRESS_W, PROGRESS_Y + PROGRESS_H)],
        radius=3, fill=PROGRESS_BG
    )

    if duration_ms and duration_ms > 0:
        progress = min(position_ms / duration_ms, 1.0)
        bar_w = int(PROGRESS_W * progress)
        if bar_w > 0:
            draw.rounded_rectangle(
                [(PROGRESS_X, PROGRESS_Y), (PROGRESS_X + bar_w, PROGRESS_Y + PROGRESS_H)],
                radius=3, fill=PROGRESS_FG
            )
        # Playhead dot
        dot_x = PROGRESS_X + bar_w
        draw.ellipse([(dot_x - 5, PROGRESS_Y - 2), (dot_x + 5, PROGRESS_Y + PROGRESS_H + 2)],
                     fill=PROGRESS_FG)

    # Time
    elapsed = format_time(position_ms)
    total = format_time(duration_ms) if duration_ms else ""
    time_str = f"{elapsed} / {total}" if total else elapsed
    draw.text((PROGRESS_X, PROGRESS_Y - 18), time_str, font=fonts["time"], fill=TEXT_SECONDARY)

    # ── Bottom status bar ──
    # Volume
    vol_pct = int(volume * 100)
    vol_str = f"Vol {vol_pct}%"
    draw.text((PROGRESS_X, STATUS_Y), vol_str, font=fonts["status"], fill=TEXT_DIM)

    # Volume bar
    vol_bar_x = PROGRESS_X + 80
    vol_bar_w = 120
    draw.rounded_rectangle(
        [(vol_bar_x, STATUS_Y + 3), (vol_bar_x + vol_bar_w, STATUS_Y + 9)],
        radius=2, fill=PROGRESS_BG
    )
    v_w = int(vol_bar_w * volume)
    if v_w > 0:
        draw.rounded_rectangle(
            [(vol_bar_x, STATUS_Y + 3), (vol_bar_x + v_w, STATUS_Y + 9)],
            radius=2, fill=VOLUME_FG
        )

    # Tune logo / branding
    draw.text((WIDTH - 40, STATUS_Y), "TUNE", font=fonts["logo"], fill=ACCENT, anchor="ra")

    # Online indicator
    online = zone.get("online", False)
    dot_color = ACCENT_GREEN if online else (180, 60, 60)
    draw.ellipse([(WIDTH - 26, STATUS_Y + 1), (WIDTH - 18, STATUS_Y + 9)], fill=dot_color)

# ── Main loop ─────────────────────────────────────────────────────────────────

def main():
    fonts = load_fonts()
    last_cover_path = None
    cover_img = None
    running = True

    def handle_signal(sig, frame):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGINT, handle_signal)

    print(f"Tune Display starting — server={TUNE_SERVER} fb={FB_DEVICE} {WIDTH}x{HEIGHT}")

    while running:
        img = Image.new("RGB", (WIDTH, HEIGHT), BG)
        draw = ImageDraw.Draw(img)

        zones = fetch_zones()

        if zones is None:
            render_no_connection(draw, fonts)
        else:
            zone = find_zone(zones)
            if zone and zone.get("current_track"):
                track = zone["current_track"]
                cp = track.get("cover_path")
                if cp != last_cover_path:
                    cover_img = fetch_cover(cp)
                    last_cover_path = cp
                render_now_playing(img, draw, fonts, zone, cover_img)
            else:
                render_idle(draw, fonts)

        try:
            fb_data = image_to_fb(img)
            write_fb(fb_data)
        except PermissionError:
            print("Permission denied on framebuffer. Run with sudo or add user to 'video' group.")
            sys.exit(1)
        except Exception as e:
            print(f"FB write error: {e}")

        time.sleep(POLL_INTERVAL)

    # Clear screen on exit
    img = Image.new("RGB", (WIDTH, HEIGHT), (0, 0, 0))
    try:
        write_fb(image_to_fb(img))
    except Exception:
        pass
    print("Tune Display stopped.")

if __name__ == "__main__":
    main()
