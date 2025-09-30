#!/usr/bin/env python3
"""
restrict_keys.py

Provides a function `disable_keys()` that blocks all keys except:
0-9, A-Z, Enter, Backspace, Shift, CapsLock.
Safe to run in a separate thread for use with pygame locks.
"""

import sys
import time
import threading
from evdev import InputDevice, list_devices, ecodes, UInput

ALLOWED = set()
for d in "0123456789":
    ALLOWED.add(getattr(ecodes, f"KEY_{d}", None))
    ALLOWED.add(getattr(ecodes, f"KEY_KP{d}", None))
for ch in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
    ALLOWED.add(getattr(ecodes, f"KEY_{ch}", None))
for special in ["KEY_ENTER", "KEY_BACKSPACE", "KEY_LEFTSHIFT", "KEY_RIGHTSHIFT", "KEY_CAPSLOCK"]:
    ALLOWED.add(getattr(ecodes, special, None))
ALLOWED.discard(None)

_stop_event = threading.Event()
_threads = []
_grabbed_devices = []
_uinput = None

def find_keyboard_devices():
    devices = []
    for path in list_devices():
        try:
            dev = InputDevice(path)
            caps = dev.capabilities()
        except Exception:
            continue
        if ecodes.EV_KEY in caps:
            key_codes = set(caps[ecodes.EV_KEY])
            if ecodes.KEY_A in key_codes or ecodes.KEY_1 in key_codes:
                devices.append(dev)
    return devices

def _reader_loop(dev, uinput_dev):
    try:
        for ev in dev.read_loop():
            if _stop_event.is_set():
                break
            if ev.type == ecodes.EV_KEY and ev.code in ALLOWED:
                try:
                    uinput_dev.write(ev.type, ev.code, ev.value)
                    uinput_dev.syn()
                except Exception:
                    pass
    except Exception:
        return

def _cleanup():
    global _grabbed_devices, _uinput, _threads
    _stop_event.set()
    for d in list(_grabbed_devices):
        try: d.ungrab()
        except Exception: pass
    _grabbed_devices = []
    if _uinput:
        try: _uinput.close()
        except Exception: pass
        _uinput = None
    for t in list(_threads):
        try:
            if t.is_alive(): t.join(timeout=0.2)
        except Exception: pass
    _threads = []

def disable_keys():
    """
    Grab all keyboard devices and forward only allowed keys.
    Blocks until process exits (or killed). Safe to run in a thread.
    """
    global _grabbed_devices, _uinput, _threads

    if not sys.platform.startswith("linux"):
        raise RuntimeError("Only Linux is supported.")

    keyboards = find_keyboard_devices()
    if not keyboards:
        raise RuntimeError("No keyboard devices found.")

    grabbed = []
    try:
        for d in keyboards:
            d.grab()
            grabbed.append(d)
    except Exception as e:
        for dd in grabbed:
            try: dd.ungrab()
            except Exception: pass
        raise RuntimeError(f"Failed to grab device: {e}")
    _grabbed_devices = grabbed

    _uinput = UInput(events={ecodes.EV_KEY: list(ALLOWED)}, name="restrict_keys")

    for d in grabbed:
        t = threading.Thread(target=_reader_loop, args=(d, _uinput), daemon=True)
        t.start()
        _threads.append(t)

    try:
        while True:
            time.sleep(0.5)
    finally:
        _cleanup()
