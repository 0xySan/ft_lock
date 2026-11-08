# widgets/battery.py — SIMPLE widget: update(ctx) returns the text to show.
# Layout:  - {id: battery, type: battery, x: 40, y: 1040, color: "#c8d2e6", size: 14}
# Params:  label (default "🔋"), interval (seconds, default 10)
from pathlib import Path

LABEL = "Battery"
DESCRIPTION = "Battery level from /sys/class/power_supply (hidden text on desktops)"
SIZE = (90, 22)
PARAMS = {"interval": 10, "label": "🔋", "size": 14}


def update(ctx):                       # runs in a background thread: just return a string
    for bat in sorted(Path("/sys/class/power_supply").glob("BAT*")):
        try:
            level = (bat / "capacity").read_text().strip()
            status = (bat / "status").read_text().strip()
            plug = "⚡" if status == "Charging" else ""
            return f"{ctx.param('label', '🔋')} {level}%{plug}"
        except OSError:
            continue
    return ""
