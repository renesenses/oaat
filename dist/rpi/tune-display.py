#!/usr/bin/env python3
"""Tune Now Playing display for Raspberry Pi framebuffer (800x480 DSI touchscreen).

Polls the Tune server API and renders album art, track info, progress bar,
and endpoint status directly to /dev/fb0. No X11 required.

Touch: tap bottom bar to open zone picker, tap a zone to select it.
"""

import io
import json
import os
import select
import signal
import struct
import sys
import threading
import time
import urllib.error
import urllib.request

from PIL import Image, ImageDraw, ImageFont

# ── Config ────────────────────────────────────────────────────────────────────

TUNE_SERVER = os.environ.get("TUNE_SERVER", "http://192.168.1.15:8888/api/v1")
POLL_INTERVAL = float(os.environ.get("POLL_INTERVAL", "2"))
FB_DEVICE = os.environ.get("FB_DEVICE", "/dev/fb0")
TOUCH_DEVICE = os.environ.get("TOUCH_DEVICE", "/dev/input/event0")

# ── Display constants ─────────────────────────────────────────────────────────

WIDTH, HEIGHT = 800, 480

# Colors (RGB)
BG = (12, 12, 20)
BG_OVERLAY = (20, 20, 35)
TEXT_PRIMARY = (255, 255, 255)
TEXT_SECONDARY = (160, 160, 175)
TEXT_DIM = (100, 100, 115)
ACCENT = (34, 211, 238)
ACCENT_GREEN = (80, 200, 120)
PROGRESS_BG = (40, 40, 55)
PROGRESS_FG = ACCENT
VOLUME_FG = ACCENT_GREEN
DIVIDER = (40, 40, 55)
ZONE_SELECTED = (34, 211, 238, 40)
ZONE_HOVER = (255, 255, 255, 10)

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
ZONE_BAR_Y = 430
ZONE_BAR_H = 50

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
            "zone_name": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans-Bold.ttf", 18),
            "zone_detail": ImageFont.truetype(f"{FONT_DIR}/DejaVuSans.ttf", 13),
        }
    except OSError:
        return {k: ImageFont.load_default() for k in
                ["title", "artist", "album", "format", "time", "status",
                 "big", "logo", "zone_name", "zone_detail"]}

# ── Framebuffer ───────────────────────────────────────────────────────────────

def rgb_to_565(r, g, b):
    return ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)

def image_to_fb(img):
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

# ── Touch input ───────────────────────────────────────────────────────────────

# evdev event struct: time(16 bytes) + type(2) + code(2) + value(4) = 24 bytes
EVENT_SIZE = 24
EV_ABS = 3
EV_KEY = 1
ABS_X = 0
ABS_Y = 1
BTN_TOUCH = 330

class TouchReader:
    def __init__(self, device_path):
        self.device_path = device_path
        self.fd = None
        self.touch_x = 0
        self.touch_y = 0
        self.taps = []  # list of (x, y) taps
        self.lock = threading.Lock()
        self._running = True

    def start(self):
        try:
            self.fd = os.open(self.device_path, os.O_RDONLY | os.O_NONBLOCK)
            t = threading.Thread(target=self._read_loop, daemon=True)
            t.start()
            return True
        except OSError as e:
            print(f"Touch device {self.device_path}: {e}")
            return False

    def stop(self):
        self._running = False
        if self.fd is not None:
            os.close(self.fd)

    def get_taps(self):
        with self.lock:
            taps = self.taps[:]
            self.taps.clear()
            return taps

    def _read_loop(self):
        touching = False
        while self._running:
            try:
                r, _, _ = select.select([self.fd], [], [], 0.1)
                if not r:
                    continue
                data = os.read(self.fd, EVENT_SIZE * 16)
                for offset in range(0, len(data) - EVENT_SIZE + 1, EVENT_SIZE):
                    ev = data[offset:offset + EVENT_SIZE]
                    _, _, ev_type, ev_code, ev_value = struct.unpack("llHHi", ev)
                    if ev_type == EV_ABS and ev_code == ABS_X:
                        self.touch_x = ev_value
                    elif ev_type == EV_ABS and ev_code == ABS_Y:
                        self.touch_y = ev_value
                    elif ev_type == EV_KEY and ev_code == BTN_TOUCH:
                        if ev_value == 1:
                            touching = True
                        elif ev_value == 0 and touching:
                            touching = False
                            # Map touch coords to screen (ft5x06 range is 0-800, 0-480)
                            sx = int(self.touch_x * WIDTH / 800)
                            sy = int(self.touch_y * HEIGHT / 480)
                            with self.lock:
                                self.taps.append((sx, sy))
            except OSError:
                time.sleep(0.5)

# ── API ───────────────────────────────────────────────────────────────────────

def fetch_zones():
    try:
        req = urllib.request.Request(f"{TUNE_SERVER}/zones", headers={"Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=5) as resp:
            data = json.loads(resp.read())
            if isinstance(data, dict) and "zones" in data:
                return data["zones"]
            if isinstance(data, list):
                return data
            return None
    except Exception:
        return None

def fetch_cover(cover_path):
    if not cover_path:
        return None
    try:
        base = TUNE_SERVER.replace("/api/v1", "")
        url = f"{base}/{cover_path}"
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req, timeout=5) as resp:
            return Image.open(io.BytesIO(resp.read())).convert("RGB")
    except Exception:
        return None

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
    draw.text((WIDTH // 2, HEIGHT // 2 - 40), "TUNE", font=fonts["big"],
              fill=ACCENT, anchor="mm")
    draw.text((WIDTH // 2, HEIGHT // 2 + 20), "En attente de lecture...",
              font=fonts["artist"], fill=TEXT_DIM, anchor="mm")

def render_no_connection(draw, fonts):
    draw.text((WIDTH // 2, HEIGHT // 2 - 40), "TUNE", font=fonts["big"],
              fill=(180, 60, 60), anchor="mm")
    draw.text((WIDTH // 2, HEIGHT // 2 + 20), "Connexion au serveur...",
              font=fonts["artist"], fill=TEXT_DIM, anchor="mm")

def render_now_playing(img, draw, fonts, zone, cover_img):
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

    # Album art
    if cover_img:
        art = cover_img.resize((ART_SIZE, ART_SIZE), Image.LANCZOS)
        mask = Image.new("L", (ART_SIZE, ART_SIZE), 0)
        mask_draw = ImageDraw.Draw(mask)
        mask_draw.rounded_rectangle([(0, 0), (ART_SIZE-1, ART_SIZE-1)], radius=16, fill=255)
        img.paste(art, (ART_X, ART_Y), mask)
    else:
        draw.rounded_rectangle(
            [(ART_X, ART_Y), (ART_X + ART_SIZE, ART_Y + ART_SIZE)],
            radius=16, fill=(30, 30, 45))

    # Track info
    y = INFO_Y
    draw.text((INFO_X, y), truncate_text(draw, title, fonts["title"], max_text_w),
              font=fonts["title"], fill=TEXT_PRIMARY)
    y += 40
    draw.text((INFO_X, y), truncate_text(draw, artist, fonts["artist"], max_text_w),
              font=fonts["artist"], fill=ACCENT)
    y += 32
    draw.text((INFO_X, y), truncate_text(draw, album, fonts["album"], max_text_w),
              font=fonts["album"], fill=TEXT_SECONDARY)
    y += 35

    draw.line([(INFO_X, y), (INFO_X + max_text_w, y)], fill=DIVIDER, width=1)
    y += 15

    source = track.get("source", "")
    source_label = source.upper() if source else ""
    if output_type:
        source_label += f"  ·  {output_type.upper()}"
    draw.text((INFO_X, y), source_label, font=fonts["format"], fill=TEXT_DIM)

    # State icon
    state_x = INFO_X + max_text_w
    if state == "playing":
        draw.text((state_x, INFO_Y), "▶", font=fonts["artist"], fill=ACCENT_GREEN, anchor="ra")
    elif state == "paused":
        draw.text((state_x, INFO_Y), "❚❚", font=fonts["format"], fill=TEXT_DIM, anchor="ra")

    # Progress bar
    draw.rounded_rectangle(
        [(PROGRESS_X, PROGRESS_Y), (PROGRESS_X + PROGRESS_W, PROGRESS_Y + PROGRESS_H)],
        radius=3, fill=PROGRESS_BG)

    if duration_ms and duration_ms > 0:
        progress = min(position_ms / duration_ms, 1.0)
        bar_w = int(PROGRESS_W * progress)
        if bar_w > 0:
            draw.rounded_rectangle(
                [(PROGRESS_X, PROGRESS_Y), (PROGRESS_X + bar_w, PROGRESS_Y + PROGRESS_H)],
                radius=3, fill=PROGRESS_FG)
        dot_x = PROGRESS_X + bar_w
        draw.ellipse([(dot_x - 5, PROGRESS_Y - 2), (dot_x + 5, PROGRESS_Y + PROGRESS_H + 2)],
                     fill=PROGRESS_FG)

    # Time
    elapsed = format_time(position_ms)
    total = format_time(duration_ms) if duration_ms else ""
    time_str = f"{elapsed} / {total}" if total else elapsed
    draw.text((PROGRESS_X, PROGRESS_Y - 18), time_str, font=fonts["time"], fill=TEXT_SECONDARY)

    # Bottom bar — zone name (tappable)
    draw.rounded_rectangle(
        [(PROGRESS_X, ZONE_BAR_Y), (WIDTH - PROGRESS_X, ZONE_BAR_Y + ZONE_BAR_H)],
        radius=8, fill=(25, 25, 40))

    # Zone name + chevron
    zn = truncate_text(draw, zone_name, fonts["zone_name"], PROGRESS_W - 100)
    draw.text((PROGRESS_X + 16, ZONE_BAR_Y + 8), zn, font=fonts["zone_name"], fill=TEXT_PRIMARY)

    # Volume
    vol_pct = int(volume * 100)
    draw.text((PROGRESS_X + 16, ZONE_BAR_Y + 30), f"Vol {vol_pct}%", font=fonts["zone_detail"], fill=TEXT_DIM)

    # Online dot + TUNE label
    online = zone.get("online", False)
    dot_color = ACCENT_GREEN if online else (180, 60, 60)
    draw.text((WIDTH - PROGRESS_X - 16, ZONE_BAR_Y + 10), "TUNE ▼", font=fonts["logo"],
              fill=ACCENT, anchor="ra")
    draw.ellipse([(WIDTH - PROGRESS_X - 16, ZONE_BAR_Y + 32), (WIDTH - PROGRESS_X - 8, ZONE_BAR_Y + 40)],
                 fill=dot_color)
    draw.text((WIDTH - PROGRESS_X - 22, ZONE_BAR_Y + 30), output_type.upper() if output_type else "",
              font=fonts["zone_detail"], fill=TEXT_DIM, anchor="ra")


def render_zone_picker(img, draw, fonts, zones, selected_zone_id):
    """Render fullscreen zone picker overlay."""
    draw.rectangle([(0, 0), (WIDTH, HEIGHT)], fill=BG)

    # Header
    draw.text((WIDTH // 2, 30), "Choisir une zone", font=fonts["title"], fill=TEXT_PRIMARY, anchor="mm")
    draw.line([(40, 55), (WIDTH - 40, 55)], fill=DIVIDER, width=1)

    # Zone list
    row_h = 52
    start_y = 70
    zone_rects = []

    for i, zone in enumerate(zones):
        y = start_y + i * row_h
        if y + row_h > HEIGHT - 10:
            break

        is_selected = zone.get("id") == selected_zone_id
        state = zone.get("state", "stopped")
        online = zone.get("online", False)
        name = zone.get("name", "?")
        track = zone.get("current_track")
        output_type = zone.get("output_type", "")

        # Row background
        if is_selected:
            draw.rounded_rectangle([(30, y), (WIDTH - 30, y + row_h - 4)], radius=8, fill=(34, 211, 238, 30))
            draw.rounded_rectangle([(30, y), (WIDTH - 30, y + row_h - 4)], radius=8, outline=ACCENT, width=1)
        else:
            draw.rounded_rectangle([(30, y), (WIDTH - 30, y + row_h - 4)], radius=8, fill=(30, 30, 45))

        # Online dot
        dot_color = ACCENT_GREEN if online else (80, 40, 40)
        if state == "playing":
            dot_color = ACCENT_GREEN
        elif state == "paused":
            dot_color = (200, 180, 50)
        draw.ellipse([(46, y + 12), (56, y + 22)], fill=dot_color)

        # Zone name
        zn = truncate_text(draw, name, fonts["zone_name"], 400)
        draw.text((68, y + 6), zn, font=fonts["zone_name"], fill=TEXT_PRIMARY if online else TEXT_DIM)

        # Track info
        if track:
            track_str = f"{track.get('artist_name', '')} — {track.get('title', '')}"
            ts = truncate_text(draw, track_str, fonts["zone_detail"], 400)
            draw.text((68, y + 28), ts, font=fonts["zone_detail"], fill=TEXT_SECONDARY)
        else:
            draw.text((68, y + 28), "—", font=fonts["zone_detail"], fill=TEXT_DIM)

        # Output type badge
        if output_type:
            badge = output_type.upper()
            draw.text((WIDTH - 50, y + 12), badge, font=fonts["zone_detail"], fill=TEXT_DIM, anchor="ra")

        # State icon
        if state == "playing":
            draw.text((WIDTH - 50, y + 28), "▶", font=fonts["zone_detail"], fill=ACCENT_GREEN, anchor="ra")
        elif state == "paused":
            draw.text((WIDTH - 50, y + 28), "❚❚", font=fonts["zone_detail"], fill=TEXT_DIM, anchor="ra")

        zone_rects.append((y, y + row_h - 4, zone))

    return zone_rects

# ── Main loop ─────────────────────────────────────────────────────────────────

def main():
    fonts = load_fonts()
    last_cover_path = None
    cover_img = None
    running = True
    selected_zone_id = None
    show_picker = False
    zone_rects = []
    all_zones = []

    def handle_signal(sig, frame):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, handle_signal)
    signal.signal(signal.SIGINT, handle_signal)

    # Start touch reader
    touch = TouchReader(TOUCH_DEVICE)
    touch_ok = touch.start()
    if touch_ok:
        print(f"Touch input: {TOUCH_DEVICE}")
    else:
        print("No touch input available, display-only mode")

    print(f"Tune Display starting — server={TUNE_SERVER} fb={FB_DEVICE} {WIDTH}x{HEIGHT}")

    while running:
        # Handle touch events
        if touch_ok:
            for tx, ty in touch.get_taps():
                if show_picker:
                    # Check if tap hits a zone row
                    hit = False
                    for row_top, row_bot, zone in zone_rects:
                        if row_top <= ty <= row_bot:
                            selected_zone_id = zone.get("id")
                            show_picker = False
                            last_cover_path = None
                            cover_img = None
                            hit = True
                            break
                    if not hit:
                        show_picker = False
                else:
                    # Tap on bottom zone bar → open picker
                    if ty >= ZONE_BAR_Y:
                        show_picker = True

        img = Image.new("RGB", (WIDTH, HEIGHT), BG)
        draw = ImageDraw.Draw(img)

        zones = fetch_zones()
        if zones is not None:
            all_zones = zones

        if show_picker and all_zones:
            zone_rects = render_zone_picker(img, draw, fonts, all_zones, selected_zone_id)
        elif zones is None:
            render_no_connection(draw, fonts)
        else:
            # Find zone
            zone = None
            if selected_zone_id is not None:
                for z in zones:
                    if z.get("id") == selected_zone_id:
                        zone = z
                        break
            if zone is None:
                # Auto: OAAT > playing > first with track
                for z in zones:
                    if z.get("output_type") == "oaat":
                        zone = z
                        break
                if zone is None:
                    for z in zones:
                        if z.get("state") == "playing" and z.get("current_track"):
                            zone = z
                            break
                if zone is None:
                    for z in zones:
                        if z.get("current_track"):
                            zone = z
                            break
                if zone is None and zones:
                    zone = zones[0]

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
            write_fb(image_to_fb(img))
        except PermissionError:
            print("Permission denied on framebuffer. Run with sudo or add user to 'video' group.")
            sys.exit(1)
        except Exception as e:
            print(f"FB write error: {e}")

        time.sleep(POLL_INTERVAL)

    touch.stop()
    img = Image.new("RGB", (WIDTH, HEIGHT), (0, 0, 0))
    try:
        write_fb(image_to_fb(img))
    except Exception:
        pass
    print("Tune Display stopped.")

if __name__ == "__main__":
    main()
