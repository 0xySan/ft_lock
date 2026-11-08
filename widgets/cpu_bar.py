# widgets/cpu_bar.py — DRAWING widget: a `Widget` class that draws on the canvas.
# Layout:  - {id: cpu, type: cpu_bar, x: 40, y: 980, width: 220, height: 14, color: "#5a7ab0"}
# Params:  width, height, color (bar), bg (track), interval (seconds), label
import os
import time

LABEL = "CPU bar"
DESCRIPTION = "Live CPU usage bar (reads /proc/stat)"
SIZE = (220, 34)
PARAMS = {"width": 220, "height": 14, "bg": "#1a2840", "interval": 1, "label": "CPU", "size": 11}


def _cpu_times():
    with open("/proc/stat") as f:
        parts = f.readline().split()[1:]
    vals = [int(v) for v in parts]
    return sum(vals), vals[3] + (vals[4] if len(vals) > 4 else 0)   # total, idle


class Widget:
    def __init__(self, ctx):
        self.ctx = ctx
        self.w = int(ctx.param("width", 220) * ctx.scale)
        self.h = int(ctx.param("height", 14) * ctx.scale)
        self.prev = None

    def create(self):
        c = self.ctx
        self.label = c.text(0, 0, f"{c.param('label', 'CPU')}  0%", fill=c.color, font=c.font())
        top = c.size + 8
        c.rect(0, top, self.w, top + self.h, fill=c.param("bg", "#1a2840"), outline="")
        self.bar = c.rect(0, top, 0, top + self.h, fill=c.color, outline="")
        self.top = top

    def update(self):
        try:
            total, idle = _cpu_times()
        except OSError:
            return
        if self.prev:
            dt, di = total - self.prev[0], idle - self.prev[1]
            pct = 0 if dt <= 0 else max(0, min(100, round(100 * (1 - di / dt))))
            self.ctx.coords(self.bar, 0, self.top, self.w * pct / 100, self.top + self.h)
            self.ctx.itemconfig(self.label, text=f"{self.ctx.param('label', 'CPU')}  {pct}%")
        self.prev = (total, idle)
