# ft_lock

A fullscreen lock screen for Ubuntu that you can fully customize: every piece of the screen is a
YAML entry, you can arrange them visually in a Rust editor, and you can add your own widgets in Python.

```
config.yaml      settings: timeouts, colours, background, weather key, your own {variables}
default.yaml     layout: what is shown and where (edited by hand or in the editor)
widgets/*.py     your coded widgets (one file each)
ft_lock.py       the lock screen
layout_editor/   Rust (egui) visual editor
```

## Quick start

```bash
sudo apt install python3-yaml python3-tk python3-pam python3-pil   # yaml/tk/pam/pillow
python3 ft_lock.py --init              # copies config, layout and example widgets to ~/.config/ft_lock/
python3 ft_lock.py --check             # validates everything, prints errors with the element id
python3 ft_lock.py --preview           # a window that behaves like the lock but never grabs input (Esc closes)
python3 ft_lock.py --edit              # visual editor (needs Rust: https://rustup.rs)
python3 ft_lock.py                     # lock the screen for real
```

Config is looked up in `~/.config/ft_lock/config.yaml`, then next to `ft_lock.py`. `--config FILE` overrides it.
The old `ft_lock.cfg` key=value file is still read when no `config.yaml` exists.

## Visual editor

- **Canvas**: drag elements, drag the white handles to resize, Shift+click or drag a box to multi-select,
  arrow keys nudge (Shift = 10 px), smart guides and grid snapping (hold Alt to disable),
  Ctrl+scroll zooms, Space or middle-drag pans.
- **Right-click** the canvas for quick actions; **drop an image or GIF** onto it to add it, drop a `.py`
  widget on the Library tab to install it, drop a `.yaml` layout to open it.
- **Layers**: drag rows to reorder, eye/lock icons hide or lock elements in the editor only.
- **Library**: built-in elements, presets (clock, date, command output) and every widget in `widgets/`.
- **Config** tab edits `config.yaml` and keeps your comments.
- **Live preview** runs the real lock screen in a window. Turn on **Auto-save** and every change shows up
  there within a second.
- Undo/redo: Ctrl+Z / Ctrl+Shift+Z. The editor writes a `.bak` copy the first time it saves a file.

## Layout reference

Every element has `id`, `type`, `x`, `y` and optional `anchor` (`nw n ne w center e sw s se`, the point of
the element that sits at x/y), `visible`, `seconds`, `enabled`. Coordinates are in the `screen:` size
(default 1920×1080) and are scaled to your real screen.

| type      | keys                                                                                              |
|-----------|---------------------------------------------------------------------------------------------------|
| `text`    | `text`, `color`, `size`, `font`, `bold`, `italic`, `shadow`, `width` (wrap), `align`, `command`, `interval`, `shell` |
| `image`   | `path`, `width`, `height`, `fit` (`contain` or `stretch`). Animated GIFs play.                    |
| `rect`    | `width`, `height`, `color` (fill), `border`, `border_width`, `radius`                             |
| `input`   | the password box: `width`, `height`, `radius`, `color` (border), `bg`, `text_color`, `size`, `gloss` |
| `weather` | `size`, `color`, `text_color`. Click it for the 5-day popup. Needs an OpenWeather key.          |
| `<name>`  | your own widget from `widgets/<name>.py`; every other key is passed to it as a parameter          |

Colours: `#rgb`, `#rrggbb`, a few names, or `accent` (follows `display.accent_color`).

Text placeholders: `{username} {message} {elapsed} {clock} {clock24} {time} {date} {date_long}
{hostname} {city} {temp} {weather} {locked_seconds} {locked_minutes} {output}` plus anything under
`variables:` in `config.yaml`. Unknown placeholders are left as they are.

`visible` can be `always`, `after_logout_timeout`, `after_force_timeout`, `after_seconds` or
`before_seconds` (with `seconds: N`).

`command: "uname -r"` on a text element runs the command every `interval` seconds and makes its output
available as `{output}`. Commands run without a shell unless you set `shell: true`.

## Config reference (`config.yaml`)

- `layout`: the layout file. `widgets_dir`: where your widgets live.
- `lock.timeout`: seconds of inactivity before the screen sleeps. `logout_timeout` / `logout_timeout_force`:
  minutes before Escape may log out / before a forced logout. `allow_escape_logout`. `password_text`: a
  preview of typed characters (e.g. `Hello` shows H, He, Hel…).
- `display`: `username`, `message`, `background_image` (+ `background_fit`: `stretch`, `cover`, `contain`),
  `background_color`, `accent_color`, `font_family`, `scale_layout`.
- `weather`: `api_key` (or env `OPENWEATHER_API_KEY`), `city` (or env `FT_LOCK_CITY`).
- `variables`: your own placeholders. A plain value, or `{command: "...", interval: 60}` for live output.

## Writing a widget

Create one with `python3 ft_lock.py --new-widget NAME` (add `--class` for a drawing widget), or copy one of
the examples in `widgets/`:

**Simple** (`update(ctx)` returns a string, refreshed every `interval` seconds in a background thread):

```python
LABEL = "Load"
DESCRIPTION = "1-minute load average"
SIZE = (120, 24)
PARAMS = {"interval": 5, "size": 14}

def update(ctx):
    import os
    return f"load {os.getloadavg()[0]:.2f}"
```

**Drawing** (a `Widget` class that draws on the lock screen; runs on the UI thread, keep it fast):

```python
class Widget:
    def __init__(self, ctx):
        self.ctx = ctx
    def create(self):
        self.label = self.ctx.text(0, 0, "hi")
    def update(self):
        self.ctx.itemconfig(self.label, text="tick")
    def on_click(self, event):
        pass
```

Context helpers: `ctx.param(name, default)`, `ctx.text / rect / oval / line / polygon / image`
(coordinates relative to the widget), `ctx.color`, `ctx.accent`, `ctx.values` (all placeholders),
`ctx.username`, `ctx.locked_seconds`, `ctx.run(fn, callback)` for slow work in a thread,
`ctx.after(ms, fn)` for timers.

Use it in a layout: `- {id: load, type: load, x: 100, y: 900, color: "#c8d2e6", interval: 10}`.

A widget that raises an exception does not crash the lock screen: the traceback is logged (first time,
then every 50th) and the element keeps running. `--check` reports widgets with no `Widget` class and no
`update()` function, and layout elements that point to missing widgets.

## Security notes

Unlocking uses PAM (`python3-pam`). Without it, the `FT_LOCK_PASSWORD` environment variable is used,
and if neither exists the lock prints a warning and unlocks, so install PAM for real use.
Widgets are Python code that runs with your user's permissions: only install widgets you trust.

## Migrating from the earlier version

- `layout.cfg` (JSON, written by the first Python editor) is no longer read. Recreate the elements in
  the editor, or use the `default.yaml` shipped here as a starting point.
- Layouts with `version: 1` are upgraded automatically the first time you save them.
