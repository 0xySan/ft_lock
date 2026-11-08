# widgets/analog_clock.py — DRAWING widget: an analog clock.
# Layout:  - {id: clock2, type: analog_clock, x: 1600, y: 120, size: 160, color: "#c8d2e6", accent_color: "#5a7ab0"}
# Params:  size (diameter in px), color (face + hands), accent_color (second hand)
import math
import time

LABEL = "Analog clock"
DESCRIPTION = "A round analog clock drawn with canvas lines"
SIZE = (160, 160)
PARAMS = {"size": 160, "color": "#c8d2e6", "accent_color": "#5a7ab0", "interval": 1}


class Widget:
    def __init__(self, ctx):
        self.ctx = ctx
        self.d = int(ctx.param("size", 160) * ctx.scale)
        self.r = self.d / 2

    def create(self):
        c, r = self.ctx, self.r
        c.oval(0, 0, self.d, self.d, outline=c.color, width=2)
        for i in range(12):
            a = math.radians(i * 30)
            c.line(r + math.sin(a) * r * 0.86, r - math.cos(a) * r * 0.86,
                   r + math.sin(a) * r * 0.94, r - math.cos(a) * r * 0.94, fill=c.color, width=2)
        self.hour = c.line(r, r, r, r, fill=c.color, width=4, capstyle="round")
        self.minute = c.line(r, r, r, r, fill=c.color, width=3, capstyle="round")
        self.second = c.line(r, r, r, r, fill=c.param("accent_color", "#5a7ab0"), width=1)
        self.update()

    def _hand(self, item, turns, length):
        a = turns * 2 * math.pi
        self.ctx.coords(item, self.r, self.r, self.r + math.sin(a) * self.r * length, self.r - math.cos(a) * self.r * length)

    def update(self):
        t = time.localtime()
        self._hand(self.hour, ((t.tm_hour % 12) + t.tm_min / 60) / 12, 0.5)
        self._hand(self.minute, (t.tm_min + t.tm_sec / 60) / 60, 0.75)
        self._hand(self.second, t.tm_sec / 60, 0.85)
