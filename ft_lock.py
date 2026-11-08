#!/usr/bin/env python3
"""
ft_lock.py — fully customizable fullscreen lock screen for Ubuntu (Tk).

    python3 ft_lock.py                    lock the screen
    python3 ft_lock.py --preview          windowed + live-reload, NEVER grabs input (safe for testing)
    python3 ft_lock.py --edit             open the Rust layout editor
    python3 ft_lock.py --check            validate config + layout, list widgets
    python3 ft_lock.py --init [DIR]       copy a starter config/layout/widgets into ~/.config/ft_lock
    python3 ft_lock.py --new-widget NAME  scaffold widgets/NAME.py   (add --class for a drawing widget)

Configuration lives in YAML (see config.yaml and default.yaml, README.md for every option).
The legacy key=value ft_lock.cfg is still read when no config.yaml exists.
Unlocks with the current user's system password (PAM) or the FT_LOCK_PASSWORD env var.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import queue
import random
import re
import shutil
import socket
import subprocess
import sys
import threading
import time
import traceback
import urllib.parse
import urllib.request
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

import tkinter as tk
from tkinter import font as tkfont

try:
    import yaml  # PyYAML  (sudo apt install python3-yaml)
except ImportError:  # the lock must still work without it
    yaml = None

# ── PAM authentication (requires python3-pam) ──────────────────────────────
try:
    import pam  # type: ignore[import-not-found]
    PAM_AVAILABLE = True
except ImportError:
    PAM_AVAILABLE = False

ROOT = Path(__file__).resolve().parent
USER_DIR = Path.home() / ".config" / "ft_lock"

DEFAULT_CONFIG: dict[str, Any] = {
    "version": 2,
    "layout": "default.yaml",
    "widgets_dir": "widgets",
    "lock": {
        "timeout": 5,                 # seconds of inactivity before the screen sleeps (DPMS off)
        "logout_timeout": 30,         # minutes before Escape may log out
        "logout_timeout_force": 42,   # minutes before forced logout
        "allow_escape_logout": True,
        "password_text": "",
    },
    "display": {
        "username": "",
        "message": "Back sOOn..",
        "background_image": "",
        "background_fit": "stretch",  # stretch | cover | contain
        "background_color": "#1a0a1e",
        "accent_color": "#5a7ab0",
        "font_family": "Ubuntu",
        "scale_layout": True,         # scale layout.screen -> real screen size
    },
    "weather": {"api_key": "", "city": "Paris"},
    "carousel": {
        "enabled": False,
        "folder": "",            # every png/jpg/gif/webp/bmp in this folder
        "images": [],            # and/or an explicit list of files
        "mode": "interval",      # launch = another image at each lock | interval = also rotate while locked
        "interval": 60,          # seconds between images (mode: interval)
        "order": "sequential",   # sequential | random
        "transition": "fade",    # fade | none
    },
    "variables": {},
}

# Legacy ft_lock.cfg keys -> (section, key)
LEGACY_KEYS = {
    "image_file": ("display", "background_image"), "wallpaper": ("display", "background_image"),
    "passwordtext": ("lock", "password_text"), "password_text": ("lock", "password_text"),
    "text": ("display", "message"), "ft_colorname": ("display", "accent_color"),
    "username": ("display", "username"), "timeout": ("lock", "timeout"),
    "logout_timeout": ("lock", "logout_timeout"), "logout_timeout_force": ("lock", "logout_timeout_force"),
    "api_key": ("weather", "api_key"), "openweather_api_key": ("weather", "api_key"),
    "city": ("weather", "city"), "layout": ("layout",), "layout_file": ("layout",),
}

# Layout files with version < 2 (written by the first editor) relied on per-id defaults.
LEGACY_V1 = {
    "locked_text": {"type": "text", "anchor": "s", "text": "Locked by {username} · {elapsed}", "color": "#c8d2e6", "size": 13},
    "message": {"type": "text", "anchor": "s", "text": "{message}", "color": "#8a96aa", "size": 11, "italic": True},
    "clock": {"type": "text", "anchor": "ne", "text": "{clock}", "color": "accent", "size": 16, "bold": True,
              "shadow": True, "font": "Ubuntu Mono"},
    "logout_warning": {"type": "text", "anchor": "center", "color": "#ff4444", "size": 32, "bold": True},
    "password_input": {"type": "input"},
    "weather": {"type": "weather"},
}

ANCHORS = ("nw", "n", "ne", "w", "center", "e", "sw", "s", "se")
VISIBLE_CONDITIONS = ("always", "after_logout_timeout", "after_force_timeout", "after_seconds", "before_seconds")
BUILTIN_TYPES = ("text", "image", "rect", "input", "weather")

DEFAULT_LAYOUT: dict[str, Any] = {
    "version": 2,
    "screen": {"width": 1920, "height": 1080},
    "elements": [
        {"id": "locked_text", "type": "text", "x": 380, "y": 60, "anchor": "s",
         "text": "Locked by {username} · {elapsed}", "color": "#c8d2e6", "size": 13},
        {"id": "message", "type": "text", "x": 350, "y": 80, "anchor": "s", "text": "{message}",
         "color": "#8a96aa", "size": 11, "italic": True},
        {"id": "password_input", "type": "input", "x": 230, "y": 92},
        {"id": "clock", "type": "text", "x": 1900, "y": 18, "anchor": "ne", "text": "{clock}", "color": "accent",
         "size": 16, "bold": True, "shadow": True, "font": "Ubuntu Mono"},
        {"id": "weather", "type": "weather", "x": 1750, "y": 950},
        {"id": "logout_warning", "type": "text", "x": 800, "y": 500, "anchor": "center",
         "text": "PRESS ESCAPE TO LOGOUT", "color": "#ff4444", "size": 32, "bold": True,
         "visible": "after_logout_timeout"},
    ],
}


# ── small helpers ───────────────────────────────────────────────────────────
def log(*args: Any) -> None:
    print("[ft_lock]", *args, file=sys.stderr)


def as_int(v: Any, default: int) -> int:
    try:
        return int(float(v))
    except (TypeError, ValueError):
        return default


def as_float(v: Any, default: float) -> float:
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


def as_bool(v: Any, default: bool = False) -> bool:
    if isinstance(v, bool):
        return v
    if v is None or v == "":
        return default
    if isinstance(v, str):
        return v.strip().lower() in ("1", "true", "yes", "on")
    return bool(v)


def deep_merge(base: dict, over: dict) -> dict:
    out = dict(base)
    for k, v in (over or {}).items():
        out[k] = deep_merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def norm_anchor(v: Any, default: str = "nw") -> str:
    v = str(v or default).strip().lower()
    v = {"c": "center", "centre": "center", "middle": "center"}.get(v, v)
    return v if v in ANCHORS else default


def resolve_path(p: Any, *bases: Path) -> Path:
    """Absolute path, or the first existing <base>/<p>, or <first base>/<p>."""
    path = Path(os.path.expanduser(str(p)))
    if path.is_absolute():
        return path
    for b in bases:
        if (b / path).exists():
            return b / path
    return (bases[0] if bases else Path.cwd()) / path


IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"}
STATE_PATH = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "ft_lock" / "state.json"


def carousel_images(cfg: dict, base: Path) -> list[Path]:
    """Images listed in carousel.images, then every image of carousel.folder (sorted by name)."""
    car = cfg.get("carousel") or {}
    found: list[Path] = []
    for item in car.get("images") or []:
        p = resolve_path(item, base, ROOT)
        if p.is_file():
            found.append(p)
    folder = car.get("folder")
    if folder:
        d = resolve_path(folder, base, ROOT)
        if d.is_dir():
            found += sorted((f for f in d.iterdir() if f.is_file() and f.suffix.lower() in IMAGE_EXTS),
                            key=lambda f: f.name.lower())
    out: list[Path] = []
    for p in found:
        if p not in out:
            out.append(p)
    return out


def pick_carousel(images: list[Path], order: str, persist: bool) -> Path:
    """Choose the image for this launch: the next one (sequential) or a different random one.
    The last choice is remembered in ~/.cache/ft_lock/state.json so each launch differs."""
    try:
        last = json.loads(STATE_PATH.read_text(encoding="utf-8")).get("carousel_last", "")
    except (OSError, ValueError):
        last = ""
    names = [str(p) for p in images]
    last_idx = names.index(last) if last in names else -1
    if str(order).lower() == "random" and len(images) > 1:
        idx = random.choice([i for i in range(len(images)) if i != last_idx])
    else:
        idx = (last_idx + 1) % len(images)
    if persist:
        try:
            STATE_PATH.parent.mkdir(parents=True, exist_ok=True)
            STATE_PATH.write_text(json.dumps({"carousel_last": names[idx]}), encoding="utf-8")
        except OSError:
            pass
    return images[idx]


_PLACEHOLDER = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")


def render_template(text: str, values: dict[str, Any]) -> str:
    """Replace {name}; unknown names are left untouched (no str.format surprises)."""
    return _PLACEHOLDER.sub(lambda m: str(values.get(m.group(1), m.group(0))), text)


def safe(fn: Callable, *args: Any, what: str = "") -> Any:
    """Run user/plugin code; the lock screen must never die because of it."""
    try:
        return fn(*args)
    except Exception:  # noqa: BLE001
        log(f"error in {what or getattr(fn, '__name__', 'callback')}:\n" + traceback.format_exc())
        return None


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


# ── YAML / config loading ───────────────────────────────────────────────────
def _scalar(value: str) -> Any:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    low = value.lower()
    if low in ("true", "false"):
        return low == "true"
    try:
        return int(value)
    except ValueError:
        try:
            return float(value)
        except ValueError:
            return value


def _mini_yaml_layout(text: str) -> dict:
    """Dependency-free fallback reader for the flat `elements:` list (used only without PyYAML)."""
    elements: list[dict] = []
    current: dict | None = None
    version = 1
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or line == "elements:":
            continue
        if line.startswith("version:"):
            version = as_int(line.split(":", 1)[1], 1)
            continue
        if line.startswith("- "):
            if current:
                elements.append(current)
            current, line = {}, line[2:].strip()
        if current is None or ":" not in line:
            continue
        key, value = line.split(":", 1)
        current[key.strip()] = _scalar(value)
    if current:
        elements.append(current)
    return {"version": version, "elements": elements}


def read_yaml(path: Path) -> Any:
    text = path.read_text(encoding="utf-8")
    if yaml is not None:
        return yaml.safe_load(text)
    log("PyYAML missing (sudo apt install python3-yaml): using the minimal fallback parser")
    return _mini_yaml_layout(text)


def load_legacy_cfg(path: Path) -> dict:
    cfg: dict = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip().strip('"').strip("'")
        if value == "" or key not in LEGACY_KEYS:
            continue  # empty values mean "use the default" (example.cfg ships with empty values)
        target = LEGACY_KEYS[key]
        if len(target) == 1:
            cfg[target[0]] = value
        else:
            cfg.setdefault(target[0], {})[target[1]] = value
    return cfg


def load_config(explicit: str | None = None) -> tuple[dict, Path, Path | None]:
    """-> (config, base_dir used to resolve relative paths, source file or None)."""
    candidates: list[Path] = [Path(explicit)] if explicit else [
        USER_DIR / "config.yaml", ROOT / "config.yaml", USER_DIR / "ft_lock.cfg",
        ROOT / "ft_lock" / "ft_lock.cfg"]
    for path in candidates:
        if not path.is_file():
            continue
        try:
            if path.suffix in (".yaml", ".yml"):
                if yaml is None:
                    log("config.yaml found but PyYAML is missing (sudo apt install python3-yaml)")
                    continue
                data = read_yaml(path) or {}
                if not isinstance(data, dict):
                    raise ValueError("top level must be a mapping")
            else:
                data = load_legacy_cfg(path)
            return deep_merge(DEFAULT_CONFIG, data), path.parent, path
        except Exception as e:  # noqa: BLE001
            log(f"cannot read {path}: {e}")
    return deep_merge(DEFAULT_CONFIG, {}), ROOT, None


def apply_legacy(spec: dict) -> dict:
    spec = dict(spec)
    for k, v in LEGACY_V1.get(str(spec.get("id", "")), {}).items():
        spec.setdefault(k, v)
    return spec


def load_layout(path: Path) -> dict:
    try:
        data = read_yaml(path)
    except Exception as e:  # noqa: BLE001
        log(f"cannot read layout {path}: {e} — using the built-in layout")
        return json.loads(json.dumps(DEFAULT_LAYOUT))
    if isinstance(data, list):
        data = {"version": 1, "elements": data}
    if not isinstance(data, dict) or not isinstance(data.get("elements"), list):
        log(f"layout {path} has no 'elements' list — using the built-in layout")
        return json.loads(json.dumps(DEFAULT_LAYOUT))
    version = as_int(data.get("version"), 1)
    elements = [e for e in data["elements"] if isinstance(e, dict)]
    if version < 2:
        elements = [apply_legacy(e) for e in elements]
    return {"version": version, "screen": data.get("screen"), "elements": elements}


# ── widget plugins ──────────────────────────────────────────────────────────
_PLUGIN_CACHE: dict[str, Any] = {}
_NAME_OK = re.compile(r"^[A-Za-z0-9_\-]+$")


def widget_dirs(cfg: dict, base: Path) -> list[Path]:
    dirs = [resolve_path(cfg.get("widgets_dir", "widgets"), base, ROOT), USER_DIR / "widgets"]
    out: list[Path] = []
    for d in dirs:
        if d.is_dir() and d not in out:
            out.append(d)
    return out


def find_plugin_file(kind: str, spec: dict, dirs: list[Path], base: Path) -> Path | None:
    if spec.get("script"):
        p = resolve_path(spec["script"], base, ROOT)
        return p if p.is_file() else None
    if not _NAME_OK.match(kind):
        return None
    for d in dirs:
        p = d / f"{kind}.py"
        if p.is_file():
            return p
    return None


def load_plugin_module(path: Path) -> Any:
    key = str(path)
    if key not in _PLUGIN_CACHE:
        spec = importlib.util.spec_from_file_location(f"ft_lock_widget_{path.stem}", path)
        mod = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
        spec.loader.exec_module(mod)  # type: ignore[union-attr]
        _PLUGIN_CACHE[key] = mod
    return _PLUGIN_CACHE[key]


def list_widgets(dirs: list[Path]) -> list[dict]:
    out = []
    for d in dirs:
        for p in sorted(d.glob("*.py")):
            if p.name.startswith("_"):
                continue
            info: dict[str, Any] = {"name": p.stem, "file": str(p), "label": p.stem, "description": "",
                                    "size": [160, 32], "params": {}, "error": None}
            try:
                mod = load_plugin_module(p)
                info["label"] = str(getattr(mod, "LABEL", p.stem))
                info["description"] = str(getattr(mod, "DESCRIPTION", ""))
                size = getattr(mod, "SIZE", (160, 32))
                info["size"] = [int(size[0]), int(size[1])]
                info["params"] = {k: v for k, v in dict(getattr(mod, "PARAMS", {})).items()
                                  if isinstance(v, (str, int, float, bool))}
                if not hasattr(mod, "Widget") and not hasattr(mod, "update"):
                    info["error"] = "define a `Widget` class or an `update(ctx)` function"
            except Exception as e:  # noqa: BLE001
                info["error"] = f"{type(e).__name__}: {e}"
            out.append(info)
    return out


class PluginContext:
    """What a widget plugin sees. Drawing helpers take coordinates RELATIVE to the widget's x/y."""

    def __init__(self, app: "LockScreen", element: "Element", params: dict):
        self._app, self._el = app, element
        self.canvas = app.canvas
        self.root = app.root
        self.params = params
        self.tag = element.tag
        self.x, self.y = element.pos()
        self.scale = app.sf                      # layout -> screen scale (1.0 unless layout.screen differs)
        self.screen_width, self.screen_height = app.sw, app.sh
        self.accent = app.accent
        self.color = app.color(params.get("color"), "#ffffff")
        self.size = max(1, round(as_float(params.get("size"), 14) * app.sf))
        self.font_family = str(params.get("font") or app.font_family)

    # values
    @property
    def values(self) -> dict[str, Any]:
        return self._app.values

    @property
    def username(self) -> str:
        return self._app.username

    @property
    def locked_seconds(self) -> float:
        return time.time() - self._app.locked_at

    def param(self, name: str, default: Any = None) -> Any:
        return self.params.get(name, default)

    def font(self, size: int | None = None, bold: bool = False, italic: bool = False,
             family: str | None = None) -> tuple:
        style = " ".join(s for s, on in (("bold", bold), ("italic", italic)) if on)
        return (family or self.font_family, size or self.size, style) if style else (family or self.font_family, size or self.size)

    # drawing (all items are tagged so the engine can hide/show them)
    def _tags(self, kw: dict) -> tuple:
        return tuple(kw.pop("tags", ())) + (self.tag, "el")

    def text(self, dx: float, dy: float, text: str = "", **kw: Any) -> int:
        kw.setdefault("fill", self.color)
        kw.setdefault("font", self.font())
        kw.setdefault("anchor", "nw")
        return self.canvas.create_text(self.x + dx, self.y + dy, text=text, tags=self._tags(kw), **kw)

    def rect(self, dx0: float, dy0: float, dx1: float, dy1: float, **kw: Any) -> int:
        return self.canvas.create_rectangle(self.x + dx0, self.y + dy0, self.x + dx1, self.y + dy1,
                                            tags=self._tags(kw), **kw)

    def oval(self, dx0: float, dy0: float, dx1: float, dy1: float, **kw: Any) -> int:
        return self.canvas.create_oval(self.x + dx0, self.y + dy0, self.x + dx1, self.y + dy1,
                                       tags=self._tags(kw), **kw)

    def line(self, *points: float, **kw: Any) -> int:
        pts = [p + (self.x if i % 2 == 0 else self.y) for i, p in enumerate(points)]
        return self.canvas.create_line(*pts, tags=self._tags(kw), **kw)

    def polygon(self, *points: float, **kw: Any) -> int:
        pts = [p + (self.x if i % 2 == 0 else self.y) for i, p in enumerate(points)]
        return self.canvas.create_polygon(*pts, tags=self._tags(kw), **kw)

    def image(self, dx: float, dy: float, photo: Any, **kw: Any) -> int:
        kw.setdefault("anchor", "nw")
        return self.canvas.create_image(self.x + dx, self.y + dy, image=photo, tags=self._tags(kw), **kw)

    def itemconfig(self, item: int, **kw: Any) -> None:
        self.canvas.itemconfig(item, **kw)

    def coords(self, item: int, *points: float) -> None:
        pts = [p + (self.x if i % 2 == 0 else self.y) for i, p in enumerate(points)]
        self.canvas.coords(item, *pts)

    def run(self, fn: Callable[[], Any], callback: Callable[[Any], None] | None = None) -> None:
        """Run a slow function in a thread; callback(result) runs on the Tk thread."""
        self._app.run_async(fn, callback)

    def after(self, ms: int, fn: Callable[[], None]) -> None:
        self.root.after(ms, lambda: safe(fn, what=f"widget {self._el.spec.get('id')} timer"))


# ── async helpers ───────────────────────────────────────────────────────────
class CommandSource:
    """Runs a shell command every `interval` seconds in a thread and keeps the last output."""

    def __init__(self, app: "LockScreen", command: str, interval: float, shell: bool):
        self.app, self.command, self.interval, self.shell = app, command, max(0.5, interval), shell
        self.value, self.busy, self.next_at = "", False, 0.0

    def _run(self) -> str:
        try:
            import shlex
            args: Any = self.command if self.shell else shlex.split(self.command)
            r = subprocess.run(args, shell=self.shell, capture_output=True, text=True, timeout=3)
            return (r.stdout or "").strip()[:400]
        except Exception:  # noqa: BLE001
            return ""

    def poll(self, now: float) -> None:
        if self.busy or now < self.next_at:
            return
        self.busy, self.next_at = True, now + self.interval
        self.app.run_async(self._run, self._done)

    def _done(self, result: Any) -> None:
        self.value, self.busy = result or "", False


class WeatherService:
    TTL = 300

    def __init__(self, app: "LockScreen"):
        self.app = app
        self.current: dict | None = None
        self.forecast: list | None = None
        self.last = 0.0
        self.in_flight = False
        self.error: str | None = None
        self.listeners: list[Callable[[], None]] = []
        self._timer: str | None = None

    def valid(self) -> bool:
        return self.current is not None and self.forecast is not None and time.time() - self.last < self.TTL

    def ensure(self, force: bool = False) -> None:
        if self.app.sleeping or self.in_flight:
            return
        if not force and self.valid():
            self._notify()
            return
        self.in_flight = True
        self.app.run_async(self._fetch, self._done)

    def _fetch(self) -> tuple:
        cfg = self.app.cfg["weather"]
        api_key = os.environ.get("OPENWEATHER_API_KEY") or str(cfg.get("api_key") or "")
        city = os.environ.get("FT_LOCK_CITY") or str(cfg.get("city") or "") or "Paris"
        if not api_key:
            return ("error", "No API key")
        try:
            q = urllib.parse.quote(city)
            with urllib.request.urlopen(
                    f"https://api.openweathermap.org/data/2.5/weather?q={q}&appid={api_key}&units=metric", timeout=8) as r:
                now = json.loads(r.read().decode("utf-8"))
            with urllib.request.urlopen(
                    f"https://api.openweathermap.org/data/2.5/forecast?q={q}&appid={api_key}&units=metric&cnt=40", timeout=8) as r:
                fore = json.loads(r.read().decode("utf-8"))
            return ("ok", now, fore.get("list", []))
        except Exception as e:  # noqa: BLE001
            return ("error", str(e))

    def _done(self, result: Any) -> None:
        self.in_flight = False
        if result and result[0] == "ok":
            self.current, self.forecast, self.last, self.error = result[1], result[2], time.time(), None
            if self._timer:
                self.app.root.after_cancel(self._timer)
            if self.app.locked:
                self._timer = self.app.root.after(self.TTL * 1000, lambda: self.ensure(force=True))
        else:
            self.error = result[1] if result else "unknown error"
        self._notify()

    def _notify(self) -> None:
        for fn in list(self.listeners):
            safe(fn, what="weather listener")

    @property
    def temp(self) -> str:
        if not self.current:
            return "--°"
        return f"{round(self.current.get('main', {}).get('temp', 0))}°"

    @property
    def description(self) -> str:
        if not self.current:
            return ""
        return str(self.current.get("weather", [{}])[0].get("description", "")).title()


# ── layout elements ─────────────────────────────────────────────────────────
class Element:
    _counter = 0

    def __init__(self, app: "LockScreen", spec: dict):
        Element._counter += 1
        self.app, self.spec = app, spec
        self.tag = f"el{Element._counter}"
        self.shown = True
        self.alive = True
        self.local: dict[str, Any] = {}       # extra template values (e.g. command output)

    # coordinates / sizes, already scaled to the real screen
    def pos(self) -> tuple[float, float]:
        return (as_float(self.spec.get("x"), 0.0) * self.app.sx, as_float(self.spec.get("y"), 0.0) * self.app.sy)

    def length(self, key: str, default: float = 0.0, axis: str = "x") -> float:
        return as_float(self.spec.get(key), default) * (self.app.sx if axis == "x" else self.app.sy)

    def scaled(self, key: str, default: float) -> float:
        return as_float(self.spec.get(key), default) * self.app.sf

    def font(self, size_key: str = "size", default_size: int = 18, family_default: str | None = None) -> tuple:
        s = self.spec
        family = str(s.get("font") or family_default or self.app.font_family)
        size = max(1, round(as_float(s.get(size_key), default_size) * self.app.sf))
        style = " ".join(n for n, on in (("bold", as_bool(s.get("bold"))), ("italic", as_bool(s.get("italic")))) if on)
        return (family, size, style) if style else (family, size)

    def create(self) -> None: ...

    def update(self, now: float) -> None: ...

    def on_visibility(self, visible: bool) -> None:
        self.app.canvas.itemconfig(self.tag, state="normal" if visible else "hidden")

    def tick(self, now: float) -> None:
        vis = self.app.item_visible(self.spec)
        if vis != self.shown:
            self.shown = vis
            self.on_visibility(vis)
        if self.shown:
            self.update(now)

    def destroy(self) -> None:
        self.alive = False


class TextElement(Element):
    def create(self) -> None:
        s, app = self.spec, self.app
        anchor = norm_anchor(s.get("anchor"), "nw")
        x, y = self.pos()
        kw: dict[str, Any] = {"anchor": anchor, "font": self.font(), "tags": (self.tag, "el")}
        wrap = self.length("width", 0)
        if wrap > 0:
            kw["width"] = wrap
        kw["justify"] = str(s.get("align") or ("left" if anchor in ("nw", "w", "sw") else "right" if anchor in ("ne", "e", "se") else "center"))
        shadow = s.get("shadow")
        self.shadow_item = None
        if shadow:
            shadow_color = shadow if isinstance(shadow, str) and shadow.lower() not in ("true", "yes", "on") else "#000000"
            self.shadow_item = app.canvas.create_text(x + 1, y + 1, text="", fill=shadow_color, **kw)
        self.item = app.canvas.create_text(x, y, text="", fill=app.color(s.get("color"), "#ffffff"), **kw)
        self.template = str(s.get("text", "Text"))
        self.last: str | None = None
        self.cmd = None
        if s.get("command"):
            self.cmd = CommandSource(app, str(s["command"]), as_float(s.get("interval"), 5), as_bool(s.get("shell")))
        self.update(time.time())

    def update(self, now: float) -> None:
        if self.cmd:
            self.cmd.poll(now)
            self.local["output"] = self.cmd.value
        text = render_template(self.template, {**self.app.values, **self.local})
        if text != self.last:
            self.last = text
            self.app.canvas.itemconfig(self.item, text=text)
            if self.shadow_item:
                self.app.canvas.itemconfig(self.shadow_item, text=text)


class ImageElement(Element):
    def create(self) -> None:
        s, app = self.spec, self.app
        path = resolve_path(s.get("path", ""), app.layout_path.parent, app.base_dir, ROOT)
        x, y = self.pos()
        anchor = norm_anchor(s.get("anchor"), "nw")
        box_w, box_h = int(self.length("width", 0)), int(self.length("height", 0, "y"))
        stretch = str(s.get("fit", "contain")).lower() == "stretch"
        self.frames: list[Any] = []
        self.delays: list[int] = []
        self.index = 0
        try:
            from PIL import Image, ImageSequence, ImageTk

            def fit(im: Any) -> Any:
                im = im.convert("RGBA")
                nat_w, nat_h = im.size
                if not box_w and not box_h:
                    w, h = round(nat_w * app.sf), round(nat_h * app.sf)
                elif stretch and box_w and box_h:
                    w, h = box_w, box_h
                else:
                    ratio = min(box_w / nat_w if box_w else 1e9, box_h / nat_h if box_h else 1e9)
                    w, h = round(nat_w * ratio), round(nat_h * ratio)
                return im.resize((max(1, w), max(1, h)), Image.LANCZOS) if (w, h) != (nat_w, nat_h) else im

            img = Image.open(path)
            if getattr(img, "is_animated", False):
                for frame in ImageSequence.Iterator(img):
                    self.delays.append(max(20, int(frame.info.get("duration", 100))))
                    self.frames.append(ImageTk.PhotoImage(fit(frame)))
            else:
                self.frames.append(ImageTk.PhotoImage(fit(img)))
        except ImportError:
            self.frames.append(tk.PhotoImage(file=str(path)))      # no Pillow: PNG/GIF, natural size
        self.item = app.canvas.create_image(x, y, image=self.frames[0], anchor=anchor, tags=(self.tag, "el"))
        if len(self.frames) > 1:
            app.root.after(self.delays[0], self._animate)

    def _animate(self) -> None:
        if not self.alive or not self.app.locked:
            return
        self.index = (self.index + 1) % len(self.frames)
        self.app.canvas.itemconfig(self.item, image=self.frames[self.index])
        self.app.root.after(self.delays[self.index], self._animate)


class RectElement(Element):
    def create(self) -> None:
        s, app = self.spec, self.app
        x, y = self.pos()
        w, h = self.length("width", 100), self.length("height", 40, "y")
        anchor = norm_anchor(s.get("anchor"), "nw")
        x0 = x - (w / 2 if anchor in ("n", "center", "s") else w if anchor in ("ne", "e", "se") else 0)
        y0 = y - (h / 2 if anchor in ("w", "center", "e") else h if anchor in ("sw", "s", "se") else 0)
        fill = app.color(s.get("color"), "#ffffff") if s.get("color", "#ffffff") not in ("", "none") else ""
        border = s.get("border")
        outline = app.color(border, "") if border else ""
        radius = self.scaled("radius", 0)
        kw = {"fill": fill, "outline": outline, "width": max(1, round(self.scaled("border_width", 1))) if outline else 0}
        if radius > 0:
            app.draw_rounded_rect(x0, y0, x0 + w, y0 + h, radius, tags=(self.tag, "el"), **kw)
        else:
            app.canvas.create_rectangle(x0, y0, x0 + w, y0 + h, tags=(self.tag, "el"), **kw)


class InputElement(Element):
    """The password box. x/y is its top-left corner."""

    def create(self) -> None:
        s, app, c = self.spec, self.app, self.app.canvas
        x, y = self.pos()
        self.w, self.h = self.length("width", 240), self.length("height", 32, "y")
        radius = self.scaled("radius", 10)
        accent = app.color(s.get("color"), app.accent)
        bg = app.color(s.get("bg"), "#1a2840")
        gloss = s.get("gloss", True)
        text_color = app.color(s.get("text_color"), "#c8d2e6")
        mono = str(s.get("font") or "Ubuntu Mono")
        size = max(1, round(as_float(s.get("size"), 13) * app.sf))
        self.shake_tag = self.tag + "s"
        self.origin_x, self.y0 = x, y
        tags = (self.tag, self.shake_tag, "el")
        app.draw_rounded_rect(x + 2, y + 2, x + self.w + 2, y + self.h + 2, radius, fill="#101725", outline="#101725", tags=tags)
        app.draw_rounded_rect(x, y, x + self.w, y + self.h, radius, fill=bg, outline=accent, width=2, tags=tags)
        if as_bool(gloss, True):
            gloss_color = gloss if isinstance(gloss, str) and gloss.startswith("#") else "#274067"
            app.draw_rounded_rect(x + 2, y + 2, x + self.w - 2, y + self.h / 2 + 1, max(3, radius - 3),
                                  fill=gloss_color, outline="", tags=tags)
        try:
            self.font_obj = tkfont.Font(family=mono, size=size)
        except Exception:  # noqa: BLE001
            self.font_obj = tkfont.Font(size=size)
        self.dots = c.create_text(x + 10, y + self.h / 2, text="", anchor="w", fill=text_color,
                                  font=self.font_obj, tags=tags)
        self.cursor_visible = True
        self.cursor = c.create_line(x + 10, y + 5, x + 10, y + self.h - 5, fill=text_color, width=1, tags=tags)
        status_font = (app.font_family, max(1, round(as_float(s.get("status_size"), 11) * app.sf)), "italic")
        self.status = c.create_text(x, y + self.h + 10, text="", anchor="nw", fill=app.color(s.get("error_color"), "#e05050"),
                                    font=status_font, tags=(self.tag, "el"))
        self.shown_text = ""

    def fit_text(self, text: str) -> str:
        max_width = max(8, self.w - 20)
        if self.font_obj.measure(text) <= max_width:
            return text
        tail = text
        while tail and self.font_obj.measure("…" + tail) > max_width:
            tail = tail[1:]
        return ("…" + tail) if tail else ""

    def set_text(self, shown: str) -> None:
        c = self.app.canvas
        shown = self.fit_text(shown)
        c.itemconfig(self.dots, text=shown)
        bbox = c.bbox(self.dots) if shown else None
        cx = (bbox[2] + 2) if bbox else (self.origin_x + 10)
        c.coords(self.cursor, cx, self.y0 + 5, cx, self.y0 + self.h - 5)

    def set_status(self, text: str) -> None:
        self.app.canvas.itemconfig(self.status, text=text)

    def blink(self) -> None:
        self.cursor_visible = not self.cursor_visible
        if self.shown:
            self.app.canvas.itemconfig(self.cursor, state="normal" if self.cursor_visible else "hidden")

    def on_visibility(self, visible: bool) -> None:
        super().on_visibility(visible)
        if visible:
            self.app.canvas.itemconfig(self.cursor, state="normal" if self.cursor_visible else "hidden")

    def shift(self, dx: float) -> None:
        self.app.canvas.move(self.shake_tag, dx, 0)


class WeatherElement(Element):
    """Round icon + temperature; click for the 5-day popup. x/y is the top-left of the whole widget."""
    LABEL_W, GAP = 34, 6
    POPUP_W, POPUP_H = 360, 220

    def create(self) -> None:
        s, app, c = self.spec, self.app, self.app.canvas
        x, y = self.pos()
        self.size = round(self.scaled("size", 46))
        self.cx0, self.cy0 = x + (self.LABEL_W + self.GAP) * app.sf, y
        self.hit = self.tag + "h"
        tags = (self.tag, self.hit, "el")
        accent = app.color(s.get("color"), app.accent)
        text_color = app.color(s.get("text_color"), "#ffffff")
        c.create_oval(self.cx0, y, self.cx0 + self.size, y + self.size, fill=accent, outline="", tags=tags)
        self.emoji = c.create_text(self.cx0 + self.size / 2, y + self.size / 2, text="☀",
                                   font=self._font(18, False), fill=text_color, tags=tags)
        self.temp = c.create_text(x + self.LABEL_W * app.sf, y + self.size / 2, text="--°", font=self._font(11, True),
                                  fill=text_color, anchor="e", tags=tags)
        c.tag_bind(self.hit, "<Button-1>", lambda e: self.toggle_popup())
        self.popup_tag = self.tag + "p"
        self.popup_visible = False
        self.popup_id: int | None = None
        self.day_index = 0
        app.weather.listeners.append(self.on_weather)
        self.on_weather()

    def _font(self, size: int, bold: bool) -> tuple:
        size = max(1, round(size * self.app.sf))
        try:
            tkfont.Font(family="Ubuntu Mono", size=size)
            return ("Ubuntu Mono", size, "bold") if bold else ("Ubuntu Mono", size)
        except Exception:  # noqa: BLE001
            return ("TkDefaultFont", size, "bold") if bold else ("TkDefaultFont", size)

    def destroy(self) -> None:
        super().destroy()
        if self.on_weather in self.app.weather.listeners:
            self.app.weather.listeners.remove(self.on_weather)

    def on_visibility(self, visible: bool) -> None:
        super().on_visibility(visible)
        if not visible:
            self.hide_popup(animate=False)

    def on_weather(self) -> None:
        w = self.app.weather
        c = self.app.canvas
        c.itemconfig(self.temp, text=w.temp)
        if w.current:
            code = str(w.current.get("weather", [{}])[0].get("icon", ""))
            c.itemconfig(self.emoji, text=self.app.weather_icon(code))
        if self.popup_visible:
            self.render_popup()

    def outside_click(self, event: Any) -> None:
        if not self.popup_visible:
            return
        for item in self.app.canvas.find_overlapping(event.x, event.y, event.x, event.y):
            tags = self.app.canvas.gettags(item)
            if self.popup_tag in tags or self.hit in tags:
                return
        self.hide_popup()

    def toggle_popup(self) -> None:
        if self.app.sleeping:
            return
        self.hide_popup() if self.popup_visible else self.show_popup()

    def _popup_box(self) -> tuple[float, float, float, float]:
        pw, ph = self.POPUP_W * self.app.sf, self.POPUP_H * self.app.sf
        right, bottom = self.cx0 + self.size, self.cy0 + self.size
        x2 = min(max(right, pw + 10), self.app.sw - 10)
        if bottom - ph >= 10:
            y1, y2 = bottom - ph, bottom           # open upwards
        else:
            y1 = self.cy0
            y2 = min(y1 + ph, self.app.sh - 10)
            y1 = y2 - ph
        return x2 - pw, y1, x2, y2

    def show_popup(self) -> None:
        if self.popup_visible:
            return
        self.popup_visible = True
        c = self.app.canvas
        x1, y1, x2, y2 = self._popup_box()
        up = y2 >= self.cy0 + self.size - 1
        self.popup_id = c.create_rectangle(x2, y2 if up else y1, x2, y2 if up else y1, fill="#0f1012", outline="",
                                           tags=(self.popup_tag, "el"))
        steps = 8

        def step(i: int) -> None:
            if not self.popup_visible or self.popup_id is None:
                return
            f = i / steps
            if up:
                c.coords(self.popup_id, x2 - (x2 - x1) * f, y2 - (y2 - y1) * f, x2, y2)
            else:
                c.coords(self.popup_id, x2 - (x2 - x1) * f, y1, x2, y1 + (y2 - y1) * f)
            if i < steps:
                self.app.root.after(16, lambda: step(i + 1))
            else:
                self.render_popup()
                self.app.weather.ensure(force=False)

        step(1)

    def hide_popup(self, animate: bool = True) -> None:
        if not self.popup_visible:
            return
        self.popup_visible = False
        c = self.app.canvas
        c.delete(self.popup_tag + "c")
        if not (animate and self.popup_id is not None and c.coords(self.popup_id)):
            c.delete(self.popup_tag)
            return
        x1, y1, x2, y2 = c.coords(self.popup_id)
        pid = self.popup_id

        def step(i: int) -> None:
            f = i / 6
            c.coords(pid, x1 + (x2 - x1) * f, y1 + (y2 - y1) * f, x2, y2)
            if i < 6:
                self.app.root.after(16, lambda: step(i + 1))
            else:
                c.delete(self.popup_tag)

        step(1)

    def _txt(self, x: float, y: float, text: str, fill: str, font: tuple, anchor: str = "nw", extra: str = "") -> None:
        tags = (self.popup_tag, self.popup_tag + "c", "el") + ((extra,) if extra else ())
        self.app.canvas.create_text(x, y, text=text, anchor=anchor, fill=fill, font=font, tags=tags)

    def render_popup(self) -> None:
        app, c, w = self.app, self.app.canvas, self.app.weather
        c.delete(self.popup_tag + "c")
        if self.popup_id is None or not c.coords(self.popup_id):
            return
        x1, y1, x2, y2 = c.coords(self.popup_id)
        pad = 14
        text_c, dim = app.text_color, app.text_dim
        mono = (app.mono_family, round(11 * app.sf))
        if w.error and not w.current:
            self._txt(x1 + pad, y1 + pad, f"Error: {w.error}", "#ff6666", mono)
            return
        now = w.current
        if not now:
            self._txt(x1 + pad, y1 + pad, "Loading weather...", text_c, mono)
            return
        name = now.get("name", "Unknown")
        country = now.get("sys", {}).get("country", "")
        temp = round(now.get("main", {}).get("temp", 0))
        desc = w.description
        icon = app.weather_icon(str(now.get("weather", [{}])[0].get("icon", "")))
        self._txt(x1 + pad, y1 + pad, f"{name}, {country}", text_c, (None, 14, "bold"))
        self._txt(x1 + pad, y1 + pad + 22, f"{temp}°C  {desc}", dim, mono)
        self._txt(x2 - pad - 8, y1 + pad + 10, icon, text_c, (None, 32), anchor="ne")
        groups: dict[Any, list] = {}
        for item in w.forecast or []:
            groups.setdefault(datetime.fromtimestamp(item.get("dt", 0)).date(), []).append(item)
        days = sorted(groups)
        if not days:
            self._txt(x1 + pad, y1 + pad + 56, "No forecast available", dim, mono)
            return
        self.day_index = max(0, min(self.day_index, len(days) - 1))
        sel = days[self.day_index]
        nav_y = y1 + pad + 64
        self._txt(x1 + pad, nav_y, "<", text_c, (None, 18, "bold"), "w", self.popup_tag + "l")
        self._txt(x2 - pad, nav_y, ">", text_c, (None, 18, "bold"), "e", self.popup_tag + "r")
        c.tag_bind(self.popup_tag + "l", "<Button-1>", lambda e: self._day(-1))
        c.tag_bind(self.popup_tag + "r", "<Button-1>", lambda e: self._day(1))
        max_show, gap = 8, max(36, int((x2 - x1 - pad * 2) / 8))
        y_base = nav_y + 18
        for i, it in enumerate(groups[sel][:max_show]):
            cx = x1 + pad + i * gap
            self._txt(cx, y_base, datetime.fromtimestamp(it.get("dt", 0)).strftime("%H:%M"), dim, (None, 10), "n")
            self._txt(cx, y_base + 18, app.weather_icon(str(it.get("weather", [{}])[0].get("icon", ""))), text_c, (None, 14), "n")
            self._txt(cx, y_base + 40, f"{round(it.get('main', {}).get('temp', 0))}°", dim, (None, 11, "bold"), "n")
        self._txt(x1 + pad + 56, nav_y, sel.strftime("%a %d"), text_c, (None, 12, "bold"), "w")

    def _day(self, delta: int) -> None:
        self.day_index = max(0, self.day_index + delta)
        self.render_popup()


class PluginElement(Element):
    """A coded widget from widgets/<type>.py (or `script:`)."""
    _LOG_EVERY = 50  # a widget that fails every tick logs its traceback once, then every 50th time

    def _guard(self, fn: Callable[[], Any], what: str) -> Any:
        try:
            return fn()
        except Exception:  # noqa: BLE001
            self.errors = getattr(self, "errors", 0) + 1
            if self.errors == 1 or self.errors % self._LOG_EVERY == 0:
                log(f"widget {what} failed ({self.errors}x):\n" + traceback.format_exc())
            return None

    def create(self) -> None:
        app, s = self.app, self.spec
        kind = str(s.get("type"))
        self.inst: Any = None
        self.inner: TextElement | None = None
        self.interval = 1.0
        self.next_run = 0.0
        self.busy = False
        path = find_plugin_file(kind, s, app.widget_dirs, app.base_dir)
        if path is None:
            raise RuntimeError(f"unknown element type '{kind}' (no widgets/{kind}.py found)")
        mod = load_plugin_module(path)
        params = {**dict(getattr(mod, "PARAMS", {})), **s}
        self.interval = max(0.05, as_float(params.get("interval"), 1.0))
        self.ctx = PluginContext(app, self, params)
        self.mod = mod
        if hasattr(mod, "Widget"):
            self.inst = mod.Widget(self.ctx)
            self._guard(getattr(self.inst, "create", lambda: None), f"{kind}.create")
            if hasattr(self.inst, "on_click"):
                app.canvas.tag_bind(self.tag, "<Button-1>", lambda e: self._guard(lambda: self.inst.on_click(e), f"{kind}.on_click"))
        elif hasattr(mod, "update"):
            spec2 = dict(s)
            spec2.setdefault("text", "{output}")
            self.inner = TextElement(app, spec2)
            self.inner.tag = self.tag
            self.inner.create()
        else:
            raise RuntimeError(f"{path.name}: define a `Widget` class or an `update(ctx)` function")

    def update(self, now: float) -> None:
        if now < self.next_run or self.busy:
            return
        self.next_run = now + self.interval
        if self.inst is not None:
            if hasattr(self.inst, "update"):
                self._guard(self.inst.update, f"{self.spec.get('type')}.update")
        elif self.inner is not None:
            self.busy = True
            self.app.run_async(lambda: self._guard(lambda: self.mod.update(self.ctx), f"{self.spec.get('type')}.update"), self._got)

    def _got(self, result: Any) -> None:
        self.busy = False
        if self.inner is not None and result is not None:
            self.inner.local["output"] = str(result)
            self.inner.update(time.time())

    def destroy(self) -> None:
        super().destroy()
        if self.inst is not None and hasattr(self.inst, "destroy"):
            self._guard(self.inst.destroy, f"{self.spec.get('type')}.destroy")
        if self.inner is not None:
            self.inner.destroy()


ELEMENT_TYPES: dict[str, type[Element]] = {
    "text": TextElement, "image": ImageElement, "rect": RectElement, "input": InputElement, "weather": WeatherElement,
}


# ── main lock screen ────────────────────────────────────────────────────────
class LockScreen:
    SHAKE_DISTANCE = 14
    SHAKE_STEPS = 8
    SHAKE_INTERVAL = 30  # ms per step
    TICK_MS = 250
    _BLOCKED_KEYSYMS = {"Escape", "F2", "F4", "Super_L", "Super_R", "Meta_L", "Meta_R", "Alt_L", "Alt_R"}

    def __init__(self, root: tk.Tk, cfg: dict, base_dir: Path, layout_path: Path, config_path: Path | None = None,
                 preview: bool = False):
        self.root, self.cfg, self.base_dir = root, cfg, base_dir
        self.layout_path, self.config_path, self.preview = layout_path, config_path, preview
        self.password = ""
        self.locked = True
        self.shaking = False
        self.locked_at = time.time()
        self.sleeping = False
        self.last_activity_at = time.time()
        self.dpms_supported = bool(shutil.which("xset")) and bool(os.environ.get("DISPLAY"))
        self.dpms_sleeping = False
        self.text_color, self.text_dim = "#c8d2e6", "#8a96aa"
        self.elements: list[Element] = []
        self.input: InputElement | None = None
        self.values: dict[str, Any] = {}
        self.var_sources: dict[str, CommandSource] = {}
        self._ui_queue: queue.Queue = queue.Queue()
        self._bg_frames: list[Any] = []
        self._bg_delays: list[int] = []
        self._bg_index = 0
        self._bg_gen = 0           # bumped on every rebuild: cancels carousel timers
        self._bg_anim = 0          # bumped on every image swap: cancels GIF / fade loops
        self._bg_item: int | None = None
        self._bg_last_pil: Any = None
        self._fade_photo: Any = None
        self._carousel_imgs: list[Path] = []
        self._carousel_path: Path | None = None
        self.weather = WeatherService(self)
        self._apply_cfg()

        root.title("ft_lock")
        sw, sh = root.winfo_screenwidth(), root.winfo_screenheight()
        if preview:
            design = (load_layout(layout_path).get("screen") or {})
            dw, dh = as_int(design.get("width"), 1920), as_int(design.get("height"), 1080)
            k = min(0.92 * sw / dw, 0.92 * sh / dh, 1.0)
            sw, sh = int(dw * k), int(dh * k)
            root.geometry(f"{sw}x{sh}+20+20")
            root.title("ft_lock — preview (Esc closes)")
        else:
            root.attributes("-fullscreen", True)
            root.attributes("-topmost", True)
            root.overrideredirect(True)
            root.after(100, self._grab)
        self.sw, self.sh = sw, sh
        self.sx = self.sy = self.sf = 1.0

        self.canvas = tk.Canvas(root, width=sw, height=sh, bd=0, highlightthickness=0, bg=self.bg_color)
        self.canvas.pack(fill="both", expand=True)
        self.overlay = self.canvas.create_rectangle(0, 0, sw, sh, fill="#000000", outline="", state="hidden")

        self._build_scene()

        root.bind("<Key>", self._on_key)
        root.bind("<Return>", self._on_enter)
        root.bind("<BackSpace>", self._on_backspace)
        root.bind("<Motion>", self._on_mouse_motion)
        root.bind("<Button>", self._on_mouse_button)
        root.bind("<Alt-F4>", lambda e: "break")
        root.bind("<Alt-F2>", lambda e: "break")
        root.bind("<Escape>", self._on_escape)
        root.protocol("WM_DELETE_WINDOW", self.close if preview else (lambda: None))
        root.bind("<FocusOut>", lambda e: self.root.after(10, self._restore_window))
        self.canvas.bind("<Button-1>", self._on_canvas_click)

        root.focus_force()
        if not preview:
            self._enforce_window()
            self._check_idle_sleep()
            self._check_logout_timeout()
        self._pump()
        self._tick()
        self._blink()
        if preview:
            self._mtimes = self._stat_files()
            self.root.after(500, self._watch_files)
        self.root.after(200, lambda: self.weather.ensure(force=True))

    # ── config / scene ────────────────────────────────────────────────────
    def _apply_cfg(self) -> None:
        cfg = self.cfg
        lock, disp = cfg["lock"], cfg["display"]
        self.username = str(disp.get("username") or os.environ.get("USER", "user"))
        self.message = str(disp.get("message") or "")
        self.accent = str(disp.get("accent_color") or "#5a7ab0")
        self.bg_color = str(disp.get("background_color") or "#1a0a1e")
        self.font_family = str(disp.get("font_family") or "Ubuntu")
        self.mono_family = "Ubuntu Mono"
        self.password_text = str(lock.get("password_text") or "")
        self.IDLE_SLEEP_SECONDS = max(1, as_int(lock.get("timeout"), 5))
        self.logout_timeout = as_int(lock.get("logout_timeout"), 30)
        self.logout_timeout_force = as_int(lock.get("logout_timeout_force"), 42)
        if self.logout_timeout_force < self.logout_timeout:
            self.logout_timeout_force = self.logout_timeout + 5
        self.allow_escape_logout = as_bool(lock.get("allow_escape_logout"), True)
        self.widget_dirs = widget_dirs(cfg, self.base_dir)
        self.var_sources = {}
        for name, val in (cfg.get("variables") or {}).items():
            if isinstance(val, dict) and val.get("command"):
                self.var_sources[str(name)] = CommandSource(self, str(val["command"]), as_float(val.get("interval"), 10),
                                                            as_bool(val.get("shell")))

    def _build_scene(self) -> None:
        c = self.canvas
        for el in self.elements:
            safe(el.destroy, what="element destroy")
        self.elements, self.input = [], None
        c.delete("el")
        c.delete("bg")
        self._bg_gen += 1
        self._bg_anim += 1
        self._bg_item = None
        c.configure(bg=self.bg_color)
        layout = load_layout(self.layout_path)
        design = layout.get("screen") or {}
        dw, dh = as_int(design.get("width"), 0), as_int(design.get("height"), 0)
        if dw > 0 and dh > 0 and as_bool(self.cfg["display"].get("scale_layout"), True):
            self.sx, self.sy = self.sw / dw, self.sh / dh
            self.sf = min(self.sx, self.sy)
        else:
            self.sx = self.sy = self.sf = 1.0
        self._start_background()
        for spec in layout["elements"]:
            el = self._make_element(spec)
            if el is None:
                continue
            try:
                el.create()
            except Exception as e:  # noqa: BLE001
                log(f"element '{spec.get('id', '?')}' ({spec.get('type')}) skipped: {e}")
                c.delete(el.tag)
                if self.preview:
                    c.create_text(as_float(spec.get("x"), 0) * self.sx, as_float(spec.get("y"), 0) * self.sy,
                                  text=f"⚠ {spec.get('id', '?')}: {e}", anchor="nw", fill="#ff6666",
                                  font=("TkDefaultFont", 10), tags="el")
                continue
            self.elements.append(el)
            if isinstance(el, InputElement) and self.input is None:
                self.input = el
        self.canvas.tag_raise(self.overlay)
        self._update_dots()
        now = time.time()
        for el in self.elements:
            el.shown = not self.item_visible(el.spec)   # force the first visibility sync
            safe(el.tick, now, what="element tick")

    def _make_element(self, spec: dict) -> Element | None:
        if not as_bool(spec.get("enabled", True), True):
            return None
        kind = str(spec.get("type") or "")
        cls = ELEMENT_TYPES.get(kind)
        if cls is None:
            cls = PluginElement
        return cls(self, spec)

    # ── background: single image or carousel ──────────────────────────────
    def _start_background(self) -> None:
        car = self.cfg.get("carousel") or {}
        imgs = carousel_images(self.cfg, self.base_dir) if as_bool(car.get("enabled")) else []
        self._carousel_imgs = imgs
        path: Path | None = None
        if imgs:
            if self._carousel_path not in imgs:      # (a live reload keeps the current image)
                self._carousel_path = pick_carousel(imgs, str(car.get("order", "sequential")), persist=not self.preview)
            path = self._carousel_path
            interval = max(2.0, as_float(car.get("interval"), 60))
            if str(car.get("mode", "interval")).lower() == "interval" and len(imgs) > 1:
                gen = self._bg_gen
                self.root.after(int(interval * 1000), lambda: self._carousel_tick(gen, interval))
        else:
            self._carousel_path = None
            if as_bool(car.get("enabled")):
                log("carousel enabled but no images found (check carousel.folder / carousel.images)")
            image_path = str(self.cfg["display"].get("background_image") or "")
            if image_path:
                path = resolve_path(image_path, self.base_dir, ROOT)
        if path is None:
            return
        if not path.is_file():
            log(f"background image not found: {path}")
            return
        try:
            prepared = self._prepare_bg(path)
        except ImportError:
            try:                                   # no Pillow: PNG/GIF, natural size, no carousel fading
                photo = tk.PhotoImage(file=str(path))
                self._bg_frames, self._bg_delays = [photo], []
                self._bg_item = self.canvas.create_image(0, 0, anchor="nw", image=photo, tags="bg")
                self.canvas.tag_lower("bg")
            except Exception as e:  # noqa: BLE001
                log(f"Could not load image: {e}")
            return
        except Exception as e:  # noqa: BLE001
            log(f"Could not load image {path}: {e}")
            return
        self._install_bg(prepared, fade=False)

    def _prepare_bg(self, path: Path) -> tuple[list, list[int]]:
        """Decode + fit the image to the screen. Pure Pillow work: safe to run in a thread."""
        from PIL import Image, ImageSequence
        sw, sh = self.sw, self.sh
        fit = str(self.cfg["display"].get("background_fit") or "stretch").lower()
        bg = self.bg_color

        def fitted(im: Any) -> Any:
            im = im.convert("RGBA")
            if fit == "stretch":
                return im.resize((sw, sh), Image.LANCZOS)
            k = (max if fit == "cover" else min)(sw / im.width, sh / im.height)
            im = im.resize((max(1, round(im.width * k)), max(1, round(im.height * k))), Image.LANCZOS)
            try:
                canvas = Image.new("RGBA", (sw, sh), bg)
            except ValueError:
                canvas = Image.new("RGBA", (sw, sh), "#000000")
            canvas.paste(im, ((sw - im.width) // 2, (sh - im.height) // 2))
            return canvas

        img = Image.open(path)
        frames: list = []
        delays: list[int] = []
        if getattr(img, "is_animated", False) or getattr(img, "n_frames", 1) > 1:
            for frame in ImageSequence.Iterator(img):
                delays.append(max(20, int(frame.info.get("duration", 100))))
                frames.append(fitted(frame))
        else:
            frames.append(fitted(img))
        return frames, delays

    def _install_bg(self, prepared: tuple[list, list[int]], fade: bool) -> None:
        """Show prepared frames (Tk thread). Cross-fades from the previous image when asked."""
        from PIL import Image, ImageTk
        frames, delays = prepared
        if not frames:
            return
        photos = [ImageTk.PhotoImage(f) for f in frames]
        old = self._bg_last_pil
        self._bg_anim += 1
        token = self._bg_anim
        self._bg_frames, self._bg_delays, self._bg_index = photos, delays, 0
        self._bg_last_pil = frames[0]
        if self._bg_item is None:
            self._bg_item = self.canvas.create_image(0, 0, anchor="nw", image=photos[0], tags="bg")
            self.canvas.tag_lower("bg")
            fade = False
        if fade and old is not None and old.size == frames[0].size:
            steps = 8

            def step(i: int) -> None:
                if token != self._bg_anim or self._bg_item is None:
                    return
                if i >= steps:
                    self._fade_photo = None
                    self.canvas.itemconfig(self._bg_item, image=photos[0])
                    self._begin_bg_anim(token)
                    return
                self._fade_photo = ImageTk.PhotoImage(Image.blend(old, frames[0], i / steps))
                self.canvas.itemconfig(self._bg_item, image=self._fade_photo)
                self.root.after(40, lambda: step(i + 1))

            step(1)
        else:
            self.canvas.itemconfig(self._bg_item, image=photos[0])
            self._begin_bg_anim(token)

    def _begin_bg_anim(self, token: int) -> None:
        if len(self._bg_frames) > 1 and self._bg_delays:
            self.root.after(self._bg_delays[0], lambda: self._animate_bg(token))

    def _animate_bg(self, token: int) -> None:
        """Advance animated GIF background frames."""
        if token != self._bg_anim or not self.locked or self._bg_item is None:
            return
        self._bg_index = (self._bg_index + 1) % len(self._bg_frames)
        self.canvas.itemconfig(self._bg_item, image=self._bg_frames[self._bg_index])
        self.root.after(self._bg_delays[self._bg_index], lambda: self._animate_bg(token))

    def _carousel_tick(self, gen: int, interval: float) -> None:
        if gen != self._bg_gen or not self.locked:
            return
        if not self.sleeping and len(self._carousel_imgs) > 1:
            self._carousel_advance()
        self.root.after(int(interval * 1000), lambda: self._carousel_tick(gen, interval))

    def _carousel_advance(self) -> None:
        imgs = self._carousel_imgs
        car = self.cfg.get("carousel") or {}
        cur = imgs.index(self._carousel_path) if self._carousel_path in imgs else -1
        if str(car.get("order", "sequential")).lower() == "random":
            idx = random.choice([i for i in range(len(imgs)) if i != cur])
        else:
            idx = (cur + 1) % len(imgs)
        path = imgs[idx]
        self._carousel_path = path
        gen = self._bg_gen
        fade = str(car.get("transition", "fade")).lower() == "fade"

        def done(result: Any) -> None:     # decoding happened in a thread: only the swap is on the Tk thread
            if result and gen == self._bg_gen and path == self._carousel_path:
                self._install_bg(result, fade)

        self.run_async(lambda: self._prepare_bg(path), done)

    # ── preview live reload ───────────────────────────────────────────────
    def _stat_files(self) -> tuple:
        out = []
        for p in [self.layout_path, self.config_path, *self.widget_dirs]:
            try:
                if p and p.is_dir():
                    out.append(tuple(sorted((f.name, f.stat().st_mtime) for f in p.glob("*.py"))))
                elif p:
                    out.append(p.stat().st_mtime)
            except OSError:
                out.append(None)
        return tuple(out)

    def _watch_files(self) -> None:
        if not self.locked:
            return
        now = self._stat_files()
        if now != self._mtimes:
            self._mtimes = now
            log("change detected — reloading")
            if self.config_path:
                cfg, _, _ = load_config(str(self.config_path))
                self.cfg = cfg
            _PLUGIN_CACHE.clear()
            self._apply_cfg()
            self._build_scene()
        self.root.after(500, self._watch_files)

    # ── helpers ───────────────────────────────────────────────────────────
    def color(self, value: Any, default: str) -> str:
        v = value.strip() if isinstance(value, str) and value.strip() else default
        return self.accent if v.lower() in ("accent", "$accent") else v

    def weather_icon(self, code: str) -> str:
        return {"01": "☀", "02": "⛅", "03": "☁", "04": "☁", "09": "🌧", "10": "🌦", "11": "⛈", "13": "❄",
                "50": "🌫"}.get(code[:2], "🌡")

    def item_visible(self, spec: dict) -> bool:
        if not as_bool(spec.get("enabled", True), True):
            return False
        cond = spec.get("visible", spec.get("condition", "always"))
        if cond is False:
            return False
        elapsed = time.time() - self.locked_at
        if cond == "after_logout_timeout":
            return elapsed >= self.logout_timeout * 60
        if cond == "after_force_timeout":
            return elapsed >= self.logout_timeout_force * 60
        if cond == "after_seconds":
            return elapsed >= as_float(spec.get("seconds"), 0)
        if cond == "before_seconds":
            return elapsed < as_float(spec.get("seconds"), 0)
        return True

    def draw_rounded_rect(self, x1: float, y1: float, x2: float, y2: float, r: float, **kwargs: Any) -> int:
        r = min(r, (x2 - x1) / 2, (y2 - y1) / 2)
        return self.canvas.create_polygon(
            x1 + r, y1, x2 - r, y1, x2, y1, x2, y1 + r, x2, y2 - r, x2, y2, x2 - r, y2, x1 + r, y2,
            x1, y2, x1, y2 - r, x1, y1 + r, x1, y1, smooth=True, **kwargs)

    def call_soon(self, fn: Callable[[], None]) -> None:
        self._ui_queue.put(fn)

    def run_async(self, fn: Callable[[], Any], callback: Callable[[Any], None] | None = None) -> None:
        def worker() -> None:
            result = safe(fn, what="background task")
            if callback:
                self.call_soon(lambda: safe(callback, result, what="background callback"))
        threading.Thread(target=worker, daemon=True).start()

    def _pump(self) -> None:
        try:
            while True:
                self._ui_queue.get_nowait()()
        except queue.Empty:
            pass
        except Exception:  # noqa: BLE001
            log(traceback.format_exc())
        if self.locked:
            self.root.after(40, self._pump)

    def template_values(self) -> dict[str, Any]:
        now = datetime.now()
        elapsed = time.time() - self.locked_at
        try:
            clock = time.strftime("%I:%M %p").lstrip("0")
        except Exception:  # noqa: BLE001
            clock = time.strftime("%H:%M:%S")
        v: dict[str, Any] = {
            "username": self.username, "message": self.message, "elapsed": self._format_elapsed(elapsed),
            "clock": clock, "clock24": now.strftime("%H:%M"), "time": now.strftime("%H:%M:%S"),
            "date": now.strftime("%a %d %b"), "date_long": now.strftime("%A %d %B %Y"), "year": now.year,
            "hostname": socket.gethostname(), "city": str(self.cfg["weather"].get("city") or ""),
            "temp": self.weather.temp, "weather": self.weather.description,
            "locked_seconds": int(elapsed), "locked_minutes": int(elapsed // 60),
            "background": self._carousel_path.name if self._carousel_path else "",
        }
        for name, val in (self.cfg.get("variables") or {}).items():
            if str(name) in self.var_sources:
                src = self.var_sources[str(name)]
                src.poll(time.time())
                v[str(name)] = src.value
            elif not isinstance(val, dict):
                v[str(name)] = val
        return v

    def _tick(self) -> None:
        if not self.locked:
            return
        self.values = self.template_values()
        now = time.time()
        for el in self.elements:
            safe(el.tick, now, what=f"element {el.spec.get('id', '?')}")
        self.root.after(self.TICK_MS, self._tick)

    def _blink(self) -> None:
        if not self.locked:
            return
        if self.input:
            self.input.blink()
        self.root.after(530, self._blink)

    def close(self) -> None:
        self.locked = False
        try:
            self.root.destroy()
        except Exception:  # noqa: BLE001
            pass

    # ── window grabbing ───────────────────────────────────────────────────
    def _grab(self) -> None:
        """Grab all input — must be called after the window is viewable."""
        try:
            self.root.grab_set_global()
        except tk.TclError:
            self.root.after(100, self.root.grab_set_global)

    def _restore_window(self) -> None:
        if self.preview or not self.locked:
            return
        try:
            self.root.attributes("-fullscreen", True)
            self.root.attributes("-topmost", True)
            self.root.overrideredirect(True)
            self.root.lift()
            self.root.focus_force()
            self.root.grab_set_global()
        except tk.TclError:
            pass

    def _enforce_window(self) -> None:
        if self.locked:
            self._restore_window()
            self.root.after(250, self._enforce_window)

    # ── sleep / dpms / logout ─────────────────────────────────────────────
    def _mark_activity(self) -> None:
        self.last_activity_at = time.time()
        if self.sleeping:
            self._wake_from_sleep()

    def _enter_sleep(self) -> None:
        if self.sleeping or not self.locked:
            return
        self.sleeping = True
        if self.dpms_supported and self._dpms_force("off"):
            self._dpms_set_high_timeout()
            self.dpms_sleeping = True
            return
        self.dpms_sleeping = False
        self.canvas.itemconfig(self.overlay, state="normal")
        self.canvas.tag_raise(self.overlay)

    def _wake_from_sleep(self) -> None:
        if not self.sleeping:
            return
        self.sleeping = False
        if self.dpms_sleeping:
            self._dpms_force("on")
            self._dpms_restore_timeout()
            self.dpms_sleeping = False
            return
        self.canvas.itemconfig(self.overlay, state="hidden")
        self._update_dots()

    def _check_idle_sleep(self) -> None:
        if not self.locked:
            return
        if not self.sleeping and (time.time() - self.last_activity_at) >= self.IDLE_SLEEP_SECONDS:
            self._enter_sleep()
        self.root.after(300, self._check_idle_sleep)

    def _check_logout_timeout(self) -> None:
        if not self.locked:
            return
        if time.time() - self.locked_at >= self.logout_timeout_force * 60:
            self._perform_logout()
            return
        self.root.after(1000, self._check_logout_timeout)

    def _perform_logout(self) -> None:
        """Log out the user via gnome-session-quit."""
        try:
            subprocess.run(["gnome-session-quit", "--logout", "--no-prompt"], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
        except Exception as e:  # noqa: BLE001
            print(f"Could not perform logout: {e}", file=sys.stderr)
        finally:
            self.locked = False
            try:
                self.root.destroy()
            except Exception:  # noqa: BLE001
                pass

    def _xset(self, *args: str) -> bool:
        try:
            subprocess.run(["xset", *args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2)
            return True
        except Exception:  # noqa: BLE001
            return False

    def _dpms_force(self, mode: str) -> bool:
        return self._xset("dpms", "force", mode)

    def _dpms_set_high_timeout(self) -> bool:
        """Set DPMS timeouts very high to keep display off while sleeping."""
        return self._xset("dpms", "9999", "9999", "9999")

    def _dpms_restore_timeout(self) -> bool:
        """Restore normal DPMS timeouts after waking."""
        return self._xset("dpms", "600", "600", "600")

    # ── password box ──────────────────────────────────────────────────────
    def _update_dots(self) -> None:
        if not self.input:
            return
        if self.password_text:
            typed = len(self.password)
            shown = (self.password_text[:typed] if typed <= len(self.password_text)
                     else self.password_text + "●" * (typed - len(self.password_text)))
        else:
            shown = "●" * len(self.password)
        self.input.set_text(shown)

    def _format_elapsed(self, seconds: float) -> str:
        total = max(0, int(seconds))
        if total < 60:
            return "a few seconds ago…"
        minutes, secs = divmod(total, 60)
        hours, mins = divmod(minutes, 60)
        if mins > 40:
            return f"{mins % 10 + 30} minutes ago…"
        if mins == 1:
            return "1 minute ago"
        return f"{mins} minutes ago…"

    # ── key handlers ──────────────────────────────────────────────────────
    def _on_key(self, event: Any) -> str:
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping or self.shaking:
            return "break"
        if event.keysym in self._BLOCKED_KEYSYMS:
            return "break"
        if bool(event.state & 0x0008):  # Alt held
            return "break"
        ch = event.char
        if ch and ch.isprintable():
            self.password += ch
            self._update_dots()
            if self.input:
                self.input.set_status("")
        return "break"

    def _on_backspace(self, event: Any) -> str:
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping or self.shaking:
            return "break"
        self.password = self.password[:-1]
        self._update_dots()
        return "break"

    def _on_enter(self, event: Any) -> str:
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping or self.shaking:
            return "break"
        self._try_unlock()
        return "break"

    def _on_mouse_motion(self, event: Any) -> str:
        self._mark_activity()
        return "break"

    def _on_mouse_button(self, event: Any) -> str:
        self._mark_activity()
        return "break"

    def _on_canvas_click(self, event: Any) -> None:
        for el in self.elements:
            if isinstance(el, WeatherElement):
                el.outside_click(event)

    def _on_escape(self, event: Any) -> str:
        was_sleeping = self.sleeping
        self._mark_activity()
        if was_sleeping:
            return "break"
        if self.preview:
            self.close()
        elif self.allow_escape_logout and time.time() - self.locked_at >= self.logout_timeout * 60:
            self._perform_logout()
        return "break"

    def _try_unlock(self) -> None:
        pwd = self.password
        self.password = ""
        self._update_dots()
        self.run_async(lambda: authenticate(self.username, pwd), lambda ok: self._auth_result(bool(ok)))

    def _auth_result(self, success: bool) -> None:
        if success:
            self.locked = False
            try:
                self.root.grab_release()
            except tk.TclError:
                pass
            self.root.destroy()
        else:
            if self.input:
                self.input.set_status("Wrong password")
            self._shake()

    # ── shake animation ───────────────────────────────────────────────────
    def _shake(self) -> None:
        if not self.input:
            return
        self.shaking = True
        offsets: list[float] = []
        d = self.SHAKE_DISTANCE * self.sf
        for i in range(self.SHAKE_STEPS):
            offsets.append(d if i % 2 == 0 else -d)
            d = max(2, d - 2)
        offsets.append(0)
        self._shake_step(offsets, 0.0)

    def _shake_step(self, offsets: list[float], current: float) -> None:
        if not offsets or not self.input:
            self.shaking = False
            return
        new = offsets[0]
        self.input.shift(new - current)
        self.root.after(self.SHAKE_INTERVAL, lambda: self._shake_step(offsets[1:], new))


# ── command line helpers ────────────────────────────────────────────────────
WIDGET_SIMPLE = '''\
# widgets/{name}.py — simple widget: `update(ctx)` returns the text to display.
# Use it in a layout with:   - {{id: my_{name}, type: {name}, x: 100, y: 100, color: "#ffffff", size: 18}}
# update() runs in a BACKGROUND THREAD every `interval` seconds: don't touch Tk here, just return a string.
LABEL = "{name}"
DESCRIPTION = "What this widget shows"
SIZE = (160, 24)                 # box shown in the layout editor
PARAMS = {{"interval": 5}}         # default parameters (editable in the editor, overridable per element)


def update(ctx):
    return "hello from {name}"
'''

WIDGET_CLASS = '''\
# widgets/{name}.py — drawing widget: a `Widget` class that draws on the lock screen canvas.
# Use it in a layout with:   - {{id: my_{name}, type: {name}, x: 100, y: 100}}
# Drawing helpers take coordinates RELATIVE to the widget's x/y: ctx.text, ctx.rect, ctx.oval, ctx.line,
# ctx.polygon, ctx.image.  Slow work: ctx.run(fn, callback).  Params: ctx.param("name", default).
LABEL = "{name}"
DESCRIPTION = "What this widget draws"
SIZE = (120, 120)
PARAMS = {{"interval": 1, "color": "#c8d2e6"}}


class Widget:
    def __init__(self, ctx):
        self.ctx = ctx

    def create(self):                       # called once: draw the initial items
        ctx = self.ctx
        ctx.rect(0, 0, 120, 120, outline=ctx.color)
        self.label = ctx.text(60, 60, "0", anchor="center")
        self.count = 0

    def update(self):                       # called every `interval` seconds on the Tk thread (keep it fast)
        self.count += 1
        self.ctx.itemconfig(self.label, text=str(self.count))

    def on_click(self, event):              # optional
        pass
'''


def cmd_check(cfg: dict, base: Path, cfg_path: Path | None, layout_path: Path) -> int:
    problems = warnings = 0
    print(f"config : {cfg_path or '(none found — built-in defaults)'}")
    print(f"layout : {layout_path}{'' if layout_path.is_file() else '   <-- NOT FOUND, built-in layout used'}")
    dirs = widget_dirs(cfg, base)
    plugins = list_widgets(dirs)
    print("widgets: " + (", ".join(p["name"] + (" (ERROR)" if p["error"] else "") for p in plugins) or "(none)"))
    for p in plugins:
        if p["error"]:
            print(f"  ERROR widget {p['name']}: {p['error']}")
            problems += 1
    car = cfg.get("carousel") or {}
    if as_bool(car.get("enabled")):
        imgs = carousel_images(cfg, base)
        mode, order, trans = (str(car.get(k, d)).lower() for k, d in (("mode", "interval"), ("order", "sequential"), ("transition", "fade")))
        print(f"carousel: {len(imgs)} image(s), mode={mode}, order={order}, transition={trans}"
              + (f", every {as_float(car.get('interval'), 60):g}s" if mode == "interval" else ""))
        if not imgs:
            print("  ERROR carousel enabled but no images found (carousel.folder / carousel.images)"); problems += 1
        for key, val, ok in (("mode", mode, ("launch", "interval")), ("order", order, ("sequential", "random")),
                             ("transition", trans, ("fade", "none"))):
            if val not in ok:
                print(f"  ERROR carousel.{key} '{val}' (use {' or '.join(ok)})"); problems += 1
        if mode == "interval" and as_float(car.get("interval"), 60) < 2:
            print("  WARN  carousel.interval below 2 s is raised to 2 s"); warnings += 1
        try:
            import PIL  # noqa: F401
        except ImportError:
            print("  WARN  Pillow missing (sudo apt install python3-pil): the carousel cannot switch images"); warnings += 1
    layout = load_layout(layout_path)
    seen: set[str] = set()
    inputs = 0
    for i, spec in enumerate(layout["elements"]):
        eid, kind = str(spec.get("id", f"#{i}")), str(spec.get("type", ""))
        where = f"element '{eid}'"
        if eid in seen:
            print(f"  WARN  {where}: duplicate id"); warnings += 1
        seen.add(eid)
        if kind not in BUILTIN_TYPES and find_plugin_file(kind, spec, dirs, base) is None:
            print(f"  ERROR {where}: unknown type '{kind}' (builtin: {', '.join(BUILTIN_TYPES)}; or widgets/{kind}.py)")
            problems += 1
        if kind == "input":
            inputs += 1
        if "x" not in spec or "y" not in spec:
            print(f"  WARN  {where}: missing x/y"); warnings += 1
        if "anchor" in spec and norm_anchor(spec["anchor"], "?") == "?":
            print(f"  WARN  {where}: bad anchor '{spec['anchor']}' (use {', '.join(ANCHORS)})"); warnings += 1
        cond = spec.get("visible", spec.get("condition", "always"))
        if cond not in VISIBLE_CONDITIONS and cond is not False:
            print(f"  ERROR {where}: bad visible '{cond}' (use {', '.join(VISIBLE_CONDITIONS)})"); problems += 1
        for key in ("color", "bg", "border", "text_color"):
            val = spec.get(key)
            if isinstance(val, str) and val.startswith("#") and not re.fullmatch(r"#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})", val):
                print(f"  ERROR {where}: bad {key} '{val}'"); problems += 1
        if kind == "image":
            p = resolve_path(spec.get("path", ""), layout_path.parent, base, ROOT)
            if not p.is_file():
                print(f"  WARN  {where}: image not found: {p}"); warnings += 1
    if inputs == 0:
        print("  WARN  no `input` element: nothing will show while you type your password"); warnings += 1
    if inputs > 1:
        print("  WARN  several `input` elements: only the first one is used"); warnings += 1
    print(f"{len(layout['elements'])} elements, {problems} error(s), {warnings} warning(s)")
    return 1 if problems else 0


def cmd_init(target: str | None) -> int:
    dest = Path(target).expanduser() if target else USER_DIR
    dest.mkdir(parents=True, exist_ok=True)
    cfg_text = (ROOT / "config.yaml").read_text(encoding="utf-8")
    cfg_text = re.sub(r"(?m)^layout:.*$", "layout: layout.yaml", cfg_text, count=1)
    plan = [("config.yaml", cfg_text), ("layout.yaml", (ROOT / "default.yaml").read_text(encoding="utf-8"))]
    for name, text in plan:
        p = dest / name
        if p.exists():
            print(f"keep   {p} (already exists)")
        else:
            p.write_text(text, encoding="utf-8")
            print(f"wrote  {p}")
    (dest / "widgets").mkdir(exist_ok=True)
    for src in sorted((ROOT / "widgets").glob("*.py")):
        p = dest / "widgets" / src.name
        if not p.exists():
            shutil.copy(src, p)
            print(f"wrote  {p}")
    print(f"\nNext: python3 {Path(__file__).name} --preview --config {dest / 'config.yaml'}")
    return 0


def cmd_new_widget(name: str, as_class: bool, cfg: dict, base: Path) -> int:
    if not _NAME_OK.match(name):
        print("widget names may only contain letters, digits, - and _")
        return 1
    d = resolve_path(cfg.get("widgets_dir", "widgets"), base, ROOT)
    d.mkdir(parents=True, exist_ok=True)
    p = d / f"{name}.py"
    if p.exists():
        print(f"{p} already exists")
        return 1
    p.write_text((WIDGET_CLASS if as_class else WIDGET_SIMPLE).format(name=name), encoding="utf-8")
    print(f"created {p}\nuse it in your layout:  - {{id: my_{name}, type: {name}, x: 100, y: 100}}")
    return 0


def cmd_edit(cfg_path: Path | None, layout_path: Path) -> int:
    proj = ROOT / "layout_editor"
    built = proj / "target" / "release" / "ft-lock-layout-editor"
    args = [str(layout_path), "--project", str(ROOT)]
    if cfg_path:
        args += ["--config", str(cfg_path)]
    if built.is_file():
        cmd = [str(built), *args]
    elif shutil.which("cargo"):
        cmd = ["cargo", "run", "--release", "--manifest-path", str(proj / "Cargo.toml"), "--", *args]
    else:
        print("The layout editor needs Rust: install it from https://rustup.rs, then run again "
              "(or `cd layout_editor && cargo build --release`).")
        return 1
    return subprocess.run(cmd, check=False).returncode


def main() -> int:
    ap = argparse.ArgumentParser(description="ft_lock — customizable Python lock screen")
    ap.add_argument("--config", help="path to config.yaml (default: ~/.config/ft_lock/, then next to this script)")
    ap.add_argument("--layout", help="layout file (overrides `layout:` in the config)")
    ap.add_argument("--preview", action="store_true", help="windowed, live-reloading, never grabs input")
    ap.add_argument("--edit", action="store_true", help="open the Rust layout editor")
    ap.add_argument("--check", action="store_true", help="validate config + layout")
    ap.add_argument("--list-widgets", action="store_true", help="print available widget plugins as JSON")
    ap.add_argument("--init", nargs="?", const="", metavar="DIR", help="write a starter config/layout/widgets")
    ap.add_argument("--new-widget", metavar="NAME", help="create widgets/NAME.py from a template")
    ap.add_argument("--class", dest="class_mode", action="store_true", help="with --new-widget: drawing-widget template")
    ap.add_argument("--image", default="", help="background image (overrides config)")
    ap.add_argument("--user", default="", help="username to display (overrides config)")
    ap.add_argument("--message", default=None, help="message under the username (overrides config)")
    args = ap.parse_args()

    if args.init is not None:
        return cmd_init(args.init or None)

    cfg, base, cfg_path = load_config(args.config)
    if args.image:
        cfg["display"]["background_image"] = args.image
    if args.user:
        cfg["display"]["username"] = args.user
    if args.message is not None:
        cfg["display"]["message"] = args.message
    layout_path = resolve_path(args.layout or cfg["layout"], base, ROOT)

    if args.list_widgets:
        print(json.dumps(list_widgets(widget_dirs(cfg, base))))
        return 0
    if args.new_widget:
        return cmd_new_widget(args.new_widget, args.class_mode, cfg, base)
    if args.check:
        return cmd_check(cfg, base, cfg_path, layout_path)
    if args.edit:
        return cmd_edit(cfg_path, layout_path)

    try:
        root = tk.Tk()
    except tk.TclError as e:
        print(f"Cannot open the display: {e}", file=sys.stderr)
        return 1
    LockScreen(root, cfg, base, layout_path, cfg_path, preview=args.preview)
    root.mainloop()
    return 0


if __name__ == "__main__":
    sys.exit(main())