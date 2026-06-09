#!/usr/bin/env python3
"""
ft_lock.py — Fullscreen lock screen for Ubuntu
Usage: python3 ft_lock.py [--image /path/to/image.png] [--user etaquet] [--message "Back sOOn.."]
Unlocks with the current user's system password (PAM) or a fallback env var FT_LOCK_PASSWORD.
"""

import tkinter as tk
from tkinter import font as tkfont
import argparse
import os
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
import json
import urllib.request
import urllib.parse

# ── PAM authentication (requires python3-pam) ──────────────────────────────
try:
    import pam  # type: ignore[import-not-found]
    PAM_AVAILABLE = True
except ImportError:
    PAM_AVAILABLE = False

def authenticate(username: str, password: str) -> bool:
    """Return True if the password is correct for the current user."""
    # 1. Try PAM (real system password)
    if PAM_AVAILABLE:
        p = pam.pam()
        return p.authenticate(username, password)
    # 2. Fallback: compare against FT_LOCK_PASSWORD env var (dev/testing only)
    fallback = os.environ.get("FT_LOCK_PASSWORD")
    if fallback:
        return password == fallback
    # 3. No auth method available — warn and unlock anyway so you're not stuck
    print("WARNING: python3-pam not installed and FT_LOCK_PASSWORD not set. "
          "Install it with:  sudo apt install python3-pam", file=sys.stderr)
    return True


def load_config() -> dict[str, str]:
    """Load key=value configuration.
    Priority:
      1) ~/.config/ft_lock/ft_lock.cfg
      3) ./ft_lock/ft_lock.cfg
    """
    config: dict[str, str] = {}
    candidates = [
        Path.home() / ".config" / "ft_lock" / "ft_lock.cfg",
        Path(__file__).parent / "ft_lock" / "ft_lock.cfg",
    ]

    for path in candidates:
        try:
            if path.is_dir():
                path = path / "ft_lock.cfg"
            if not path.is_file():
                continue

            for raw_line in path.read_text(encoding="utf-8").splitlines():
                line = raw_line.strip()
                if not line or line.startswith("#"):
                    continue
                if "=" not in line:
                    continue
                key, value = line.split("=", 1)
                key = key.strip()
                value = value.strip().strip('"').strip("'")
                config[key] = value
            break
        except Exception:
            continue

    return config


# ── Main lock screen ────────────────────────────────────────────────────────
class LockScreen:
    SHAKE_DISTANCE = 14
    SHAKE_STEPS    = 8
    SHAKE_INTERVAL = 30   # ms per step
    IDLE_SLEEP_SECONDS = 5
    _BLOCKED_KEYSYMS = {
        "Escape",
        "F2",
        "F4",
        "Super_L",
        "Super_R",
        "Meta_L",
        "Meta_R",
        "Alt_L",
        "Alt_R",
    }

    def __init__(
        self,
        root: tk.Tk,
        image_path: str,
        username: str,
        message: str,
        password_text: str = "",
        accent_color: str = "#5a7ab0",
        timeout: int = 10,
        logout_timeout: int = 30,
        logout_timeout_force: int = 42,
    ):
        self.root     = root
        self.username = username
        self.password = ""
        self.locked   = True
        self.shaking  = False
        self.locked_at = time.time()
        self.password_text = password_text
        self.sleeping = False
        self.last_activity_at = time.time()
        self.dpms_supported = bool(shutil.which("xset")) and bool(os.environ.get("DISPLAY"))
        self.dpms_sleeping = False
        self.original_dpms_settings = None
        self.timeout = timeout
        self.IDLE_SLEEP_SECONDS = timeout
        self.logout_timeout = logout_timeout  # in minutes
        self.logout_timeout_force = logout_timeout_force  # in minutes

        # ── Window setup ──────────────────────────────────────────────────
        root.title("ft_lock")
        root.attributes("-fullscreen", True)
        root.attributes("-topmost", True)
        root.overrideredirect(True)          # no title bar / decorations

        # grab_set_global requires the window to be visible — schedule it
        # after the first expose event via update() + after()
        root.after(100, self._grab)

        sw = root.winfo_screenwidth()
        sh = root.winfo_screenheight()

        # ── Canvas fills the whole screen ─────────────────────────────────
        self.canvas = tk.Canvas(root, width=sw, height=sh,
                                bd=0, highlightthickness=0, bg="#1a0a1e")
        self.canvas.pack(fill="both", expand=True)

        self.sleep_overlay = self.canvas.create_rectangle(
            0, 0, sw, sh, fill="#000000", outline="", state="hidden", tags="sleep_overlay"
        )

        # ── Background image ──────────────────────────────────────────────
        self._bg_photo = None
        self._bg_frames = []
        self._bg_frame_index = 0
        self._bg_frame_durations = []
        self._bg_image_id = None

        if image_path and os.path.isfile(image_path):
            try:
                from PIL import Image, ImageTk, ImageSequence
                img = Image.open(image_path)
                is_animated = getattr(img, "is_animated", False) or getattr(img, "n_frames", 1) > 1

                if is_animated:
                    # Build frames scaled to the screen size
                    self._bg_frames = []
                    for frame in ImageSequence.Iterator(img):
                        try:
                            frm = frame.convert("RGBA").resize((sw, sh), Image.LANCZOS)
                        except Exception:
                            frm = frame.convert("RGBA")
                        self._bg_frames.append(ImageTk.PhotoImage(frm))

                    # Collect per-frame durations (ms)
                    try:
                        self._bg_frame_durations = []
                        img.seek(0)
                        n = getattr(img, "n_frames", len(self._bg_frames))
                        for i in range(n):
                            img.seek(i)
                            self._bg_frame_durations.append(img.info.get("duration", 100))
                    except Exception:
                        self._bg_frame_durations = [100] * len(self._bg_frames)

                    if self._bg_frames:
                        self._bg_frame_index = 0
                        self._bg_image_id = self.canvas.create_image(0, 0, anchor="nw", image=self._bg_frames[0])
                        self._animate_bg()
                else:
                    img = img.resize((sw, sh), Image.LANCZOS)
                    self._bg_photo = ImageTk.PhotoImage(img)
                    self.canvas.create_image(0, 0, anchor="nw", image=self._bg_photo)
            except ImportError:
                # Pillow not installed — try native tk (PNG/GIF static only)
                try:
                    self._bg_photo = tk.PhotoImage(file=image_path)
                    self.canvas.create_image(0, 0, anchor="nw", image=self._bg_photo)
                except Exception as e:
                    print(f"Could not load image: {e}", file=sys.stderr)

        # ── Fonts ─────────────────────────────────────────────────────────
        try:
            label_font  = tkfont.Font(family="Ubuntu", size=13)
            msg_font    = tkfont.Font(family="Ubuntu", size=11, slant="italic")
            input_font  = tkfont.Font(family="Ubuntu Mono", size=13)
        except Exception:
            label_font  = tkfont.Font(size=13)
            msg_font    = tkfont.Font(size=11, slant="italic")
            input_font  = tkfont.Font(size=13)
        self.input_font = input_font

        try:
            clock_font = tkfont.Font(family="Ubuntu Mono", size=16, weight="bold")
        except Exception:
            clock_font = tkfont.Font(size=16, weight="bold")
        self.clock_font = clock_font

        PAD_X, PAD_Y = 230, 40

        # ── Password entry box geometry ──────────────────────────────────
        BOX_X      = PAD_X
        BOX_Y      = PAD_Y + 52
        BOX_W      = 240
        BOX_H      = 32
        BOX_RADIUS = 10

        TEXT_COLOR  = "#c8d2e6"
        TEXT_DIM    = "#8a96aa"
        # expose colors for other methods
        self._text_color = TEXT_COLOR
        self._text_dim = TEXT_DIM

        top_center_x = BOX_X + (BOX_W // 2)

        self.locked_text = self.canvas.create_text(
            BOX_X + 150, BOX_Y - 30,
            text=f"Locked by {username} · a few seconds ago…",
            anchor="s", fill=TEXT_COLOR, font=label_font,
            tags="ui"
        )
        self.canvas.create_text(
            top_center_x, BOX_Y - 10,
            text=message,
            anchor="s", fill=TEXT_DIM, font=msg_font,
            tags="ui"
        )

        # ── Password entry box ────────────────────────────────────────────
        # We draw it on canvas; actual text is stored in self.password
        self._draw_rounded_rect(BOX_X + 2, BOX_Y + 2, BOX_X + BOX_W + 2, BOX_Y + BOX_H + 2,
            BOX_RADIUS, fill="#101725", outline="#101725",
            tags="input_shadow")

        self._draw_rounded_rect(BOX_X, BOX_Y, BOX_X + BOX_W, BOX_Y + BOX_H,
            BOX_RADIUS, fill="#1a2840", outline=accent_color, width=2,
                tags="input_box")

        self._draw_rounded_rect(BOX_X + 2, BOX_Y + 2, BOX_X + BOX_W - 2, BOX_Y + (BOX_H // 2) + 1,
            max(3, BOX_RADIUS - 3), fill="#274067", outline="",
            tags="input_gloss")

        # Bullet dots representing typed chars
        self.dot_text = self.canvas.create_text(
            BOX_X + 10, BOX_Y + BOX_H // 2,
            text="", anchor="w", fill=TEXT_COLOR, font=input_font,
            tags="dots"
        )

        # Blinking cursor
        self.cursor_visible = True
        self.cursor_line = self.canvas.create_line(
            BOX_X + 10, BOX_Y + 5, BOX_X + 10, BOX_Y + BOX_H - 5,
            fill=TEXT_COLOR, width=1, tags="cursor"
        )
        self._blink_cursor()

        # Error / status message below the box
        self.status_text = self.canvas.create_text(
            BOX_X, BOX_Y + BOX_H + 10,
            text="", anchor="nw", fill="#e05050", font=msg_font,
            tags="status"
        )

        # Logout warning message (centered, large, red)
        self.logout_warning = self.canvas.create_text(
            sw // 2, sh // 2,
            text="", anchor="center", fill="#ff4444", font=tkfont.Font(size=32, weight="bold"),
            tags="logout_warning", state="hidden"
        )

        # Clock (top-right) — shadow + colored text for a stylish look
        clock_x = sw - 20
        clock_y = 18
        self.clock_shadow = self.canvas.create_text(
            clock_x + 1, clock_y + 1,
            text="", anchor="ne", fill="#000000", font=self.clock_font, tags="clock"
        )
        self.clock_text = self.canvas.create_text(
            clock_x, clock_y,
            text="", anchor="ne", fill=accent_color, font=self.clock_font, tags="clock"
        )
        self._update_clock()

        # ── Weather icon / popup setup ──────────────────────────────────
        ICONS = {
            "01": "☀", "02": "⛅", "03": "☁", "04": "☁",
            "09": "🌧", "10": "🌦", "11": "⛈", "13": "❄",
            "50": "🌫",
        }
        def _weather_icon(code: str) -> str:
            return ICONS.get(code[:2], "🌡")
        self._weather_icon_func = _weather_icon

        icon_size = 46
        ix2 = sw - 18
        iy2 = sh - 18
        # circle background
        self.weather_bg = self.canvas.create_oval(
            ix2 - icon_size, iy2 - icon_size, ix2, iy2,
            fill=accent_color, outline="", tags="weather_icon"
        )
        # emoji inside
        try:
            emoji_font = tkfont.Font(family="Ubuntu Mono", size=18)
        except Exception:
            emoji_font = tkfont.Font(size=18)
        self.weather_emoji = self.canvas.create_text(
            ix2 - icon_size/2, iy2 - icon_size/2,
            text="☀", font=emoji_font, fill="#ffffff", tags="weather_icon"
        )
        # small temperature label next to icon
        try:
            temp_font = tkfont.Font(family="Ubuntu Mono", size=11, weight="bold")
        except Exception:
            temp_font = tkfont.Font(size=11, weight="bold")
        self.weather_temp_text = self.canvas.create_text(
            ix2 - icon_size - 6, iy2 - icon_size/2,
            text="--°", font=temp_font, fill="#ffffff", anchor="e", tags="weather_icon"
        )
        # bind click
        self.canvas.tag_bind("weather_icon", "<Button-1>", lambda e: self._toggle_weather_popup())
        # close popup when clicking elsewhere
        self.canvas.bind("<Button-1>", self._on_canvas_click)
        self.weather_popup_visible = False
        self.popup_id = None
        self.popup_day_index = 0
        self._weather_current = None
        self._weather_forecast = None
        self._weather_last_fetch_at = 0.0
        self._weather_cache_ttl = 300
        self._weather_fetch_in_flight = False

        # Store box geometry for shake animation
        self._box_origin_x = BOX_X
        self._box_y        = BOX_Y
        self._box_w        = BOX_W
        self._box_h        = BOX_H
        self._box_r        = BOX_RADIUS

        # ── Keyboard bindings ─────────────────────────────────────────────
        root.bind("<Key>",       self._on_key)
        root.bind("<Return>",    self._on_enter)
        root.bind("<BackSpace>", self._on_backspace)
        root.bind("<Motion>",    self._on_mouse_motion)
        root.bind("<Button>",    self._on_mouse_button)

        # Prevent common escape shortcuts as much as Tk allows.
        root.bind("<Alt-F4>",     lambda e: "break")
        root.bind("<Alt-F2>",     lambda e: "break")
        root.bind("<Escape>",     self._on_escape)
        root.protocol("WM_DELETE_WINDOW", lambda: None)
        root.bind("<FocusOut>", lambda e: self.root.after(10, self._restore_window))

        root.focus_force()
        self._enforce_window()
        self._update_locked_time()
        self._check_idle_sleep()
        self._check_logout_timeout()
        self.root.after(200, lambda: self._ensure_weather_data(force=True))

    def _grab(self):
        """Grab all input — must be called after the window is viewable."""
        try:
            self.root.grab_set_global()
        except tk.TclError:
            # Retry once more if the window manager hasn't mapped the window yet
            self.root.after(100, self.root.grab_set_global)

    def _restore_window(self):
        try:
            self.root.attributes("-fullscreen", True)
            self.root.attributes("-topmost", True)
            self.root.overrideredirect(True)
            self.root.lift()
            self.root.focus_force()
            self.root.grab_set_global()
        except tk.TclError:
            pass

    def _enforce_window(self):
        if self.locked:
            self._restore_window()
            self.root.after(250, self._enforce_window)

    # ── Helpers ─────────────────────────────────────────────────────────────

    def _draw_rounded_rect(self, x1, y1, x2, y2, r, **kwargs):
        tags = kwargs.pop("tags", "")
        self.canvas.create_polygon(
            x1+r, y1,  x2-r, y1,
            x2,   y1,  x2,   y1+r,
            x2,   y2-r, x2,  y2,
            x2-r, y2,  x1+r, y2,
            x1,   y2,  x1,   y2-r,
            x1,   y1+r, x1,  y1,
            smooth=True, tags=tags, **kwargs
        )

    def _animate_bg(self):
        """Advance animated GIF background frames (Pillow-backed)."""
        if not self._bg_frames or not self._bg_image_id:
            return
        # Advance index and update canvas image
        self._bg_frame_index = (self._bg_frame_index + 1) % len(self._bg_frames)
        try:
            self.canvas.itemconfig(self._bg_image_id, image=self._bg_frames[self._bg_frame_index])
        except Exception:
            pass
        # Frame duration (ms)
        try:
            duration = int(self._bg_frame_durations[self._bg_frame_index]) if self._bg_frame_durations else 100
        except Exception:
            duration = 100
        delay = max(20, duration)
        self.root.after(delay, self._animate_bg)

    def _mark_activity(self):
        self.last_activity_at = time.time()
        if self.sleeping:
            self._wake_from_sleep()

    def _enter_sleep(self):
        if self.sleeping or not self.locked:
            return
        self.sleeping = True

        if self.dpms_supported and self._dpms_force("off"):
            self._dpms_set_high_timeout()
            self.dpms_sleeping = True
            return

        self.dpms_sleeping = False
        for tag in ("ui", "input_shadow", "input_box", "input_gloss", "dots", "cursor", "status", "clock", "weather_icon"):
            self.canvas.itemconfig(tag, state="hidden")
        self.canvas.itemconfig("sleep_overlay", state="normal")

    def _wake_from_sleep(self):
        if not self.sleeping:
            return
        self.sleeping = False

        if self.dpms_sleeping:
            self._dpms_force("on")
            self._dpms_restore_timeout()
            self.dpms_sleeping = False
            return

        self.canvas.itemconfig("sleep_overlay", state="hidden")
        for tag in ("ui", "input_shadow", "input_box", "input_gloss", "dots", "status", "clock", "weather_icon"):
            self.canvas.itemconfig(tag, state="normal")
        self.canvas.itemconfig(self.cursor_line, state="normal" if self.cursor_visible else "hidden")
        self._update_dots()

    def _check_idle_sleep(self):
        if not self.locked:
            return
        if not self.sleeping and (time.time() - self.last_activity_at) >= self.IDLE_SLEEP_SECONDS:
            self._enter_sleep()
        self.root.after(300, self._check_idle_sleep)

    def _check_logout_timeout(self):
        if not self.locked:
            return
        elapsed = time.time() - self.locked_at
        
        # Force logout if force timeout reached
        if elapsed >= self.logout_timeout_force * 60:
            self._perform_logout()
            return
        
        # Show warning when main timeout reached
        if elapsed >= self.logout_timeout * 60:
            self.canvas.itemconfig(self.logout_warning, state="normal", text="PRESS ESCAPE TO LOGOUT")
        else:
            self.canvas.itemconfig(self.logout_warning, state="hidden")
        
        self.root.after(1000, self._check_logout_timeout)  # Check every second

    def _perform_logout(self):
        """Log out the user via gnome-session-quit."""
        try:
            subprocess.run(
                ["gnome-session-quit", "--logout", "--no-prompt"],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
            )
        except Exception as e:
            print(f"Could not perform logout: {e}", file=sys.stderr)
        finally:
            # Ensure window closes
            self.locked = False
            try:
                self.root.destroy()
            except Exception:
                pass

    def _fit_text_to_box(self, text: str) -> str:
        max_width = max(8, self._box_w - 20)
        if self.input_font.measure(text) <= max_width:
            return text

        tail = text
        while tail and self.input_font.measure("…" + tail) > max_width:
            tail = tail[1:]

        return ("…" + tail) if tail else ""

    def _dpms_force(self, mode: str) -> bool:
        try:
            subprocess.run(
                ["xset", "dpms", "force", mode],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=2,
            )
            return True
        except Exception:
            return False

    def _dpms_set_high_timeout(self) -> bool:
        """Set DPMS timeouts very high to keep display off while sleeping."""
        try:
            subprocess.run(
                ["xset", "dpms", "9999", "9999", "9999"],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=2,
            )
            return True
        except Exception:
            return False

    def _dpms_restore_timeout(self) -> bool:
        """Restore normal DPMS timeouts after waking."""
        try:
            subprocess.run(
                ["xset", "dpms", "600", "600", "600"],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=2,
            )
            return True
        except Exception:
            return False

    def _update_dots(self):
        if self.password_text:
            typed = len(self.password)
            if typed <= len(self.password_text):
                shown = self.password_text[:typed]
            else:
                shown = self.password_text + ("●" * (typed - len(self.password_text)))
        else:
            shown = "●" * len(self.password)

        shown = self._fit_text_to_box(shown)
        self.canvas.itemconfig(self.dot_text, text=shown)
        # Move cursor after the dots
        x0, y0, x1, y1 = self.canvas.bbox(self.dot_text) if shown else (
            self._box_origin_x + 10 - 1,
            self._box_y + 5,
            self._box_origin_x + 10 + 1,
            self._box_y + self._box_h - 5
        )
        cx = (x1 + 2) if shown else (self._box_origin_x + 10)
        cy1 = self._box_y + 5
        cy2 = self._box_y + self._box_h - 5
        self.canvas.coords(self.cursor_line, cx, cy1, cx, cy2)

    def _blink_cursor(self):
        if not self.locked:
            return
        self.cursor_visible = not self.cursor_visible
        state = "normal" if self.cursor_visible else "hidden"
        self.canvas.itemconfig(self.cursor_line, state=state)
        self.root.after(530, self._blink_cursor)

    def _format_elapsed(self, seconds: float) -> str:
        total = max(0, int(seconds))
        if total < 60:
            return "a few seconds ago…"

        minutes, secs = divmod(total, 60)
        hours, mins = divmod(minutes, 60)

        # if hours:
        #     if mins:
        #         return f"{hours}h {mins}m ago…"
        #     return f"{hours}h ago…"

        if mins > 40:
            return f"{mins % 10 + 30} minutes ago…"
        if mins == 1:
            return "1 minute ago"
        return f"{mins} minutes ago…"

    def _update_locked_time(self):
        if not self.locked:
            return
        elapsed = time.time() - self.locked_at
        self.canvas.itemconfig(
            self.locked_text,
            text=f"Locked by {self.username} · {self._format_elapsed(elapsed)}",
        )
        self.root.after(1000, self._update_locked_time)

    def _update_clock(self):
        """Update the top-right clock once per second."""
        if not self.locked:
            return
        try:
            t = time.strftime("%I:%M %p").lstrip("0")
        except Exception:
            t = time.strftime("%H:%M:%S")
        try:
            self.canvas.itemconfig(self.clock_text, text=t)
            self.canvas.itemconfig(self.clock_shadow, text=t)
        except Exception:
            pass
        self.root.after(1000, self._update_clock)

    # ── Weather popup / fetch ───────────────────────────────────────────
    def _toggle_weather_popup(self):
        if self.sleeping:
            return
        if self.weather_popup_visible:
            self._hide_weather_popup()
        else:
            self._show_weather_popup()

    def _weather_cache_valid(self) -> bool:
        return (
            self._weather_current is not None
            and self._weather_forecast is not None
            and (time.time() - self._weather_last_fetch_at) < self._weather_cache_ttl
        )

    def _ensure_weather_data(self, force: bool = False):
        if self.sleeping:
            return
        if not force and self._weather_cache_valid():
            if self.weather_popup_visible:
                self._render_weather_popup({"ok": True})
            return
        if self._weather_fetch_in_flight:
            return
        self._weather_fetch_in_flight = True
        threading.Thread(target=self._fetch_weather, daemon=True).start()

    def _schedule_weather_refresh(self):
        if not self.locked:
            return
        self.root.after(self._weather_cache_ttl * 1000, self._weather_refresh_tick)

    def _weather_refresh_tick(self):
        if not self.locked:
            return
        self._ensure_weather_data(force=True)
        self._schedule_weather_refresh()

    def _show_weather_popup(self):
        if self.weather_popup_visible:
            return
        self.weather_popup_visible = True
        sw = self.root.winfo_screenwidth()
        sh = self.root.winfo_screenheight()
        pw, ph = 360, 220
        x2 = sw - 18
        y2 = sh - 18
        x1 = x2
        y1 = y2
        # create popup rect and animate expansion
        self.popup_id = self.canvas.create_rectangle(x1, y1, x2, y2,
                                                     fill="#0f1012", outline="", tags=("weather_popup",))
        steps = 8
        def step(i):
            frac = i / steps
            nx1 = x2 - int(pw * frac)
            ny1 = y2 - int(ph * frac)
            self.canvas.coords(self.popup_id, nx1, ny1, x2, y2)
            if i < steps:
                self.root.after(16, lambda: step(i+1))
            else:
                # render placeholder
                self.canvas.create_text(nx1 + 16, ny1 + 12, text="Loading weather...",
                            anchor="nw", fill=self._text_color, font=self.input_font,
                            tags=("weather_popup", "weather_popup_content"))
                # start fetch (current + forecast) or use cache if fresh
                self._ensure_weather_data(force=False)
        step(1)

    def _hide_weather_popup(self):
        if not self.weather_popup_visible:
            return
        self.weather_popup_visible = False
        # animate shrink
        try:
            coords = self.canvas.coords(self.popup_id)
        except Exception:
            coords = None
        if coords:
            x1, y1, x2, y2 = coords
            steps = 6
            def step(i):
                frac = i / steps
                nx1 = int(x1 + (x2 - x1) * frac)
                ny1 = int(y1 + (y2 - y1) * frac)
                self.canvas.coords(self.popup_id, nx1, ny1, x2, y2)
                if i < steps:
                    self.root.after(16, lambda: step(i+1))
                else:
                    self.canvas.delete("weather_popup")
            step(1)
        else:
            self.canvas.delete("weather_popup")
        # also remove popup content tags
        self.canvas.delete("weather_popup_content")

    def _fetch_weather(self):
        if self._weather_fetch_in_flight is False:
            self._weather_fetch_in_flight = True
        cfg = load_config()
        api_key = os.environ.get("OPENWEATHER_API_KEY") or cfg.get("openweather_api_key") or cfg.get("api_key") or ""
        city = os.environ.get("FT_LOCK_CITY") or cfg.get("city") or "Paris"
        if not api_key:
            self._weather_fetch_in_flight = False
            self.root.after(0, lambda: self._render_weather_popup({"error": "No API key"}))
            return
        try:
            # Current weather
            url_now = f"https://api.openweathermap.org/data/2.5/weather?q={urllib.parse.quote(city)}&appid={api_key}&units=metric"
            with urllib.request.urlopen(url_now, timeout=8) as resp:
                raw = resp.read().decode("utf-8")
                now = json.loads(raw)
            # 5-day / 3-hour forecast
            url_fore = f"https://api.openweathermap.org/data/2.5/forecast?q={urllib.parse.quote(city)}&appid={api_key}&units=metric&cnt=40"
            with urllib.request.urlopen(url_fore, timeout=8) as resp:
                rawf = resp.read().decode("utf-8")
                fore = json.loads(rawf)

            self._weather_current = now
            self._weather_forecast = fore.get("list", [])
            self._weather_last_fetch_at = time.time()
            # update small icon temp
            try:
                t = round(now.get("main", {}).get("temp", 0))
                self.canvas.itemconfig(self.weather_temp_text, text=f"{t}°")
            except Exception:
                pass

            self.root.after(0, lambda: self._render_weather_popup({"ok": True}))
        except Exception as e:
            self.root.after(0, lambda: self._render_weather_popup({"error": str(e)}))
        finally:
            self._weather_fetch_in_flight = False
            if self._weather_last_fetch_at:
                self._schedule_weather_refresh()

    def _render_weather_popup(self, data: dict):
        # clear previous content
        self.canvas.delete("weather_popup_content")
        if not getattr(self, "popup_id", None):
            return
        try:
            x1, y1, x2, y2 = self.canvas.coords(self.popup_id)
        except Exception:
            return
        pad = 14
        if data.get("error"):
            self.canvas.create_text(x1 + pad, y1 + pad, text=f"Error: {data['error']}",
                                    anchor="nw", fill="#ff6666", font=self.input_font,
                                    tags=("weather_popup", "weather_popup_content"))
            return
        # If we have stored current + forecast use them, otherwise use passed data
        now = self._weather_current
        fore_list = self._weather_forecast or []

        if not now:
            # no data available
            self.canvas.create_text(x1 + pad, y1 + pad, text="No weather data",
                                    anchor="nw", fill=self._text_dim, font=self.input_font,
                                    tags=("weather_popup", "weather_popup_content"))
            return

        name = now.get("name", "Unknown")
        country = now.get("sys", {}).get("country", "")
        temp = round(now.get("main", {}).get("temp", 0))
        desc = now.get("weather", [{}])[0].get("description", "").title()
        icon_code = now.get("weather", [{}])[0].get("icon", "")
        icon = self._weather_icon_func(icon_code)

        # header
        self.canvas.create_text(x1 + pad, y1 + pad, text=f"{name}, {country}",
                                anchor="nw", fill=self._text_color, font=(None, 14, "bold"),
                                tags=("weather_popup", "weather_popup_content"))
        self.canvas.create_text(x1 + pad, y1 + pad + 22, text=f"{temp}°C  {desc}",
                                anchor="nw", fill=self._text_dim, font=self.input_font,
                                tags=("weather_popup", "weather_popup_content"))
        # big icon on right side
        self.canvas.create_text(x2 - pad - 8, y1 + pad + 10, text=icon,
                                anchor="ne", fill=self._text_color, font=(None, 32),
                                tags=("weather_popup", "weather_popup_content"))

        # Build forecast grouped by day
        groups = {}
        from datetime import datetime
        for item in fore_list:
            dt = datetime.fromtimestamp(item.get("dt", 0))
            key = dt.date()
            groups.setdefault(key, []).append(item)

        days = sorted(groups.keys())
        if not days:
            self.canvas.create_text(x1 + pad, y1 + pad + 56, text="No forecast available",
                                    anchor="nw", fill=self._text_dim, font=self.input_font,
                                    tags=("weather_popup", "weather_popup_content"))
            return

        # clamp popup_day_index
        if self.popup_day_index < 0:
            self.popup_day_index = 0
        if self.popup_day_index >= len(days):
            self.popup_day_index = len(days) - 1

        sel_date = days[self.popup_day_index]
        entries = groups.get(sel_date, [])

        # navigation arrows
        nav_y = y1 + pad + 64
        self.canvas.create_text(x1 + pad, nav_y, text="<",
                                anchor="w", fill=self._text_color, font=(None, 18, "bold"),
                                tags=("weather_popup", "weather_popup_content", "weather_nav_left"))
        self.canvas.create_text(x2 - pad, nav_y, text=">",
                                anchor="e", fill=self._text_color, font=(None, 18, "bold"),
                                tags=("weather_popup", "weather_popup_content", "weather_nav_right"))
        # bind nav
        self.canvas.tag_bind("weather_nav_left", "<Button-1>", lambda e: self._popup_prev_day())
        self.canvas.tag_bind("weather_nav_right", "<Button-1>", lambda e: self._popup_next_day())

        # Render hourly entries in a row
        max_show = 8
        start_x = x1 + pad
        y_base = nav_y + 18
        gap = max(36, int((x2 - x1 - pad*2) / max_show))
        from datetime import datetime
        for i, it in enumerate(entries[:max_show]):
            dt = datetime.fromtimestamp(it.get("dt", 0))
            tstr = dt.strftime("%H:%M")
            ttemp = round(it.get("main", {}).get("temp", 0))
            ticon = self._weather_icon_func(it.get("weather", [{}])[0].get("icon", ""))
            cx = start_x + i * gap
            self.canvas.create_text(cx, y_base, text=tstr, anchor="n", fill=self._text_dim,
                                    font=(None, 10), tags=("weather_popup", "weather_popup_content"))
            self.canvas.create_text(cx, y_base + 18, text=ticon, anchor="n", fill=self._text_color,
                                    font=(None, 14), tags=("weather_popup", "weather_popup_content"))
            self.canvas.create_text(cx, y_base + 40, text=f"{ttemp}°", anchor="n", fill=self._text_dim,
                                    font=(None, 11, "bold"), tags=("weather_popup", "weather_popup_content"))

        # update day label / available range
        day_label = sel_date.strftime("%a %d")
        self.canvas.create_text(x1 + pad + 56, nav_y, text=day_label,
                                anchor="w", fill=self._text_color, font=(None, 12, "bold"),
                                tags=("weather_popup", "weather_popup_content"))

    def _popup_prev_day(self):
        self.popup_day_index = max(0, self.popup_day_index - 1)
        self._render_weather_popup({"ok": True})

    def _popup_next_day(self):
        if self._weather_forecast:
            # clamp based on current grouped days during render
            self.popup_day_index += 1
            self._render_weather_popup({"ok": True})


        # ── Key handlers ────────────────────────────────────────────────────────

    def _on_key(self, event):
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping:
            return "break"

        if self.shaking:
            return "break"

        if event.keysym in self._BLOCKED_KEYSYMS:
            return "break"

        alt_down = bool(event.state & 0x0008)
        if alt_down:
            return "break"

        ch = event.char
        if ch and ch.isprintable():
            self.password += ch
            self._update_dots()
            self.canvas.itemconfig(self.status_text, text="")
        return "break"

    def _on_backspace(self, event):
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping:
            return "break"

        if self.shaking:
            return "break"
        self.password = self.password[:-1]
        self._update_dots()
        return "break"

    def _on_enter(self, event):
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping:
            return "break"

        if self.shaking:
            return "break"
        self._try_unlock()
        return "break"

    def _on_mouse_motion(self, event):
        self._mark_activity()
        return "break"

    def _on_mouse_button(self, event):
        self._mark_activity()
        return "break"

    def _on_canvas_click(self, event):
        # If popup not visible nothing to do
        if not getattr(self, "weather_popup_visible", False):
            return
        # Find items under pointer
        items = self.canvas.find_overlapping(event.x, event.y, event.x, event.y)
        for it in items:
            tags = self.canvas.gettags(it)
            if any(t in ("weather_popup", "weather_icon") for t in tags):
                return
        # clicked outside
        self._hide_weather_popup()
        return

    def _on_escape(self, event):
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping:
            return "break"
        # Only allow logout if warning is visible
        if self.canvas.itemcget(self.logout_warning, "state") == "normal":
            self._perform_logout()
        return "break"

    def _try_unlock(self):
        pwd = self.password
        self.password = ""
        self._update_dots()

        def check():
            ok = authenticate(self.username, pwd)
            self.root.after(0, lambda: self._auth_result(ok))

        threading.Thread(target=check, daemon=True).start()

    def _auth_result(self, success: bool):
        if success:
            self.locked = False
            self.root.grab_release()
            self.root.destroy()
        else:
            self.canvas.itemconfig(self.status_text, text="Wrong password")
            self._shake()

    # ── Shake animation ──────────────────────────────────────────────────────

    def _shake(self):
        self.shaking = True
        offsets = []
        d = self.SHAKE_DISTANCE
        for i in range(self.SHAKE_STEPS):
            offsets.append(d if i % 2 == 0 else -d)
            d = max(2, d - 2)
        offsets.append(0)
        self._shake_step(offsets, self._box_origin_x)

    def _shake_step(self, offsets, current_x):
        if not offsets:
            self.shaking = False
            return
        dx = offsets[0]
        new_x = self._box_origin_x + dx
        shift = new_x - current_x

        for tag in ("input_shadow", "input_box", "input_gloss", "dots", "cursor"):
            self.canvas.move(tag, shift, 0)

        self.root.after(self.SHAKE_INTERVAL,
                        lambda: self._shake_step(offsets[1:], new_x))


# ── Entry point ─────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="ft_lock — Python lock screen")
    parser.add_argument("--image",   default="",           help="Path to background image")
    parser.add_argument("--user",    default=os.getenv("USER", "user"), help="Username to display")
    parser.add_argument("--message", default="Back sOOn..", help="Message under username")
    args = parser.parse_args()

    config = load_config()

    image = args.image or config.get("image_file") or config.get("wallpaper", "")
    message = args.message
    if args.message == parser.get_default("message"):
        message = config.get("text", args.message)

    password_text = config.get("passwordtext", config.get("password_text", ""))
    accent_color = config.get("ft_colorname", "#5a7ab0")
    username = config.get("username", args.user)
    timeout = int(config.get("timeout", "10"))
    logout_timeout = int(config.get("logout_timeout", "30"))
    logout_timeout_force = int(config.get("logout_timeout_force", "42"))

    if logout_timeout_force < logout_timeout:
        logout_timeout_force = logout_timeout + 5

    root = tk.Tk()
    LockScreen(root, image, username, message, password_text=password_text, accent_color=accent_color, timeout=timeout, logout_timeout=logout_timeout, logout_timeout_force=logout_timeout_force)
    root.mainloop()


if __name__ == "__main__":
    main()