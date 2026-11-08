//! ft_lock layout editor — visual editor for ft_lock's YAML layout, config and coded widgets.
//!
//!   ft-lock-layout-editor [LAYOUT.yaml] [--config config.yaml] [--project DIR]
//!
//! Everything the editor does not understand is kept untouched when saving (unknown keys, plugin parameters).

use eframe::egui::{
    self, text::LayoutJob, Align2, Color32, FontFamily, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Rounding, Sense,
    Stroke, Vec2,
};
use serde::{ser::SerializeMap, Deserialize, Serialize, Serializer};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PT_TO_PX: f32 = 96.0 / 72.0; // Tk font sizes are points, the preview draws pixels
const WEATHER_LABEL_W: f32 = 34.0;
const WEATHER_GAP: f32 = 6.0;
const ANCHORS: [&str; 9] = ["nw", "n", "ne", "w", "center", "e", "sw", "s", "se"];
const VISIBLE: [&str; 5] = ["always", "after_logout_timeout", "after_force_timeout", "after_seconds", "before_seconds"];
const FONTS: [&str; 5] = ["Ubuntu", "Ubuntu Mono", "DejaVu Sans", "DejaVu Sans Mono", "Noto Sans"];
const PLACEHOLDERS: [&str; 11] = [
    "{username}", "{message}", "{elapsed}", "{clock}", "{clock24}", "{date}", "{date_long}", "{temp}", "{weather}", "{hostname}",
    "{background}",
];
const BG_DARK: Color32 = Color32::from_rgb(24, 25, 30);
const SEL: Color32 = Color32::from_rgb(90, 160, 255);

// ───────────────────────────── value helpers ─────────────────────────────

fn vnum(v: f64) -> Value {
    if (v - v.round()).abs() < 1e-6 {
        Value::Number((v.round() as i64).into())
    } else {
        Value::Number(serde_yaml::Number::from((v * 100.0).round() / 100.0))
    }
}

fn vstr(s: &str) -> Value {
    Value::String(s.to_string())
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

fn parse_scalar(s: &str) -> Value {
    let t = s.trim();
    match t {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if let Ok(i) = t.parse::<i64>() {
                Value::Number(i.into())
            } else if let Ok(f) = t.parse::<f64>() {
                if f.is_finite() { vnum(f) } else { vstr(s) }
            } else {
                vstr(s)
            }
        }
    }
}

fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => vnum(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => vstr(s),
        _ => Value::Null,
    }
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    let h = |i: usize, n: usize| u8::from_str_radix(&s[i..i + n], 16).ok();
    if !s.is_ascii() {
        return None;
    }
    match s.len() {
        3 => {
            let (r, g, b) = (h(0, 1)?, h(1, 1)?, h(2, 1)?);
            Some(Color32::from_rgb(r * 17, g * 17, b * 17))
        }
        6 => Some(Color32::from_rgb(h(0, 2)?, h(2, 2)?, h(4, 2)?)),
        _ => None,
    }
}

fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn named_color(s: &str) -> Option<Color32> {
    Some(match s.to_ascii_lowercase().as_str() {
        "white" => Color32::WHITE,
        "black" => Color32::BLACK,
        "red" => Color32::RED,
        "green" => Color32::from_rgb(0, 255, 0),
        "blue" => Color32::from_rgb(0, 0, 255),
        "yellow" => Color32::YELLOW,
        "gray" | "grey" => Color32::GRAY,
        "orange" => Color32::from_rgb(255, 165, 0),
        "cyan" => Color32::from_rgb(0, 255, 255),
        "magenta" => Color32::from_rgb(255, 0, 255),
        _ => return None,
    })
}

fn anchor_frac(a: &str) -> (f32, f32) {
    match a {
        "n" => (0.5, 0.0),
        "ne" => (1.0, 0.0),
        "w" => (0.0, 0.5),
        "center" => (0.5, 0.5),
        "e" => (1.0, 0.5),
        "sw" => (0.0, 1.0),
        "s" => (0.5, 1.0),
        "se" => (1.0, 1.0),
        _ => (0.0, 0.0),
    }
}

fn uses_anchor(kind: &str) -> bool {
    matches!(kind, "text" | "image" | "rect")
}

fn family_for(name: &str) -> FontFamily {
    let n = name.to_ascii_lowercase();
    if n.contains("mono") || n.contains("code") || n.contains("courier") || n.contains("consolas") {
        FontFamily::Monospace
    } else {
        FontFamily::Proportional
    }
}

/// Replace `{name}` placeholders using `lookup` (unknown names are kept verbatim, like ft_lock.py).
fn substitute(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end)
                if !after[..end].is_empty()
                    && after[..end].chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !after[..end].chars().next().unwrap().is_ascii_digit() =>
            {
                match lookup(&after[..end]) {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ───────────────────────────── data model ─────────────────────────────

#[derive(Clone, Debug)]
struct Element {
    uid: u64,
    id: String,
    kind: String,
    x: f32,
    y: f32,
    props: Mapping,
}

#[derive(Deserialize)]
struct RawElement {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(flatten)]
    props: Mapping,
}

impl<'de> Deserialize<'de> for Element {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = RawElement::deserialize(d)?;
        Ok(Element { uid: 0, id: r.id, kind: r.kind, x: r.x, y: r.y, props: r.props })
    }
}

impl Serialize for Element {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(4 + self.props.len()))?;
        m.serialize_entry("id", &self.id)?;
        m.serialize_entry("type", &self.kind)?;
        m.serialize_entry("x", &vnum(self.x as f64))?;
        m.serialize_entry("y", &vnum(self.y as f64))?;
        for (k, v) in &self.props {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl Element {
    fn s(&self, k: &str) -> Option<String> {
        match self.props.get(k) {
            Some(Value::Null) | None => None,
            Some(v) => Some(value_to_string(v)),
        }
    }
    fn f(&self, k: &str) -> Option<f32> {
        match self.props.get(k) {
            Some(Value::Number(n)) => n.as_f64().map(|v| v as f32),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }
    fn b(&self, k: &str) -> Option<bool> {
        match self.props.get(k) {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::String(s)) => Some(matches!(s.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1")),
            _ => None,
        }
    }
    fn set(&mut self, k: &str, v: Value) {
        self.props.insert(Value::String(k.to_string()), v);
    }
    fn unset(&mut self, k: &str) {
        self.props.remove(k);
    }
    fn anchor(&self) -> String {
        if uses_anchor(&self.kind) {
            self.s("anchor").filter(|a| ANCHORS.contains(&a.as_str())).unwrap_or_else(|| "nw".into())
        } else {
            "nw".into()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Screen {
    width: u32,
    height: u32,
}

fn two() -> u32 {
    2
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Layout {
    #[serde(default = "two")]
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<Screen>,
    #[serde(default)]
    elements: Vec<Element>,
    #[serde(flatten)]
    extra: Mapping,
}

impl Layout {
    fn empty() -> Self {
        Layout { version: 2, screen: Some(Screen { width: 1920, height: 1080 }), elements: vec![], extra: Mapping::new() }
    }

    /// Layouts written by the first editor relied on per-id defaults in ft_lock.py: make them explicit.
    fn upgrade_v1(&mut self) {
        if self.version >= 2 {
            return;
        }
        for e in &mut self.elements {
            let set = |e: &mut Element, k: &str, v: Value| {
                if !e.props.contains_key(k) {
                    e.set(k, v);
                }
            };
            match e.id.as_str() {
                "locked_text" => {
                    e.kind = "text".into();
                    set(e, "anchor", vstr("s"));
                    set(e, "text", vstr("Locked by {username} · {elapsed}"));
                    set(e, "color", vstr("#c8d2e6"));
                    set(e, "size", vnum(13.0));
                }
                "message" => {
                    e.kind = "text".into();
                    set(e, "anchor", vstr("s"));
                    set(e, "text", vstr("{message}"));
                    set(e, "color", vstr("#8a96aa"));
                    set(e, "size", vnum(11.0));
                    set(e, "italic", Value::Bool(true));
                }
                "clock" => {
                    e.kind = "text".into();
                    set(e, "anchor", vstr("ne"));
                    set(e, "text", vstr("{clock}"));
                    set(e, "color", vstr("accent"));
                    set(e, "size", vnum(16.0));
                    set(e, "bold", Value::Bool(true));
                    set(e, "shadow", Value::Bool(true));
                    set(e, "font", vstr("Ubuntu Mono"));
                }
                "logout_warning" => {
                    e.kind = "text".into();
                    set(e, "anchor", vstr("center"));
                    set(e, "color", vstr("#ff4444"));
                    set(e, "size", vnum(32.0));
                    set(e, "bold", Value::Bool(true));
                }
                "password_input" if e.kind.is_empty() => e.kind = "input".into(),
                "weather" if e.kind.is_empty() => e.kind = "weather".into(),
                _ => {}
            }
        }
        if self.screen.is_none() {
            self.screen = Some(Screen { width: 1920, height: 1080 });
        }
        self.version = 2;
    }
}

fn leading_comments(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') || (line.trim().is_empty() && !out.is_empty()) {
            out.push_str(line);
            out.push('\n');
        } else {
            break;
        }
    }
    out
}

fn parse_layout(text: &str) -> Result<(Layout, bool), String> {
    let mut layout: Layout = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    let upgraded = layout.version < 2;
    layout.upgrade_v1();
    Ok((layout, upgraded))
}

// ───────────────────────────── config.yaml (comment-preserving) ─────────────────────────────

struct Config {
    path: Option<PathBuf>,
    text: String,
    root: Value,
    changed: BTreeSet<(String, String)>,
}

impl Config {
    fn load(path: Option<PathBuf>) -> Self {
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let root = serde_yaml::from_str::<Value>(&text).ok().filter(|v| v.is_mapping()).unwrap_or_else(|| Value::Mapping(Mapping::new()));
        Config { path, text, root, changed: BTreeSet::new() }
    }
    fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.root.get(section)?.get(key)
    }
    fn get_str(&self, section: &str, key: &str, default: &str) -> String {
        self.get(section, key).map(value_to_string).filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
    }
    fn top_str(&self, key: &str) -> Option<String> {
        self.root.get(key).map(value_to_string).filter(|s| !s.is_empty())
    }
    fn set(&mut self, section: &str, key: &str, v: Value) {
        if let Some(root) = self.root.as_mapping_mut() {
            let sec = root.entry(vstr(section)).or_insert_with(|| Value::Mapping(Mapping::new()));
            if !sec.is_mapping() {
                *sec = Value::Mapping(Mapping::new());
            }
            sec.as_mapping_mut().unwrap().insert(vstr(key), v);
        }
        self.changed.insert((section.to_string(), key.to_string()));
    }
    fn save(&mut self) -> Result<String, String> {
        let path = self.path.clone().ok_or("no config path")?;
        let mut text = self.text.clone();
        let mut ok = true;
        for (section, key) in self.changed.clone() {
            let Some(v) = self.get(&section, &key).cloned() else { continue };
            match patch_yaml_scalar(&text, &section, &key, &v) {
                Some(t) => text = t,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        // Safety net: the patched text must mean exactly what we hold in memory.
        if ok && serde_yaml::from_str::<Value>(&text).ok().as_ref() != Some(&self.root) {
            ok = false;
        }
        let mut note = "comments kept";
        if !ok {
            text = leading_comments(&self.text) + &serde_yaml::to_string(&self.root).map_err(|e| e.to_string())?;
            note = "comments inside the file could not be kept";
        }
        if path.exists() && !path.with_extension("yaml.bak").exists() {
            let _ = std::fs::copy(&path, path.with_extension("yaml.bak"));
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, &text).map_err(|e| e.to_string())?;
        self.text = text;
        self.changed.clear();
        Ok(note.to_string())
    }
}

fn render_scalar(v: &Value) -> Option<String> {
    match v {
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Null => Some("null".into()),
        Value::String(s) => {
            let plain = !s.is_empty()
                && s.chars().next().map_or(false, |c| c.is_ascii_alphabetic())
                && s.chars().all(|c| c.is_ascii_alphanumeric() || " _./-".contains(c))
                && !s.ends_with(' ')
                && !matches!(s.to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~" | "y" | "n");
            Some(if plain { s.clone() } else { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) })
        }
        _ => None,
    }
}

/// Byte index of a trailing ` #comment` that is not inside quotes.
fn comment_start(s: &str) -> Option<usize> {
    let (mut q, mut prev) = (None::<char>, ' ');
    for (i, c) in s.char_indices() {
        match q {
            Some(open) => {
                if c == open && prev != '\\' {
                    q = None;
                }
            }
            None => {
                if (c == '"' || c == '\'') && (prev == ' ' || prev == ':') {
                    q = Some(c);
                } else if c == '#' && prev.is_whitespace() {
                    return Some(i);
                }
            }
        }
        prev = c;
    }
    None
}

fn patch_yaml_scalar(text: &str, section: &str, key: &str, value: &Value) -> Option<String> {
    let rendered = render_scalar(value)?;
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let header = format!("{section}:");
    let mut sec_idx = None;
    let mut in_section = false;
    for i in 0..lines.len() {
        let line = &lines[i];
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if indent == 0 {
            in_section = t.starts_with(&header);
            if in_section {
                let after = t[header.len()..].trim();
                if !(after.is_empty() || after.starts_with('#')) {
                    return None; // flow style `section: {..}` — don't touch
                }
                sec_idx = Some(i);
            }
            continue;
        }
        if in_section && t.starts_with(&format!("{key}:")) {
            let key_end = indent + key.len() + 1;
            let rest = &line[key_end..];
            let comment = comment_start(rest);
            let value_part = &rest[..comment.unwrap_or(rest.len())];
            let old_len = value_part.trim().len();
            let gap_after = value_part.len() - value_part.trim_end().len();
            let mut new_line = format!("{}{key}: {rendered}", " ".repeat(indent));
            if let Some(c) = comment {
                let pad = (gap_after + old_len).saturating_sub(rendered.len()).max(2);
                new_line.push_str(&" ".repeat(pad));
                new_line.push_str(&rest[c..]);
            }
            lines[i] = new_line;
            return Some(lines.join("\n") + "\n");
        }
    }
    match sec_idx {
        Some(i) => lines.insert(i + 1, format!("  {key}: {rendered}")),
        None => {
            if !lines.is_empty() && !lines.last().unwrap().trim().is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("{section}:"));
            lines.push(format!("  {key}: {rendered}"));
        }
    }
    Some(lines.join("\n") + "\n")
}

// ───────────────────────────── widgets (plugins) & library ─────────────────────────────

#[derive(Clone, Debug, Deserialize, Default)]
struct WidgetInfo {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    file: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    size: Vec<f32>,
    #[serde(default)]
    params: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

impl WidgetInfo {
    fn default_size(&self) -> Vec2 {
        if self.size.len() == 2 { Vec2::new(self.size[0], self.size[1]) } else { Vec2::new(160.0, 32.0) }
    }
}

#[derive(Clone)]
struct LibItem {
    label: String,
    hint: String,
    group: &'static str,
    kind: String,
    id: String,
    preset: Vec<(&'static str, Value)>,
    needs_file: bool,
}

fn builtin_library() -> Vec<LibItem> {
    let item = |group, label: &str, hint: &str, kind: &str, id: &str, preset: Vec<(&'static str, Value)>, needs_file| LibItem {
        label: label.into(),
        hint: hint.into(),
        group,
        kind: kind.into(),
        id: id.into(),
        preset,
        needs_file,
    };
    vec![
        item("Basic", "Text", "Static text or {placeholders}", "text", "text",
            vec![("text", vstr("New text")), ("color", vstr("#ffffff")), ("size", vnum(24.0))], false),
        item("Basic", "Image / GIF", "PNG, JPG, animated GIF", "image", "image", vec![], true),
        item("Basic", "Rectangle", "Panel, bar or divider", "rect", "rect",
            vec![("width", vnum(200.0)), ("height", vnum(60.0)), ("color", vstr("#101725")), ("border", vstr("accent")),
                 ("border_width", vnum(2.0)), ("radius", vnum(12.0))], false),
        item("Lock screen", "Password input", "The box you type into", "input", "password_input", vec![], false),
        item("Lock screen", "Weather", "Icon + 5-day popup (needs API key)", "weather", "weather", vec![], false),
        item("Presets", "Clock", "{clock} with shadow", "text", "clock",
            vec![("anchor", vstr("ne")), ("text", vstr("{clock}")), ("color", vstr("accent")), ("size", vnum(16.0)),
                 ("font", vstr("Ubuntu Mono")), ("bold", Value::Bool(true)), ("shadow", Value::Bool(true))], false),
        item("Presets", "Date", "{date_long}", "text", "date",
            vec![("text", vstr("{date_long}")), ("color", vstr("#8a96aa")), ("size", vnum(14.0))], false),
        item("Presets", "Locked by…", "who + how long ago", "text", "locked_text",
            vec![("anchor", vstr("s")), ("text", vstr("Locked by {username} · {elapsed}")), ("color", vstr("#c8d2e6")), ("size", vnum(13.0))], false),
        item("Presets", "Message", "{message} from config", "text", "message",
            vec![("anchor", vstr("s")), ("text", vstr("{message}")), ("color", vstr("#8a96aa")), ("size", vnum(11.0)), ("italic", Value::Bool(true))], false),
        item("Presets", "Logout warning", "Shown after the logout timeout", "text", "logout_warning",
            vec![("anchor", vstr("center")), ("text", vstr("PRESS ESCAPE TO LOGOUT")), ("color", vstr("#ff4444")), ("size", vnum(32.0)),
                 ("bold", Value::Bool(true)), ("visible", vstr("after_logout_timeout"))], false),
        item("Presets", "Command output", "Shows any shell command's output", "text", "command_output",
            vec![("text", vstr("{output}")), ("command", vstr("uptime -p")), ("interval", vnum(10.0)), ("size", vnum(14.0)), ("color", vstr("#ffffff"))], false),
    ]
}

#[derive(Clone)]
struct LibPayload(LibItem);
#[derive(Clone)]
struct RowPayload(u64);

// ───────────────────────────── file dialogs / processes ─────────────────────────────

fn run_dialog(args: &[&str]) -> Result<Option<PathBuf>, String> {
    for tool in ["zenity", "kdialog"] {
        let mut cmd = Command::new(tool);
        if tool == "zenity" {
            cmd.arg("--file-selection").args(args);
        } else {
            let mut a = args.iter();
            let mut save = false;
            let mut dir = false;
            let mut title = String::new();
            while let Some(x) = a.next() {
                match *x {
                    "--save" => save = true,
                    "--directory" => dir = true,
                    "--title" => title = a.next().map(|s| s.to_string()).unwrap_or_default(),
                    _ => {}
                }
            }
            cmd.arg(if dir { "--getexistingdirectory" } else if save { "--getsavefilename" } else { "--getopenfilename" }).arg(".").arg("--title").arg(title);
        }
        match cmd.output() {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return Ok(if out.status.success() && !s.is_empty() { Some(PathBuf::from(s)) } else { None });
            }
            Err(_) => continue,
        }
    }
    Err("no file dialog available (install zenity) — drag a file onto the window instead".into())
}

fn pick_file(title: &str, filter: Option<&str>) -> Result<Option<PathBuf>, String> {
    let mut args: Vec<String> = vec!["--title".into(), title.into()];
    if let Some(f) = filter {
        args.push(format!("--file-filter={f}"));
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_dialog(&refs)
}

fn pick_folder(title: &str) -> Result<Option<PathBuf>, String> {
    run_dialog(&["--directory", "--title", title])
}

fn save_dialog(title: &str, name: &str) -> Result<Option<PathBuf>, String> {
    let filename = format!("--filename={name}");
    run_dialog(&["--save", "--confirm-overwrite", "--title", title, &filename])
}

fn find_project(explicit: Option<PathBuf>, hints: &[PathBuf]) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    let mut cands: Vec<PathBuf> = hints.to_vec();
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        cands.extend(exe.ancestors().skip(1).take(5).map(|p| p.to_path_buf()));
    }
    cands.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    for c in cands {
        for d in [c.clone(), c.join(".."), c.join("../..")] {
            if d.join("ft_lock.py").is_file() {
                return d.canonicalize().unwrap_or(d);
            }
        }
    }
    PathBuf::from(".")
}

// ───────────────────────────── editor state ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    TL,
    T,
    TR,
    L,
    R,
    BL,
    B,
    BR,
}

impl Handle {
    const ALL: [Handle; 8] = [Handle::TL, Handle::T, Handle::TR, Handle::L, Handle::R, Handle::BL, Handle::B, Handle::BR];
    fn frac(self) -> (f32, f32) {
        match self {
            Handle::TL => (0.0, 0.0),
            Handle::T => (0.5, 0.0),
            Handle::TR => (1.0, 0.0),
            Handle::L => (0.0, 0.5),
            Handle::R => (1.0, 0.5),
            Handle::BL => (0.0, 1.0),
            Handle::B => (0.5, 1.0),
            Handle::BR => (1.0, 1.0),
        }
    }
    fn cursor(self) -> egui::CursorIcon {
        match self {
            Handle::TL | Handle::BR => egui::CursorIcon::ResizeNwSe,
            Handle::TR | Handle::BL => egui::CursorIcon::ResizeNeSw,
            Handle::T | Handle::B => egui::CursorIcon::ResizeVertical,
            Handle::L | Handle::R => egui::CursorIcon::ResizeHorizontal,
        }
    }
}

enum Drag {
    None,
    Move { start: Pos2, orig: Vec<(u64, f32, f32)>, bounds: Rect, others: Vec<Rect> },
    Resize { uid: u64, handle: Handle, orig: Rect },
    Marquee { start: Pos2, additive: bool },
    Pan,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Layers,
    Library,
    Settings,
}

struct Tex {
    handle: egui::TextureHandle,
    nat: Vec2,
}

enum Confirm {
    Close,
    NewLayout,
    Reload,
}

struct App {
    project: PathBuf,
    layout_path: PathBuf,
    header: String,
    layout: Layout,
    config: Config,
    widgets: Vec<WidgetInfo>,
    library: Vec<LibItem>,
    selected: Vec<u64>,
    hidden: HashSet<u64>,
    locked: HashSet<u64>,
    next_uid: u64,
    undo: Vec<Vec<Element>>,
    redo: Vec<Vec<Element>>,
    last_checkpoint: Instant,
    dirty: bool,
    last_change: Instant,
    autosave: bool,
    backed_up: bool,
    zoom: f32,
    origin: Vec2,
    fit_pending: bool,
    drag: Drag,
    guides: Vec<(bool, f32)>, // (vertical?, layout coordinate)
    tab: Tab,
    status: String,
    status_at: Instant,
    grid: f32,
    snap: bool,
    show_grid: bool,
    smart_guides: bool,
    sample_text: bool,
    textures: HashMap<PathBuf, Option<Tex>>,
    preview: Option<Child>,
    confirm: Option<Confirm>,
    allow_close: bool,
    last_title: String,
    new_param: (String, String),
    new_widget: (String, bool),
    focus_text: bool,
    pending_drop_pos: Option<Pos2>,
    canvas_min: Pos2,
    carousel_cache: (String, Option<String>),
}

impl App {
    fn new(layout_path: PathBuf, project: PathBuf, config_path: Option<PathBuf>) -> Self {
        let mut app = App {
            project,
            layout_path: layout_path.clone(),
            header: String::new(),
            layout: Layout::empty(),
            config: Config::load(config_path),
            widgets: vec![],
            library: builtin_library(),
            selected: vec![],
            hidden: HashSet::new(),
            locked: HashSet::new(),
            next_uid: 1,
            undo: vec![],
            redo: vec![],
            last_checkpoint: Instant::now() - Duration::from_secs(10),
            dirty: false,
            last_change: Instant::now(),
            autosave: false,
            backed_up: false,
            zoom: 0.5,
            origin: Vec2::ZERO,
            fit_pending: true,
            drag: Drag::None,
            guides: vec![],
            tab: Tab::Layers,
            status: String::new(),
            status_at: Instant::now(),
            grid: 10.0,
            snap: true,
            show_grid: false,
            smart_guides: true,
            sample_text: true,
            textures: HashMap::new(),
            preview: None,
            confirm: None,
            allow_close: false,
            last_title: String::new(),
            new_param: (String::new(), String::new()),
            new_widget: (String::new(), true),
            focus_text: false,
            pending_drop_pos: None,
            canvas_min: Pos2::ZERO,
            carousel_cache: (String::from("\u{0}"), None),
        };
        app.load_layout_file(&layout_path);
        app.refresh_widgets();
        app
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Instant::now();
    }

    fn layout_dir(&self) -> PathBuf {
        self.layout_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
    }

    fn screen(&self) -> Vec2 {
        let s = self.layout.screen.clone().unwrap_or(Screen { width: 1920, height: 1080 });
        Vec2::new(s.width as f32, s.height as f32)
    }

    // ── loading / saving ──
    fn load_layout_file(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(text) => match parse_layout(&text) {
                Ok((mut layout, upgraded)) => {
                    for e in &mut layout.elements {
                        e.uid = self.next_uid;
                        self.next_uid += 1;
                    }
                    self.header = leading_comments(&text);
                    self.layout = layout;
                    self.layout_path = path.to_path_buf();
                    self.selected.clear();
                    self.undo.clear();
                    self.redo.clear();
                    self.dirty = upgraded;
                    self.backed_up = false;
                    self.say(if upgraded { "Loaded an old (v1) layout — it will be saved as v2" } else { "Loaded layout" });
                }
                Err(e) => self.say(format!("Cannot parse {}: {e}", path.display())),
            },
            Err(_) => {
                self.layout = Layout::empty();
                self.layout_path = path.to_path_buf();
                self.header = String::new();
                self.dirty = false;
                self.say(format!("{} does not exist yet — starting empty (Ctrl+S creates it)", path.display()));
            }
        }
        self.fit_pending = true;
    }

    fn save_layout(&mut self) {
        match serde_yaml::to_string(&self.layout) {
            Ok(body) => {
                let mut text = self.header.clone();
                if text.is_empty() {
                    text.push_str("# ft_lock layout — edited with the layout editor. See default.yaml / README for every option.\n");
                }
                text.push_str(&body);
                if !self.backed_up && self.layout_path.exists() {
                    let _ = std::fs::copy(&self.layout_path, self.layout_path.with_extension("yaml.bak"));
                    self.backed_up = true;
                }
                match std::fs::write(&self.layout_path, text) {
                    Ok(_) => {
                        self.dirty = false;
                        self.say(format!("Saved {}", self.layout_path.display()));
                    }
                    Err(e) => self.say(format!("Save failed: {e}")),
                }
            }
            Err(e) => self.say(format!("Serialize failed: {e}")),
        }
    }

    fn save_all(&mut self) {
        self.save_layout();
        if !self.config.changed.is_empty() {
            match self.config.save() {
                Ok(note) => self.say(format!("Saved layout + config ({note})")),
                Err(e) => self.say(format!("Config save failed: {e}")),
            }
        }
    }

    fn save_as(&mut self) {
        let name = self.layout_path.file_name().and_then(|n| n.to_str()).unwrap_or("layout.yaml").to_string();
        match save_dialog("Save layout as", &name) {
            Ok(Some(p)) => {
                self.layout_path = p;
                self.backed_up = true;
                self.save_layout();
            }
            Ok(None) => {}
            Err(e) => self.say(e),
        }
    }

    fn open_layout(&mut self) {
        match pick_file("Open layout", Some("YAML | *.yaml *.yml")) {
            Ok(Some(p)) => self.load_layout_file(&p),
            Ok(None) => {}
            Err(e) => self.say(e),
        }
    }

    fn script_path(&self) -> PathBuf {
        self.project.join("ft_lock.py")
    }

    fn widgets_dir(&self) -> PathBuf {
        let rel = self.config.top_str("widgets_dir").unwrap_or_else(|| "widgets".into());
        let p = PathBuf::from(&rel);
        if p.is_absolute() {
            p
        } else {
            let base = self.config.path.as_ref().and_then(|c| c.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| self.project.clone());
            if base.join(&p).exists() || !self.project.join(&p).exists() { base.join(p) } else { self.project.join(p) }
        }
    }

    fn refresh_widgets(&mut self) {
        let mut cmd = Command::new("python3");
        cmd.arg(self.script_path()).arg("--list-widgets");
        if let Some(c) = &self.config.path {
            cmd.arg("--config").arg(c);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => match serde_json::from_slice::<Vec<WidgetInfo>>(&out.stdout) {
                Ok(list) => {
                    self.widgets = list;
                }
                Err(e) => self.say(format!("Could not read the widget list: {e}")),
            },
            Ok(out) => self.say(format!("python3 ft_lock.py --list-widgets failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""))),
            Err(e) => self.say(format!("Cannot run python3 ({e}) — widget library limited to built-ins")),
        }
    }

    fn toggle_preview(&mut self) {
        if let Some(mut child) = self.preview.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.say("Preview closed");
            return;
        }
        if self.dirty {
            self.save_all();
        }
        let mut cmd = Command::new("python3");
        cmd.arg(self.script_path()).arg("--preview").arg("--layout").arg(&self.layout_path);
        if let Some(c) = &self.config.path {
            cmd.arg("--config").arg(c);
        }
        match cmd.spawn() {
            Ok(child) => {
                self.preview = Some(child);
                self.say("Preview running — it reloads whenever you save (turn on Auto-save for live editing)");
            }
            Err(e) => self.say(format!("Cannot start the preview: {e}")),
        }
    }

    // ── undo / elements ──
    fn checkpoint(&mut self) {
        self.undo.push(self.layout.elements.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_checkpoint = Instant::now();
    }

    /// For continuous edits (typing, dragging a value): one undo step per burst.
    fn checkpoint_coalesced(&mut self, before: Vec<Element>) {
        if self.last_checkpoint.elapsed() > Duration::from_millis(700) {
            self.undo.push(before);
            self.redo.clear();
        }
        self.last_checkpoint = Instant::now();
    }

    fn touch(&mut self) {
        self.dirty = true;
        self.last_change = Instant::now();
    }

    fn do_undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.layout.elements, prev));
            self.after_history();
        }
    }

    fn do_redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.layout.elements, next));
            self.after_history();
        }
    }

    fn after_history(&mut self) {
        let live: HashSet<u64> = self.layout.elements.iter().map(|e| e.uid).collect();
        self.selected.retain(|u| live.contains(u));
        self.touch();
    }

    fn idx(&self, uid: u64) -> Option<usize> {
        self.layout.elements.iter().position(|e| e.uid == uid)
    }

    fn primary(&self) -> Option<u64> {
        self.selected.last().copied()
    }

    fn unique_id(&self, base: &str) -> String {
        let taken: HashSet<&str> = self.layout.elements.iter().map(|e| e.id.as_str()).collect();
        if !taken.contains(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}_{n}")).find(|c| !taken.contains(c.as_str())).unwrap()
    }

    fn add_element(&mut self, kind: &str, id: &str, preset: &[(&'static str, Value)], at: Option<Pos2>) -> u64 {
        self.checkpoint();
        let uid = self.next_uid;
        self.next_uid += 1;
        let mut e = Element { uid, id: self.unique_id(id), kind: kind.to_string(), x: 100.0, y: 100.0, props: Mapping::new() };
        for (k, v) in preset {
            e.set(k, v.clone());
        }
        let n = self.layout.elements.len() as f32;
        let (x, y) = match at {
            Some(p) => (p.x, p.y),
            None => (self.screen().x * 0.3 + (n * 24.0) % 240.0, self.screen().y * 0.3 + (n * 24.0) % 160.0),
        };
        e.x = x.round();
        e.y = y.round();
        self.layout.elements.push(e);
        self.selected = vec![uid];
        self.touch();
        uid
    }

    fn add_from_library(&mut self, item: &LibItem, at: Option<Pos2>) {
        if item.kind == "input" && self.layout.elements.iter().any(|e| e.kind == "input") {
            self.say("There is already a password input — ft_lock only uses the first one");
        }
        if item.needs_file {
            match pick_file("Choose an image or GIF", Some("Images | *.png *.jpg *.jpeg *.gif *.webp *.bmp")) {
                Ok(Some(p)) => {
                    self.add_image(&p, at);
                }
                Ok(None) => {}
                Err(e) => self.say(e),
            }
            return;
        }
        self.add_element(&item.kind, &item.id, &item.preset, at);
    }

    fn path_for_yaml(&self, p: &Path) -> String {
        let dir = self.layout_dir();
        match p.strip_prefix(&dir) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => p.to_string_lossy().to_string(),
        }
    }

    fn add_image(&mut self, p: &Path, at: Option<Pos2>) {
        let rel = self.path_for_yaml(p);
        self.add_element("image", "image", &[("path", vstr(&rel))], at);
    }

    fn add_plugin(&mut self, w: &WidgetInfo, at: Option<Pos2>) {
        self.add_element(&w.name.clone(), &w.name.clone(), &[], at);
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        self.checkpoint();
        let sel: HashSet<u64> = self.selected.iter().copied().collect();
        self.layout.elements.retain(|e| !sel.contains(&e.uid) || self.locked.contains(&e.uid));
        self.selected.clear();
        self.touch();
    }

    fn duplicate_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        self.checkpoint();
        let mut new_sel = vec![];
        for uid in self.selected.clone() {
            if let Some(i) = self.idx(uid) {
                let mut e = self.layout.elements[i].clone();
                e.uid = self.next_uid;
                self.next_uid += 1;
                e.id = self.unique_id(&e.id);
                e.x += 20.0;
                e.y += 20.0;
                new_sel.push(e.uid);
                self.layout.elements.push(e);
            }
        }
        self.selected = new_sel;
        self.touch();
    }

    fn reorder(&mut self, uid: u64, new_index: usize) {
        if let Some(i) = self.idx(uid) {
            self.checkpoint();
            let e = self.layout.elements.remove(i);
            let j = new_index.min(self.layout.elements.len());
            self.layout.elements.insert(j, e);
            self.touch();
        }
    }

    fn nudge(&mut self, dx: f32, dy: f32) {
        if self.selected.is_empty() {
            return;
        }
        let before = self.layout.elements.clone();
        for uid in self.selected.clone() {
            if self.locked.contains(&uid) {
                continue;
            }
            if let Some(i) = self.idx(uid) {
                self.layout.elements[i].x += dx;
                self.layout.elements[i].y += dy;
            }
        }
        self.checkpoint_coalesced(before);
        self.touch();
    }

    // ── colours / text preview ──
    fn accent(&self) -> Color32 {
        parse_hex(&self.config.get_str("display", "accent_color", "#5a7ab0")).unwrap_or(Color32::from_rgb(0x5a, 0x7a, 0xb0))
    }

    fn color(&self, raw: Option<String>, default: &str) -> Color32 {
        let raw = raw.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| default.to_string());
        let r = raw.trim();
        if r.eq_ignore_ascii_case("accent") || r.eq_ignore_ascii_case("$accent") {
            return self.accent();
        }
        parse_hex(r).or_else(|| named_color(r)).unwrap_or(Color32::WHITE)
    }

    fn preview_text(&self, e: &Element) -> String {
        let raw = e.s("text").unwrap_or_else(|| "Text".into());
        if !self.sample_text {
            return raw;
        }
        let has_cmd = e.s("command").is_some();
        let vars: Option<&Mapping> = self.config.root.get("variables").and_then(|v| v.as_mapping());
        substitute(&raw, &|name| {
            Some(match name {
                "username" => std::env::var("USER").unwrap_or_else(|_| "user".into()),
                "message" => self.config.get_str("display", "message", "Back sOOn.."),
                "elapsed" => "a few seconds ago…".into(),
                "clock" => "10:42 AM".into(),
                "clock24" => "10:42".into(),
                "time" => "10:42:17".into(),
                "date" => "Fri 06 Nov".into(),
                "date_long" => "Friday 06 November 2026".into(),
                "year" => "2026".into(),
                "hostname" => std::env::var("HOSTNAME").unwrap_or_else(|_| "host".into()),
                "city" => self.config.get_str("weather", "city", "Paris"),
                "temp" => "18°".into(),
                "weather" => "Light Rain".into(),
                "locked_seconds" => "42".into(),
                "locked_minutes" => "0".into(),
                "background" => "wallpaper.png".into(),
                "output" => if has_cmd { "(command output)".into() } else { return None },
                other => {
                    let v = vars?.get(other)?;
                    if v.is_mapping() { format!("<{other}>") } else { value_to_string(v) }
                }
            })
        })
    }

    // ── textures / geometry ──
    fn resolve_asset(&self, p: &str) -> PathBuf {
        let expanded = match (p.strip_prefix("~/"), std::env::var("HOME")) {
            (Some(rest), Ok(home)) => format!("{home}/{rest}"),
            _ => p.to_string(),
        };
        let path = PathBuf::from(expanded);
        if path.is_absolute() {
            return path;
        }
        let mut bases = vec![self.layout_dir()];
        if let Some(c) = self.config.path.as_ref().and_then(|c| c.parent()) {
            bases.push(c.to_path_buf());
        }
        bases.push(self.project.clone());
        bases.iter().map(|b| b.join(&path)).find(|c| c.exists()).unwrap_or_else(|| self.layout_dir().join(path))
    }

    fn texture(&mut self, ctx: &egui::Context, path: &Path) -> Option<(egui::TextureId, Vec2)> {
        if !self.textures.contains_key(path) {
            let loaded = image::open(path).ok().map(|img| {
                let nat = Vec2::new(img.width() as f32, img.height() as f32);
                let img = if img.width() > 2048 || img.height() > 2048 { img.thumbnail(2048, 2048) } else { img };
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                Tex { handle: ctx.load_texture(path.to_string_lossy(), ci, egui::TextureOptions::LINEAR), nat }
            });
            self.textures.insert(path.to_path_buf(), loaded);
        }
        self.textures.get(path).and_then(|t| t.as_ref()).map(|t| (t.handle.id(), t.nat))
    }

    fn galley(&self, ctx: &egui::Context, e: &Element, zoom: f32, color_override: Option<Color32>) -> Arc<egui::Galley> {
        let px = (e.f("size").unwrap_or(18.0) * PT_TO_PX * zoom).max(1.0);
        let family = family_for(&e.s("font").unwrap_or_else(|| "Ubuntu".into()));
        let color = color_override.unwrap_or_else(|| self.color(e.s("color"), "#ffffff"));
        let wrap = e.f("width").filter(|w| *w > 0.0).map(|w| w * zoom).unwrap_or(f32::INFINITY);
        let mut job = LayoutJob::simple(self.preview_text(e), FontId::new(px, family), color, wrap);
        if e.b("italic").unwrap_or(false) {
            for s in &mut job.sections {
                s.format.italics = true;
            }
        }
        ctx.fonts(|f| f.layout_job(job))
    }

    /// Bounding box of an element in layout pixels.
    fn rect_of(&mut self, ctx: &egui::Context, e: &Element) -> Rect {
        let anchor = e.anchor();
        let (fx, fy) = anchor_frac(&anchor);
        let at = |size: Vec2| Rect::from_min_size(Pos2::new(e.x - size.x * fx, e.y - size.y * fy), size);
        match e.kind.as_str() {
            "text" => {
                let g = self.galley(ctx, e, 1.0, None);
                at(g.size().max(Vec2::splat(4.0)))
            }
            "image" => {
                let path = self.resolve_asset(&e.s("path").unwrap_or_default());
                let (bw, bh) = (e.f("width").unwrap_or(0.0), e.f("height").unwrap_or(0.0));
                let size = match self.texture(ctx, &path) {
                    Some((_, nat)) => {
                        if bw <= 0.0 && bh <= 0.0 {
                            nat
                        } else if e.s("fit").as_deref() == Some("stretch") && bw > 0.0 && bh > 0.0 {
                            Vec2::new(bw, bh)
                        } else {
                            let ratio = (if bw > 0.0 { bw / nat.x } else { f32::MAX }).min(if bh > 0.0 { bh / nat.y } else { f32::MAX });
                            nat * ratio
                        }
                    }
                    None => Vec2::new(if bw > 0.0 { bw } else { 160.0 }, if bh > 0.0 { bh } else { 90.0 }),
                };
                at(size)
            }
            "rect" => at(Vec2::new(e.f("width").unwrap_or(100.0), e.f("height").unwrap_or(40.0))),
            "input" => at(Vec2::new(e.f("width").unwrap_or(240.0), e.f("height").unwrap_or(32.0))),
            "weather" => {
                let s = e.f("size").unwrap_or(46.0);
                at(Vec2::new(WEATHER_LABEL_W + WEATHER_GAP + s, s))
            }
            other => {
                let info = self.widgets.iter().find(|w| w.name == other);
                let mut d = info.map(|w| w.default_size()).unwrap_or(Vec2::new(160.0, 32.0));
                if d.x == d.y {
                    if let Some(s) = e.f("size") {
                        d = Vec2::splat(s);
                    }
                }
                at(Vec2::new(e.f("width").unwrap_or(d.x), e.f("height").unwrap_or(d.y)))
            }
        }
    }

    /// First image the carousel would show (used as the editor's background preview).
    fn carousel_first(&mut self) -> Option<String> {
        if !self.config.get("carousel", "enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
            return None;
        }
        let first = self.config.get("carousel", "images").and_then(|v| v.as_sequence()).and_then(|s| s.first()).map(value_to_string).unwrap_or_default();
        let folder = self.config.get_str("carousel", "folder", "");
        let key = format!("{first}|{folder}");
        if self.carousel_cache.0 != key {
            let mut res = if first.is_empty() { None } else { Some(first.clone()) };
            if res.is_none() && !folder.is_empty() {
                let dir = self.resolve_asset(&folder);
                let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
                    .unwrap_or_default();
                files.retain(|p| {
                    p.is_file() && matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"))
                });
                files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                res = files.first().map(|p| p.to_string_lossy().to_string());
            }
            self.carousel_cache = (key, res);
        }
        self.carousel_cache.1.clone()
    }

    fn can_resize(kind: &str) -> bool {
        matches!(kind, "rect" | "image" | "input" | "weather") || !matches!(kind, "text")
    }
}

// ───────────────────────────── painting ─────────────────────────────

impl App {
    fn to_screen(&self, origin: Pos2, p: Pos2) -> Pos2 {
        origin + p.to_vec2() * self.zoom
    }

    fn to_layout(&self, origin: Pos2, p: Pos2) -> Pos2 {
        ((p - origin) / self.zoom).to_pos2()
    }

    fn paint_element(&mut self, painter: &egui::Painter, ctx: &egui::Context, org: Pos2, e: &Element) {
        let z = self.zoom;
        let r = self.rect_of(ctx, e);
        let sr = Rect::from_min_max(self.to_screen(org, r.min), self.to_screen(org, r.max));
        match e.kind.as_str() {
            "text" => {
                if e.b("shadow").unwrap_or(false) || e.s("shadow").map_or(false, |s| s.starts_with('#')) {
                    let sc = e.s("shadow").and_then(|s| parse_hex(&s)).unwrap_or(Color32::BLACK);
                    let g = self.galley(ctx, e, z, Some(sc));
                    painter.galley(sr.min + Vec2::splat(z.max(1.0)), g, sc);
                }
                let g = self.galley(ctx, e, z, None);
                let col = self.color(e.s("color"), "#ffffff");
                painter.galley(sr.min, g.clone(), col);
                if e.b("bold").unwrap_or(false) {
                    painter.galley(sr.min + Vec2::new(0.7 * z.max(1.0), 0.0), g, col);
                }
            }
            "image" => {
                let path = self.resolve_asset(&e.s("path").unwrap_or_default());
                match self.texture(ctx, &path) {
                    Some((id, _)) => {
                        painter.image(id, sr, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        painter.rect_filled(sr, 2.0, Color32::from_rgba_unmultiplied(120, 60, 60, 80));
                        painter.text(sr.center(), Align2::CENTER_CENTER, format!("missing image\n{}", e.s("path").unwrap_or_default()),
                            FontId::proportional(12.0), Color32::LIGHT_RED);
                    }
                }
            }
            "rect" => {
                let fill = match e.s("color").as_deref() {
                    Some("") | Some("none") => Color32::TRANSPARENT,
                    c => self.color(c.map(|s| s.to_string()), "#ffffff"),
                };
                let stroke = match e.s("border") {
                    Some(b) => Stroke::new(e.f("border_width").unwrap_or(1.0).max(1.0) * z, self.color(Some(b), "#ffffff")),
                    None => Stroke::NONE,
                };
                painter.rect(sr, Rounding::same(e.f("radius").unwrap_or(0.0) * z), fill, stroke);
            }
            "input" => {
                let rad = Rounding::same(e.f("radius").unwrap_or(10.0) * z);
                let accent = self.color(e.s("color"), "accent");
                painter.rect_filled(sr.translate(Vec2::splat(2.0 * z)), rad, Color32::from_rgb(0x10, 0x17, 0x25));
                painter.rect(sr, rad, self.color(e.s("bg"), "#1a2840"), Stroke::new(2.0 * z, accent));
                if e.b("gloss").unwrap_or(true) {
                    let g = Rect::from_min_max(sr.min + Vec2::splat(2.0 * z), Pos2::new(sr.max.x - 2.0 * z, sr.center().y));
                    painter.rect_filled(g, Rounding::same((e.f("radius").unwrap_or(10.0) - 3.0).max(3.0) * z), Color32::from_rgb(0x27, 0x40, 0x67));
                }
                painter.text(Pos2::new(sr.min.x + 10.0 * z, sr.center().y), Align2::LEFT_CENTER, "●●●●●●",
                    FontId::monospace((e.f("size").unwrap_or(13.0) * PT_TO_PX * z).max(1.0)), self.color(e.s("text_color"), "#c8d2e6"));
            }
            "weather" => {
                let s = e.f("size").unwrap_or(46.0) * z;
                let center = Pos2::new(sr.max.x - s / 2.0, sr.center().y);
                painter.circle_filled(center, s / 2.0, self.color(e.s("color"), "accent"));
                let tc = self.color(e.s("text_color"), "#ffffff");
                painter.text(center, Align2::CENTER_CENTER, "☀", FontId::proportional(18.0 * PT_TO_PX * z), tc);
                painter.text(Pos2::new(sr.min.x + WEATHER_LABEL_W * z, sr.center().y), Align2::RIGHT_CENTER, "18°",
                    FontId::monospace(11.0 * PT_TO_PX * z), tc);
            }
            other => {
                let info = self.widgets.iter().find(|w| w.name == other).cloned();
                let missing = info.is_none();
                let col = if missing || info.as_ref().map_or(false, |i| i.error.is_some()) { Color32::LIGHT_RED } else { Color32::from_rgb(170, 200, 255) };
                painter.rect_filled(sr, 3.0, Color32::from_rgba_unmultiplied(90, 122, 176, 40));
                painter.extend(egui::Shape::dashed_line(
                    &[sr.left_top(), sr.right_top(), sr.right_bottom(), sr.left_bottom(), sr.left_top()], Stroke::new(1.0, col), 6.0, 4.0));
                let label = match &info {
                    Some(i) => format!("◈ {}", if i.label.is_empty() { &i.name } else { &i.label }),
                    None => format!("◈ {other}  (widgets/{other}.py not found)"),
                };
                painter.with_clip_rect(sr).text(sr.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0 * z.max(0.8)), col);
            }
        }
    }

    fn paint_grid(&self, painter: &egui::Painter, org: Pos2, screen: Rect) {
        if !self.show_grid || self.grid < 2.0 {
            return;
        }
        let step = self.grid * self.zoom;
        if step < 6.0 {
            return;
        }
        let stroke = Stroke::new(1.0, Color32::from_white_alpha(14));
        let mut x = screen.min.x;
        while x <= screen.max.x {
            painter.line_segment([Pos2::new(x, screen.min.y), Pos2::new(x, screen.max.y)], stroke);
            x += step;
        }
        let mut y = screen.min.y;
        while y <= screen.max.y {
            painter.line_segment([Pos2::new(screen.min.x, y), Pos2::new(screen.max.x, y)], stroke);
            y += step;
        }
        let _ = org;
    }
}

// ───────────────────────────── canvas interaction ─────────────────────────────

impl App {
    fn handle_positions(&self, r: Rect) -> Vec<(Handle, Pos2)> {
        Handle::ALL.iter().map(|h| {
            let (fx, fy) = h.frac();
            (*h, Pos2::new(r.min.x + r.width() * fx, r.min.y + r.height() * fy))
        }).collect()
    }

    fn element_at(&mut self, ctx: &egui::Context, p: Pos2) -> Option<u64> {
        let elems = self.layout.elements.clone();
        for e in elems.iter().rev() {
            if self.hidden.contains(&e.uid) {
                continue;
            }
            let r = self.rect_of(ctx, e).expand(3.0 / self.zoom);
            if r.contains(p) {
                return Some(e.uid);
            }
        }
        None
    }

    fn bounds_of(&mut self, ctx: &egui::Context, uids: &[u64]) -> Rect {
        let mut b: Option<Rect> = None;
        for uid in uids {
            if let Some(i) = self.idx(*uid) {
                let e = self.layout.elements[i].clone();
                let r = self.rect_of(ctx, &e);
                b = Some(b.map_or(r, |x| x.union(r)));
            }
        }
        b.unwrap_or(Rect::NOTHING)
    }

    fn snap_delta(&mut self, bounds: Rect, delta: Vec2, others: &[Rect], alt: bool) -> Vec2 {
        self.guides.clear();
        if alt {
            return delta;
        }
        let mut d = delta;
        let r = bounds.translate(d);
        let scr = self.screen();
        let thr = 6.0 / self.zoom;
        if self.smart_guides {
            let mut xs = vec![0.0, scr.x / 2.0, scr.x];
            let mut ys = vec![0.0, scr.y / 2.0, scr.y];
            for o in others {
                xs.extend([o.min.x, o.center().x, o.max.x]);
                ys.extend([o.min.y, o.center().y, o.max.y]);
            }
            let best = |edges: [f32; 3], cands: &[f32]| -> Option<(f32, f32)> {
                let mut best: Option<(f32, f32)> = None;
                for e in edges {
                    for c in cands {
                        let diff = c - e;
                        if diff.abs() < thr && best.map_or(true, |b| diff.abs() < b.0.abs()) {
                            best = Some((diff, *c));
                        }
                    }
                }
                best
            };
            if let Some((diff, line)) = best([r.min.x, r.center().x, r.max.x], &xs) {
                d.x += diff;
                self.guides.push((true, line));
            }
            if let Some((diff, line)) = best([r.min.y, r.center().y, r.max.y], &ys) {
                d.y += diff;
                self.guides.push((false, line));
            }
        }
        if self.snap && self.grid >= 1.0 {
            let r2 = bounds.translate(d);
            if !self.guides.iter().any(|g| g.0) {
                d.x += (r2.min.x / self.grid).round() * self.grid - r2.min.x;
            }
            if !self.guides.iter().any(|g| !g.0) {
                d.y += (r2.min.y / self.grid).round() * self.grid - r2.min.y;
            }
        }
        d
    }

    fn apply_resize(&mut self, ctx: &egui::Context, uid: u64, handle: Handle, orig: Rect, pointer: Pos2, shift: bool, alt: bool) {
        let Some(i) = self.idx(uid) else { return };
        let mut e = self.layout.elements[i].clone();
        let mut p = pointer;
        if self.snap && !alt && self.grid >= 1.0 {
            p = Pos2::new((p.x / self.grid).round() * self.grid, (p.y / self.grid).round() * self.grid);
        }
        let (fx, fy) = handle.frac();
        let mut r = orig;
        if fx == 0.0 { r.min.x = p.x.min(orig.max.x - 4.0); }
        if fx == 1.0 { r.max.x = p.x.max(orig.min.x + 4.0); }
        if fy == 0.0 { r.min.y = p.y.min(orig.max.y - 4.0); }
        if fy == 1.0 { r.max.y = p.y.max(orig.min.y + 4.0); }
        let keep_aspect = (e.kind == "image" && !alt) || (shift && e.kind != "weather") || e.kind == "weather";
        if keep_aspect && orig.height() > 0.0 {
            let aspect = orig.width() / orig.height();
            if fx == 0.5 {
                let w = r.height() * aspect;
                r.min.x = orig.center().x - w / 2.0;
                r.max.x = orig.center().x + w / 2.0;
            } else {
                let h = r.width() / aspect;
                if fy == 0.0 { r.min.y = r.max.y - h; } else if fy == 1.0 { r.max.y = r.min.y + h; } else {
                    r.min.y = orig.center().y - h / 2.0;
                    r.max.y = orig.center().y + h / 2.0;
                }
            }
        }
        let (w, h) = (r.width().round().max(4.0), r.height().round().max(4.0));
        match e.kind.as_str() {
            "weather" => e.set("size", vnum(h as f64)),
            _ => {
                e.set("width", vnum(w as f64));
                e.set("height", vnum(h as f64));
            }
        }
        let (ax, ay) = anchor_frac(&e.anchor());
        let (w_eff, h_eff) = if e.kind == "weather" { (WEATHER_LABEL_W + WEATHER_GAP + h, h) } else { (w, h) };
        e.x = (r.min.x + w_eff * ax).round();
        e.y = (r.min.y + h_eff * ay).round();
        let _ = ctx;
        self.layout.elements[i] = e;
        self.touch();
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let avail = ui.available_rect_before_wrap();
        self.canvas_min = avail.min;
        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, BG_DARK);
        let scr = self.screen();

        if self.fit_pending {
            let margin = 40.0;
            let z = ((avail.width() - margin) / scr.x).min((avail.height() - margin) / scr.y).clamp(0.05, 8.0);
            self.zoom = z;
            self.origin = (avail.center() - avail.min) - scr * z / 2.0;
            self.fit_pending = false;
        }

        let (zoom_delta, scroll, mods, pointer, space) = ctx.input(|i| {
            (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers, i.pointer.hover_pos(), i.key_down(Key::Space))
        });
        if resp.hovered() {
            if (zoom_delta - 1.0).abs() > 1e-4 {
                if let Some(p) = pointer {
                    let org = avail.min + self.origin;
                    let before = self.to_layout(org, p);
                    self.zoom = (self.zoom * zoom_delta).clamp(0.05, 10.0);
                    self.origin = (p - avail.min) - before.to_vec2() * self.zoom;
                }
            } else if scroll != Vec2::ZERO {
                self.origin += scroll;
            }
        }
        let org = avail.min + self.origin;
        let screen_rect = Rect::from_min_size(org, scr * self.zoom);

        // screen + background
        painter.rect_filled(screen_rect.translate(Vec2::new(0.0, 4.0)), 0.0, Color32::from_black_alpha(90));
        painter.rect_filled(screen_rect, 0.0, self.color(Some(self.config.get_str("display", "background_color", "#1a0a1e")), "#1a0a1e"));
        let mut bg_img = self.config.get_str("display", "background_image", "");
        if let Some(first) = self.carousel_first() {
            bg_img = first;
        }
        if !bg_img.is_empty() {
            let path = self.resolve_asset(&bg_img);
            if let Some((id, nat)) = self.texture(&ctx, &path) {
                let fit = self.config.get_str("display", "background_fit", "stretch");
                let (mut rect, mut uv) = (screen_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)));
                if fit != "stretch" {
                    let k = if fit == "cover" { (scr.x / nat.x).max(scr.y / nat.y) } else { (scr.x / nat.x).min(scr.y / nat.y) };
                    let sz = nat * k;
                    if fit == "contain" {
                        rect = Rect::from_center_size(screen_rect.center(), sz * self.zoom);
                    } else {
                        let vis = Vec2::new(scr.x / sz.x, scr.y / sz.y);
                        uv = Rect::from_center_size(Pos2::new(0.5, 0.5), vis);
                    }
                }
                painter.with_clip_rect(screen_rect).image(id, rect, uv, Color32::WHITE);
            }
        }
        let clip = painter.with_clip_rect(avail.intersect(screen_rect));
        self.paint_grid(&clip, org, screen_rect);

        let elems = self.layout.elements.clone();
        for e in &elems {
            if !self.hidden.contains(&e.uid) {
                self.paint_element(&clip, &ctx, org, e);
            }
        }
        painter.rect_stroke(screen_rect, 0.0, Stroke::new(1.0, Color32::from_white_alpha(40)));

        // selection + handles
        let mut handle_hit: Option<(Handle, u64, Rect)> = None;
        if let Some(h_uid) = self.primary() {
            if let Some(i) = self.idx(h_uid) {
                let e = elems[i].clone();
                let r = self.rect_of(&ctx, &e);
                if Self::can_resize(&e.kind) && self.selected.len() == 1 && !self.locked.contains(&h_uid) {
                    for (h, pos) in self.handle_positions(r) {
                        let sp = self.to_screen(org, pos);
                        if let Some(pp) = pointer {
                            if pp.distance(sp) < 8.0 {
                                handle_hit = Some((h, h_uid, r));
                            }
                        }
                    }
                }
            }
        }
        for uid in self.selected.clone() {
            if let Some(i) = self.idx(uid) {
                let e = elems[i].clone();
                let r = self.rect_of(&ctx, &e);
                let sr = Rect::from_min_max(self.to_screen(org, r.min), self.to_screen(org, r.max));
                painter.rect_stroke(sr, 0.0, Stroke::new(1.5, SEL));
                painter.text(sr.left_top() - Vec2::new(0.0, 3.0), Align2::LEFT_BOTTOM, &e.id, FontId::proportional(11.0), SEL);
                if Self::can_resize(&e.kind) && self.selected.len() == 1 {
                    for (_, pos) in self.handle_positions(r) {
                        let sp = self.to_screen(org, pos);
                        let hr = Rect::from_center_size(sp, Vec2::splat(7.0));
                        painter.rect_filled(hr, 1.0, Color32::WHITE);
                        painter.rect_stroke(hr, 1.0, Stroke::new(1.0, SEL));
                    }
                }
            }
        }
        // hover outline
        if let (Some(pp), Drag::None) = (pointer, &self.drag) {
            if resp.hovered() && handle_hit.is_none() {
                let lp = self.to_layout(org, pp);
                if let Some(u) = self.element_at(&ctx, lp) {
                    if !self.selected.contains(&u) {
                        if let Some(i) = self.idx(u) {
                            let e = elems[i].clone();
                            let r = self.rect_of(&ctx, &e);
                            painter.rect_stroke(Rect::from_min_max(self.to_screen(org, r.min), self.to_screen(org, r.max)), 0.0,
                                Stroke::new(1.0, Color32::from_rgba_unmultiplied(90, 160, 255, 120)));
                        }
                    }
                }
            }
        }
        // guides
        for (vertical, v) in &self.guides {
            let stroke = Stroke::new(1.0, Color32::from_rgb(255, 80, 160));
            if *vertical {
                let x = org.x + v * self.zoom;
                painter.line_segment([Pos2::new(x, screen_rect.min.y), Pos2::new(x, screen_rect.max.y)], stroke);
            } else {
                let y = org.y + v * self.zoom;
                painter.line_segment([Pos2::new(screen_rect.min.x, y), Pos2::new(screen_rect.max.x, y)], stroke);
            }
        }

        // ── drag handling ──
        let press = ctx.input(|i| i.pointer.press_origin());
        if resp.drag_started_by(PointerButton::Middle) || (space && resp.drag_started_by(PointerButton::Primary)) {
            self.drag = Drag::Pan;
        } else if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pp) = press {
                let lp = self.to_layout(org, pp);
                let hit_handle = handle_hit.filter(|_| self.primary().is_some());
                if let Some((h, uid, r)) = hit_handle {
                    self.checkpoint();
                    self.drag = Drag::Resize { uid, handle: h, orig: r };
                } else if let Some(uid) = self.element_at(&ctx, lp) {
                    if !self.selected.contains(&uid) {
                        if mods.shift {
                            self.selected.push(uid);
                        } else {
                            self.selected = vec![uid];
                        }
                    }
                    let movable: Vec<u64> = self.selected.iter().copied().filter(|u| !self.locked.contains(u)).collect();
                    if movable.is_empty() {
                        self.drag = Drag::None;
                    } else {
                        let bounds = self.bounds_of(&ctx, &movable);
                        let orig: Vec<(u64, f32, f32)> = movable.iter().filter_map(|u| self.idx(*u).map(|i| (*u, self.layout.elements[i].x, self.layout.elements[i].y))).collect();
                        let mut others = vec![];
                        let all: Vec<Element> = self.layout.elements.clone();
                        let hidden = self.hidden.clone();
                        for o in all.iter().filter(|o| !movable.contains(&o.uid) && !hidden.contains(&o.uid)) {
                            others.push(self.rect_of(&ctx, o));
                        }
                        self.checkpoint();
                        self.drag = Drag::Move { start: lp, orig, bounds, others };
                    }
                } else {
                    if !mods.shift {
                        self.selected.clear();
                    }
                    self.drag = Drag::Marquee { start: lp, additive: mods.shift };
                }
            }
        }
        if let Some(pp) = pointer {
            let lp = self.to_layout(org, pp);
            match std::mem::replace(&mut self.drag, Drag::None) {
                Drag::Pan => {
                    if resp.dragged() {
                        self.origin += resp.drag_delta();
                        self.drag = Drag::Pan;
                    }
                }
                Drag::Move { start, orig, bounds, others } => {
                    if resp.dragged() {
                        let d = self.snap_delta(bounds, lp - start, &others, mods.alt);
                        for (uid, ox, oy) in &orig {
                            if let Some(i) = self.idx(*uid) {
                                self.layout.elements[i].x = (ox + d.x).round();
                                self.layout.elements[i].y = (oy + d.y).round();
                            }
                        }
                        self.touch();
                        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                        self.drag = Drag::Move { start, orig, bounds, others };
                    } else {
                        self.guides.clear();
                    }
                }
                Drag::Resize { uid, handle, orig } => {
                    if resp.dragged() {
                        self.apply_resize(&ctx, uid, handle, orig, lp, mods.shift, mods.alt);
                        ctx.set_cursor_icon(handle.cursor());
                        self.drag = Drag::Resize { uid, handle, orig };
                    }
                }
                Drag::Marquee { start, additive } => {
                    if resp.dragged() {
                        let m = Rect::from_two_pos(start, lp);
                        let sm = Rect::from_min_max(self.to_screen(org, m.min), self.to_screen(org, m.max));
                        painter.rect(sm, 0.0, Color32::from_rgba_unmultiplied(90, 160, 255, 30), Stroke::new(1.0, SEL));
                        let all = self.layout.elements.clone();
                        let mut sel: Vec<u64> = if additive { self.selected.clone() } else { vec![] };
                        for e in &all {
                            if !self.hidden.contains(&e.uid) && self.rect_of(&ctx, e).intersects(m) && !sel.contains(&e.uid) {
                                sel.push(e.uid);
                            }
                        }
                        self.selected = sel;
                        self.drag = Drag::Marquee { start, additive };
                    }
                }
                Drag::None => {
                    if let Some((h, _, _)) = handle_hit {
                        ctx.set_cursor_icon(h.cursor());
                    }
                }
            }
        } else {
            self.drag = Drag::None;
        }
        if resp.drag_stopped() {
            self.drag = Drag::None;
            self.guides.clear();
        }

        // click selection
        if resp.clicked() {
            if let Some(pp) = pointer {
                let lp = self.to_layout(org, pp);
                match self.element_at(&ctx, lp) {
                    Some(uid) => {
                        if mods.shift {
                            if let Some(pos) = self.selected.iter().position(|u| *u == uid) {
                                self.selected.remove(pos);
                            } else {
                                self.selected.push(uid);
                            }
                        } else {
                            self.selected = vec![uid];
                        }
                    }
                    None => {
                        if !mods.shift {
                            self.selected.clear();
                        }
                    }
                }
            }
        }
        if resp.double_clicked() && self.primary().is_some() {
            self.focus_text = true;
        }
        if resp.secondary_clicked() {
            if let Some(pp) = pointer {
                let lp = self.to_layout(org, pp);
                self.pending_drop_pos = Some(lp);
                if let Some(uid) = self.element_at(&ctx, lp) {
                    if !self.selected.contains(&uid) {
                        self.selected = vec![uid];
                    }
                }
            }
        }
        resp.context_menu(|ui| {
            let at = self.pending_drop_pos;
            if self.selected.is_empty() {
                if ui.button("Add text here").clicked() {
                    let item = self.library[0].clone();
                    self.add_from_library(&item, at);
                    ui.close_menu();
                }
                if ui.button("Add image here…").clicked() {
                    let item = self.library[1].clone();
                    self.add_from_library(&item, at);
                    ui.close_menu();
                }
                if ui.button("Fit view").clicked() {
                    self.fit_pending = true;
                    ui.close_menu();
                }
            } else {
                if ui.button("Duplicate   Ctrl+D").clicked() { self.duplicate_selected(); ui.close_menu(); }
                if ui.button("Bring to front").clicked() {
                    for u in self.selected.clone() { let n = self.layout.elements.len(); self.reorder(u, n); }
                    ui.close_menu();
                }
                if ui.button("Send to back").clicked() {
                    for u in self.selected.clone().into_iter().rev() { self.reorder(u, 0); }
                    ui.close_menu();
                }
                if ui.button("Center horizontally").clicked() { self.center_selected(&ctx, true); ui.close_menu(); }
                if ui.button("Center vertically").clicked() { self.center_selected(&ctx, false); ui.close_menu(); }
                ui.separator();
                if ui.button("Delete   Del").clicked() { self.delete_selected(); ui.close_menu(); }
            }
        });

        // drops from the library
        if let Some(p) = resp.dnd_release_payload::<LibPayload>() {
            if let Some(pp) = ctx.pointer_latest_pos() {
                let lp = self.to_layout(org, pp);
                let item = p.0.clone();
                self.add_from_library(&item, Some(lp));
            }
        }
        if let Some(p) = resp.dnd_release_payload::<WidgetInfo>() {
            if let Some(pp) = ctx.pointer_latest_pos() {
                let lp = self.to_layout(org, pp);
                self.add_plugin(&p, Some(lp));
            }
        }
        if egui::DragAndDrop::has_payload_of_type::<LibPayload>(&ctx) || egui::DragAndDrop::has_payload_of_type::<WidgetInfo>(&ctx) {
            if let Some(pp) = ctx.pointer_latest_pos() {
                let g = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("ghost")));
                let label = egui::DragAndDrop::payload::<LibPayload>(&ctx).map(|p| p.0.label.clone())
                    .or_else(|| egui::DragAndDrop::payload::<WidgetInfo>(&ctx).map(|w| w.label.clone())).unwrap_or_default();
                g.text(pp + Vec2::new(12.0, 12.0), Align2::LEFT_TOP, format!("＋ {label}"), FontId::proportional(14.0), Color32::WHITE);
            }
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(avail, 0.0, Color32::from_rgba_unmultiplied(90, 160, 255, 40));
            painter.text(avail.center(), Align2::CENTER_CENTER, "Drop images, GIFs, .yaml layouts or .py widgets", FontId::proportional(22.0), Color32::WHITE);
        }

        // hint
        if self.layout.elements.is_empty() {
            painter.text(screen_rect.center(), Align2::CENTER_CENTER, "Empty layout\nDrag items from the Library tab, or drop an image here",
                FontId::proportional(16.0), Color32::from_white_alpha(120));
        }
    }

    fn center_selected(&mut self, ctx: &egui::Context, horizontal: bool) {
        let scr = self.screen();
        let sel = self.selected.clone();
        self.checkpoint();
        for uid in sel {
            if let Some(i) = self.idx(uid) {
                let e = self.layout.elements[i].clone();
                let r = self.rect_of(ctx, &e);
                let (ax, ay) = anchor_frac(&e.anchor());
                if horizontal {
                    self.layout.elements[i].x = ((scr.x - r.width()) / 2.0 + r.width() * ax).round();
                } else {
                    self.layout.elements[i].y = ((scr.y - r.height()) / 2.0 + r.height() * ay).round();
                }
            }
        }
        self.touch();
    }
}

// ───────────────────────────── inspector widgets ─────────────────────────────

fn reset_button(ui: &mut egui::Ui, e: &mut Element, key: &str) -> bool {
    if e.props.contains_key(key) && ui.small_button("↺").on_hover_text("Back to default (removes the key)").clicked() {
        e.unset(key);
        return true;
    }
    false
}

fn prop_text(ui: &mut egui::Ui, e: &mut Element, key: &str, label: &str, hint: &str) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let mut v = e.s(key).unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(150.0)).changed() {
            if v.is_empty() { e.unset(key) } else { e.set(key, vstr(&v)) }
            changed = true;
        }
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

fn prop_f32(ui: &mut egui::Ui, e: &mut Element, key: &str, label: &str, default: f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let mut v = e.f(key).unwrap_or(default);
        let r = ui.add(egui::DragValue::new(&mut v).range(range).speed(0.5));
        if r.changed() {
            e.set(key, vnum(v as f64));
            changed = true;
        }
        if !e.props.contains_key(key) {
            ui.weak("(default)");
        }
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

fn prop_bool(ui: &mut egui::Ui, e: &mut Element, key: &str, label: &str, default: bool) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let mut v = e.b(key).unwrap_or(default);
        if ui.checkbox(&mut v, "").changed() {
            e.set(key, Value::Bool(v));
            changed = true;
        }
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

fn prop_choice(ui: &mut egui::Ui, e: &mut Element, key: &str, label: &str, default: &str, options: &[&str]) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let cur = e.s(key).unwrap_or_else(|| default.to_string());
        egui::ComboBox::from_id_salt((key, label)).selected_text(&cur).show_ui(ui, |ui| {
            for o in options {
                if ui.selectable_label(cur == *o, *o).clicked() {
                    e.set(key, vstr(o));
                    changed = true;
                }
            }
        });
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

fn prop_color(ui: &mut egui::Ui, app: &App, e: &mut Element, key: &str, label: &str, default: &str) -> bool {
    let mut changed = false;
    ui.label(label);
    ui.horizontal(|ui| {
        let raw = e.s(key).unwrap_or_else(|| default.to_string());
        let is_accent = raw.eq_ignore_ascii_case("accent");
        let mut c = app.color(Some(raw.clone()), default);
        if ui.color_edit_button_srgba(&mut c).changed() {
            e.set(key, vstr(&to_hex(c)));
            changed = true;
        }
        let mut hex = if is_accent { "accent".to_string() } else { raw };
        if ui.add(egui::TextEdit::singleline(&mut hex).desired_width(70.0)).changed() {
            e.set(key, vstr(hex.trim()));
            changed = true;
        }
        if ui.selectable_label(is_accent, "accent").on_hover_text("Follow display.accent_color from config.yaml").clicked() {
            e.set(key, vstr("accent"));
            changed = true;
        }
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

fn value_row(ui: &mut egui::Ui, e: &mut Element, key: &str, default: Option<&Value>) -> bool {
    let mut changed = false;
    ui.label(key);
    ui.horizontal(|ui| {
        let cur = e.props.get(key).cloned().or_else(|| default.cloned()).unwrap_or(Value::Null);
        match &cur {
            Value::Bool(b) => {
                let mut v = *b;
                if ui.checkbox(&mut v, "").changed() {
                    e.set(key, Value::Bool(v));
                    changed = true;
                }
            }
            Value::Number(n) => {
                let mut v = n.as_f64().unwrap_or(0.0);
                if ui.add(egui::DragValue::new(&mut v).speed(0.5)).changed() {
                    e.set(key, vnum(v));
                    changed = true;
                }
            }
            other => {
                let mut s = value_to_string(other);
                if ui.add(egui::TextEdit::singleline(&mut s).desired_width(150.0)).changed() {
                    e.set(key, parse_scalar(&s));
                    changed = true;
                }
            }
        }
        if !e.props.contains_key(key) {
            ui.weak("(default)");
        }
        changed |= reset_button(ui, e, key);
    });
    ui.end_row();
    changed
}

// ───────────────────────────── panels ─────────────────────────────

impl App {
    fn title(&self) -> String {
        let name = self.layout_path.file_name().and_then(|n| n.to_str()).unwrap_or("layout");
        format!("ft_lock editor — {}{}", name, if self.dirty || !self.config.changed.is_empty() { " •" } else { "" })
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.add(egui::Button::new("New layout").shortcut_text("")).clicked() {
                        if self.dirty { self.confirm = Some(Confirm::NewLayout) } else { self.new_layout() }
                        ui.close_menu();
                    }
                    if ui.button("Open…").clicked() { self.open_layout(); ui.close_menu(); }
                    if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() { self.save_all(); ui.close_menu(); }
                    if ui.button("Save as…").clicked() { self.save_as(); ui.close_menu(); }
                    if ui.button("Reload from disk").clicked() {
                        if self.dirty { self.confirm = Some(Confirm::Reload) } else { let p = self.layout_path.clone(); self.load_layout_file(&p); }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); ui.close_menu(); }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() { self.do_undo(); ui.close_menu(); }
                    if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo").shortcut_text("Ctrl+Shift+Z")).clicked() { self.do_redo(); ui.close_menu(); }
                    ui.separator();
                    if ui.add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() { self.duplicate_selected(); ui.close_menu(); }
                    if ui.add(egui::Button::new("Delete").shortcut_text("Del")).clicked() { self.delete_selected(); ui.close_menu(); }
                    if ui.add(egui::Button::new("Select all").shortcut_text("Ctrl+A")).clicked() {
                        self.selected = self.layout.elements.iter().map(|e| e.uid).collect();
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.add(egui::Button::new("Fit to window").shortcut_text("F")).clicked() { self.fit_pending = true; ui.close_menu(); }
                    if ui.button("100 %").clicked() { self.zoom = 1.0; ui.close_menu(); }
                    ui.separator();
                    ui.checkbox(&mut self.show_grid, "Show grid");
                    ui.checkbox(&mut self.snap, "Snap to grid");
                    ui.checkbox(&mut self.smart_guides, "Smart guides");
                    ui.checkbox(&mut self.sample_text, "Show sample values for {placeholders}");
                    ui.horizontal(|ui| {
                        ui.label("Grid");
                        ui.add(egui::DragValue::new(&mut self.grid).range(1.0..=200.0).suffix(" px"));
                    });
                });
                ui.menu_button("Help", |ui| {
                    ui.label("Drag: move · Alt: no snapping · Shift: add to selection / keep aspect");
                    ui.label("Arrows: nudge 1px (Shift = 10px) · Ctrl+scroll: zoom · Space/middle-drag: pan");
                    ui.label("Drop images/GIFs on the canvas, .py files into the widget library");
                });
            });
            ui.horizontal(|ui| {
                if ui.button("💾 Save").on_hover_text("Ctrl+S").clicked() { self.save_all(); }
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("↶")).on_hover_text("Undo (Ctrl+Z)").clicked() { self.do_undo(); }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("↷")).on_hover_text("Redo (Ctrl+Shift+Z)").clicked() { self.do_redo(); }
                ui.separator();
                let label = if self.preview.is_some() { "■ Stop preview" } else { "▶ Live preview" };
                if ui.button(label).on_hover_text("Runs ft_lock.py --preview (safe: never locks) and reloads on every save").clicked() {
                    self.toggle_preview();
                }
                ui.checkbox(&mut self.autosave, "Auto-save").on_hover_text("Save ~0.6 s after every change — the preview updates live");
                ui.separator();
                if ui.button("Fit").clicked() { self.fit_pending = true; }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                ui.separator();
                let mut s = self.layout.screen.clone().unwrap_or(Screen { width: 1920, height: 1080 });
                ui.label("Design size");
                let a = ui.add(egui::DragValue::new(&mut s.width).range(320..=7680));
                ui.label("×");
                let b = ui.add(egui::DragValue::new(&mut s.height).range(240..=4320));
                if a.changed() || b.changed() {
                    self.layout.screen = Some(s);
                    self.touch();
                    self.fit_pending = true;
                }
            });
        });
    }

    fn new_layout(&mut self) {
        self.checkpoint();
        self.layout = Layout::empty();
        self.selected.clear();
        self.header.clear();
        self.touch();
    }

    fn bottom_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let fresh = self.status_at.elapsed() < Duration::from_secs(12);
                ui.label(if fresh { self.status.clone() } else { format!("{}", self.layout_path.display()) });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{} elements · {} selected", self.layout.elements.len(), self.selected.len()));
                });
            });
        });
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left").default_width(270.0).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Layers, "Layers");
                ui.selectable_value(&mut self.tab, Tab::Library, "Library");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Config");
            });
            ui.separator();
            match self.tab {
                Tab::Layers => self.layers_ui(ui),
                Tab::Library => self.library_ui(ui),
                Tab::Settings => self.settings_ui(ui),
            }
        });
    }

    fn layers_ui(&mut self, ui: &mut egui::Ui) {
        ui.weak("Top of the list = drawn on top. Drag rows to reorder.");
        let mut select: Option<(u64, bool)> = None;
        let mut toggle_hidden = None;
        let mut toggle_locked = None;
        let mut delete = None;
        let mut reorder: Option<(u64, usize)> = None;
        let n = self.layout.elements.len();
        let ids: Vec<String> = self.layout.elements.iter().map(|e| e.id.clone()).collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (rev_i, e) in self.layout.elements.iter().enumerate().rev() {
                let selected = self.selected.contains(&e.uid);
                let dup = ids.iter().filter(|i| **i == e.id).count() > 1;
                ui.horizontal(|ui| {
                    if ui.small_button(if self.hidden.contains(&e.uid) { "🚫" } else { "👁" }).on_hover_text("Hide in the editor only").clicked() {
                        toggle_hidden = Some(e.uid);
                    }
                    if ui.small_button(if self.locked.contains(&e.uid) { "🔒" } else { "🔓" }).on_hover_text("Lock position (editor only)").clicked() {
                        toggle_locked = Some(e.uid);
                    }
                    let icon = match e.kind.as_str() { "text" => "T", "image" => "🖼", "rect" => "▭", "input" => "⌨", "weather" => "☁", _ => "◈" };
                    let mut label = egui::RichText::new(format!("{icon}  {}", e.id));
                    if dup { label = label.color(Color32::LIGHT_RED); }
                    let row = ui.add(egui::SelectableLabel::new(selected, label)).interact(Sense::click_and_drag());
                    if row.clicked() { select = Some((e.uid, ui.input(|i| i.modifiers.shift))); }
                    if row.drag_started() { egui::DragAndDrop::set_payload(ui.ctx(), RowPayload(e.uid)); }
                    if let Some(p) = row.dnd_release_payload::<RowPayload>() { reorder = Some((p.0, rev_i)); }
                    if row.dnd_hover_payload::<RowPayload>().is_some() {
                        ui.painter().hline(row.rect.x_range(), row.rect.top(), Stroke::new(2.0, SEL));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Delete").clicked() { delete = Some(e.uid); }
                    });
                });
            }
            if n == 0 { ui.weak("No elements yet — open the Library tab."); }
        });
        if let Some((u, add)) = select {
            if add { if !self.selected.contains(&u) { self.selected.push(u) } } else { self.selected = vec![u] }
        }
        if let Some(u) = toggle_hidden { if !self.hidden.remove(&u) { self.hidden.insert(u); } }
        if let Some(u) = toggle_locked { if !self.locked.remove(&u) { self.locked.insert(u); } }
        if let Some(u) = delete { self.selected = vec![u]; self.locked.remove(&u); self.delete_selected(); }
        if let Some((u, i)) = reorder { self.reorder(u, i); }
    }

    fn library_ui(&mut self, ui: &mut egui::Ui) {
        ui.weak("Click to add, or drag onto the canvas.");
        let mut add: Option<LibItem> = None;
        let mut add_widget: Option<WidgetInfo> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut last_group = "";
            for item in self.library.clone() {
                if item.group != last_group {
                    ui.add_space(6.0);
                    ui.strong(item.group);
                    last_group = item.group;
                }
                let r = ui.add_sized([ui.available_width(), 24.0], egui::Button::new(&item.label).sense(Sense::click_and_drag())).on_hover_text(&item.hint);
                if r.clicked() { add = Some(item.clone()); }
                if r.drag_started() { egui::DragAndDrop::set_payload(ui.ctx(), LibPayload(item.clone())); }
            }
            ui.add_space(10.0);
            ui.strong("Your widgets  (coded in Python)");
            if self.widgets.is_empty() {
                ui.weak("None found in the widgets folder.");
            }
            for w in self.widgets.clone() {
                let label = if w.label.is_empty() { w.name.clone() } else { w.label.clone() };
                let r = ui.add_sized([ui.available_width(), 24.0], egui::Button::new(format!("◈ {label}")).sense(Sense::click_and_drag()))
                    .on_hover_text(if let Some(err) = &w.error { format!("ERROR: {err}") } else { w.description.clone() });
                if r.clicked() { add_widget = Some(w.clone()); }
                if r.drag_started() { egui::DragAndDrop::set_payload(ui.ctx(), w.clone()); }
                if let Some(err) = &w.error { ui.colored_label(Color32::LIGHT_RED, err); }
            }
            ui.horizontal(|ui| {
                if ui.small_button("↻ Refresh").clicked() { self.refresh_widgets(); }
                if ui.small_button("Open folder").clicked() {
                    let _ = Command::new("xdg-open").arg(self.widgets_dir()).spawn();
                }
            });
            ui.separator();
            ui.strong("New widget");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_widget.0).hint_text("name").desired_width(100.0));
                ui.checkbox(&mut self.new_widget.1, "drawing");
                if ui.button("Create").clicked() {
                    let name = self.new_widget.0.trim().to_string();
                    if !name.is_empty() {
                        let mut cmd = Command::new("python3");
                        cmd.arg(self.script_path()).arg("--new-widget").arg(&name);
                        if self.new_widget.1 { cmd.arg("--class"); }
                        if let Some(c) = &self.config.path { cmd.arg("--config").arg(c); }
                        match cmd.output() {
                            Ok(o) if o.status.success() => {
                                self.say(String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("created").to_string());
                                self.new_widget.0.clear();
                                self.refresh_widgets();
                            }
                            Ok(o) => self.say(String::from_utf8_lossy(&o.stdout).to_string()),
                            Err(e) => self.say(format!("python3: {e}")),
                        }
                    }
                }
            });
            ui.weak("Then edit the .py file in any editor: the preview reloads it automatically.");
        });
        if let Some(i) = add { self.add_from_library(&i, None); }
        if let Some(w) = add_widget { self.add_plugin(&w, None); }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let path_txt = self.config.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(no config file — pick one)".into());
        ui.weak(path_txt);
        if self.config.path.is_none() {
            if ui.button("Create config.yaml next to the layout").clicked() {
                self.config.path = Some(self.layout_dir().join("config.yaml"));
            }
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut changes: Vec<(&str, &str, Value)> = vec![];
            let text_fields: [(&str, &str, &str, &str); 6] = [
                ("display", "username", "Username", "empty = $USER"),
                ("display", "message", "Message", "{message}"),
                ("display", "font_family", "Font", "Ubuntu"),
                ("lock", "password_text", "Password preview", "e.g. Hello"),
                ("weather", "city", "City", "Paris"),
                ("weather", "api_key", "OpenWeather key", ""),
            ];
            ui.strong("Display");
            egui::Grid::new("cfg_display").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                for (sec, key, label, hint) in text_fields.iter().take(3) {
                    ui.label(*label);
                    let mut v = self.config.get_str(sec, key, "");
                    if ui.add(egui::TextEdit::singleline(&mut v).hint_text(*hint).desired_width(150.0)).changed() {
                        changes.push((sec, key, vstr(&v)));
                    }
                    ui.end_row();
                }
                for (key, label, default) in [("accent_color", "Accent color", "#5a7ab0"), ("background_color", "Background", "#1a0a1e")] {
                    ui.label(label);
                    ui.horizontal(|ui| {
                        let cur = self.config.get_str("display", key, default);
                        let mut c = parse_hex(&cur).unwrap_or(Color32::WHITE);
                        if ui.color_edit_button_srgba(&mut c).changed() { changes.push(("display", key, vstr(&to_hex(c)))); }
                        ui.label(cur);
                    });
                    ui.end_row();
                }
                ui.label("Background image");
                ui.horizontal(|ui| {
                    let mut v = self.config.get_str("display", "background_image", "");
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(110.0)).changed() { changes.push(("display", "background_image", vstr(&v))); }
                    if ui.button("…").clicked() {
                        if let Ok(Some(p)) = pick_file("Background image", Some("Images | *.png *.jpg *.jpeg *.gif *.webp *.bmp")) {
                            changes.push(("display", "background_image", vstr(&p.to_string_lossy())));
                        }
                    }
                });
                ui.end_row();
                ui.label("Background fit");
                let cur = self.config.get_str("display", "background_fit", "stretch");
                egui::ComboBox::from_id_salt("bgfit").selected_text(&cur).show_ui(ui, |ui| {
                    for o in ["stretch", "cover", "contain"] {
                        if ui.selectable_label(cur == o, o).clicked() { changes.push(("display", "background_fit", vstr(o))); }
                    }
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.strong("Lock behaviour");
            egui::Grid::new("cfg_lock").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                for (key, label, default) in [("timeout", "Sleep after (s)", 5.0), ("logout_timeout", "Escape logs out after (min)", 30.0), ("logout_timeout_force", "Forced logout after (min)", 42.0)] {
                    ui.label(label);
                    let mut v = self.config.get("lock", key).and_then(|v| v.as_f64()).unwrap_or(default);
                    if ui.add(egui::DragValue::new(&mut v).range(1.0..=10000.0)).changed() { changes.push(("lock", key, vnum(v))); }
                    ui.end_row();
                }
                ui.label("Escape may log out");
                let mut b = self.config.get("lock", "allow_escape_logout").and_then(|v| v.as_bool()).unwrap_or(true);
                if ui.checkbox(&mut b, "").changed() { changes.push(("lock", "allow_escape_logout", Value::Bool(b))); }
                ui.end_row();
                let (sec, key, label, hint) = text_fields[3];
                ui.label(label);
                let mut v = self.config.get_str(sec, key, "");
                if ui.add(egui::TextEdit::singleline(&mut v).hint_text(hint).desired_width(150.0)).changed() { changes.push((sec, key, vstr(&v))); }
                ui.end_row();
            });
            ui.add_space(8.0);
            ui.strong("Weather");
            egui::Grid::new("cfg_weather").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                for (sec, key, label, hint) in text_fields.iter().skip(4) {
                    ui.label(*label);
                    let mut v = self.config.get_str(sec, key, "");
                    let mut te = egui::TextEdit::singleline(&mut v).hint_text(*hint).desired_width(150.0);
                    if *key == "api_key" { te = te.password(true); }
                    if ui.add(te).changed() { changes.push((sec, key, vstr(&v))); }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.strong("Background carousel");
            egui::Grid::new("cfg_carousel").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Enabled");
                let mut on = self.config.get("carousel", "enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                if ui.checkbox(&mut on, "").changed() { changes.push(("carousel", "enabled", Value::Bool(on))); }
                ui.end_row();
                ui.label("Folder");
                ui.horizontal(|ui| {
                    let mut v = self.config.get_str("carousel", "folder", "");
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(110.0)).changed() { changes.push(("carousel", "folder", vstr(&v))); }
                    if ui.button("…").clicked() {
                        match pick_folder("Wallpaper folder") {
                            Ok(Some(p)) => changes.push(("carousel", "folder", vstr(&p.to_string_lossy()))),
                            Ok(None) => {}
                            Err(e) => self.status = e,
                        }
                    }
                });
                ui.end_row();
                for (key, label, options, default) in [
                    ("mode", "Mode", ["launch", "interval"], "interval"),
                    ("order", "Order", ["sequential", "random"], "sequential"),
                    ("transition", "Transition", ["fade", "none"], "fade"),
                ] {
                    ui.label(label);
                    let cur = self.config.get_str("carousel", key, default);
                    egui::ComboBox::from_id_salt(("car", key)).selected_text(&cur).show_ui(ui, |ui| {
                        for o in options {
                            if ui.selectable_label(cur == o, o).clicked() { changes.push(("carousel", key, vstr(o))); }
                        }
                    });
                    ui.end_row();
                }
                ui.label("Interval (s)");
                let mut iv = self.config.get("carousel", "interval").and_then(|v| v.as_f64()).unwrap_or(60.0);
                if ui.add(egui::DragValue::new(&mut iv).range(2.0..=86400.0)).changed() { changes.push(("carousel", "interval", vnum(iv))); }
                ui.end_row();
            });
            ui.weak("launch: another image at each lock. interval: also rotates while locked. A list of single files (`images:`) is edited in config.yaml.");
            ui.add_space(8.0);
            ui.weak("`variables:` (custom {placeholders}) are edited directly in config.yaml.");
            if ui.button("💾 Save config").clicked() {
                match self.config.save() {
                    Ok(note) => self.say(format!("Config saved ({note})")),
                    Err(e) => self.say(format!("Config save failed: {e}")),
                }
            }
            for (s, k, v) in changes {
                self.config.set(s, k, v);
            }
        });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("right").default_width(290.0).resizable(true).show(ctx, |ui| {
            ui.heading("Inspector");
            ui.separator();
            let Some(uid) = self.primary() else {
                ui.weak("Nothing selected.");
                ui.add_space(8.0);
                ui.label("• Click an element to edit it");
                ui.label("• Drag to move, handles to resize");
                ui.label("• Shift-click or drag a box to select several");
                ui.label("• Double-click an element to edit its text");
                ui.label("• Drop images / GIFs straight onto the canvas");
                return;
            };
            let Some(i) = self.idx(uid) else { return };
            if self.selected.len() > 1 {
                ui.label(format!("{} elements selected", self.selected.len()));
                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() { self.duplicate_selected(); }
                    if ui.button("Delete").clicked() { self.delete_selected(); }
                });
                ui.separator();
            }
            let before = self.layout.elements.clone();
            let mut e = self.layout.elements[i].clone();
            let taken: Vec<String> = self.layout.elements.iter().filter(|o| o.uid != uid).map(|o| o.id.clone()).collect();
            let changed = egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui, &mut e, &taken)).inner;
            if changed {
                self.layout.elements[i] = e;
                self.checkpoint_coalesced(before);
                self.touch();
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui, e: &mut Element, taken: &[String]) -> bool {
        let mut changed = false;
        let mut shown: Vec<&str> = vec!["anchor", "visible", "seconds", "enabled"];
        let dup = taken.contains(&e.id);
        egui::Grid::new("insp_common").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("id");
            let r = ui.add(egui::TextEdit::singleline(&mut e.id).desired_width(150.0).text_color_opt(dup.then_some(Color32::LIGHT_RED)));
            changed |= r.changed();
            ui.end_row();
            ui.label("type");
            ui.label(egui::RichText::new(&e.kind).monospace());
            ui.end_row();
            ui.label("x / y");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut e.x).speed(1.0)).changed();
                changed |= ui.add(egui::DragValue::new(&mut e.y).speed(1.0)).changed();
            });
            ui.end_row();
        });
        if dup { ui.colored_label(Color32::LIGHT_RED, "Duplicate id"); }
        if uses_anchor(&e.kind) {
            ui.add_space(4.0);
            ui.label("Anchor (which point of the element sits at x/y)");
            let cur = e.anchor();
            egui::Grid::new("anchor_grid").spacing([2.0, 2.0]).show(ui, |ui| {
                for row in [["nw", "n", "ne"], ["w", "center", "e"], ["sw", "s", "se"]] {
                    for a in row {
                        let label = match a { "center" => "●", "nw" => "↖", "n" => "↑", "ne" => "↗", "w" => "←", "e" => "→", "sw" => "↙", "s" => "↓", _ => "↘" };
                        if ui.add_sized([28.0, 24.0], egui::SelectableLabel::new(cur == a, label)).on_hover_text(a).clicked() {
                            e.set("anchor", vstr(a));
                            changed = true;
                        }
                    }
                    ui.end_row();
                }
            });
        }
        ui.add_space(6.0);
        ui.separator();
        let app: &App = self;
        let want_focus = app.focus_text;
        let mut clear_focus = false;
        let mut widget_keys: Vec<String> = vec![];
        egui::Grid::new("insp_props").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| match e.kind.clone().as_str() {
            "text" => {
                shown.extend(["text", "color", "size", "font", "bold", "italic", "shadow", "width", "align", "command", "interval", "shell"]);
                ui.label("text");
                ui.vertical(|ui| {
                    let mut v = e.s("text").unwrap_or_default();
                    let mut te = egui::TextEdit::multiline(&mut v).desired_rows(3).desired_width(180.0);
                    if want_focus { te = te.id(egui::Id::new("text_edit_focus")); }
                    let r = ui.add(te);
                    if want_focus { r.request_focus(); clear_focus = true; }
                    if r.changed() { e.set("text", vstr(&v)); changed = true; }
                    ui.menu_button("＋ placeholder", |ui| {
                        for p in PLACEHOLDERS {
                            if ui.button(p).clicked() {
                                let mut t = e.s("text").unwrap_or_default();
                                t.push_str(p);
                                e.set("text", vstr(&t));
                                changed = true;
                                ui.close_menu();
                            }
                        }
                    });
                });
                ui.end_row();
                changed |= prop_color(ui, app, e, "color", "color", "#ffffff");
                changed |= prop_f32(ui, e, "size", "size (pt)", 18.0, 4.0..=400.0);
                ui.label("font");
                ui.horizontal(|ui| {
                    let mut v = e.s("font").unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut v).hint_text("config font").desired_width(100.0)).changed() {
                        if v.is_empty() { e.unset("font") } else { e.set("font", vstr(&v)) }
                        changed = true;
                    }
                    ui.menu_button("▾", |ui| {
                        for f in FONTS {
                            if ui.button(f).clicked() { e.set("font", vstr(f)); changed = true; ui.close_menu(); }
                        }
                    });
                });
                ui.end_row();
                changed |= prop_bool(ui, e, "bold", "bold", false);
                changed |= prop_bool(ui, e, "italic", "italic", false);
                changed |= prop_bool(ui, e, "shadow", "shadow", false);
                changed |= prop_f32(ui, e, "width", "wrap width", 0.0, 0.0..=8000.0);
                changed |= prop_text(ui, e, "command", "command", "uname -r");
                if e.props.contains_key("command") {
                    changed |= prop_f32(ui, e, "interval", "refresh (s)", 5.0, 0.5..=3600.0);
                    changed |= prop_bool(ui, e, "shell", "use shell", false);
                }
            }
            "image" => {
                shown.extend(["path", "width", "height", "fit"]);
                ui.label("file");
                ui.horizontal(|ui| {
                    let mut v = e.s("path").unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut v).desired_width(110.0)).changed() { e.set("path", vstr(&v)); changed = true; }
                    if ui.button("…").clicked() {
                        if let Ok(Some(p)) = pick_file("Choose an image or GIF", Some("Images | *.png *.jpg *.jpeg *.gif *.webp *.bmp")) {
                            e.set("path", vstr(&app.path_for_yaml(&p)));
                            changed = true;
                        }
                    }
                });
                ui.end_row();
                changed |= prop_f32(ui, e, "width", "width", 0.0, 0.0..=8000.0);
                changed |= prop_f32(ui, e, "height", "height", 0.0, 0.0..=8000.0);
                changed |= prop_choice(ui, e, "fit", "fit", "contain", &["contain", "stretch"]);
                ui.label("");
                if ui.small_button("Natural size").clicked() { e.unset("width"); e.unset("height"); changed = true; }
                ui.end_row();
            }
            "rect" => {
                shown.extend(["width", "height", "color", "border", "border_width", "radius"]);
                changed |= prop_f32(ui, e, "width", "width", 100.0, 1.0..=8000.0);
                changed |= prop_f32(ui, e, "height", "height", 40.0, 1.0..=8000.0);
                changed |= prop_color(ui, app, e, "color", "fill", "#ffffff");
                changed |= prop_color(ui, app, e, "border", "border", "accent");
                changed |= prop_f32(ui, e, "border_width", "border width", 1.0, 1.0..=50.0);
                changed |= prop_f32(ui, e, "radius", "corner radius", 0.0, 0.0..=500.0);
            }
            "input" => {
                shown.extend(["width", "height", "radius", "color", "bg", "text_color", "size", "gloss", "font", "status_size", "error_color"]);
                changed |= prop_f32(ui, e, "width", "width", 240.0, 40.0..=3000.0);
                changed |= prop_f32(ui, e, "height", "height", 32.0, 12.0..=500.0);
                changed |= prop_f32(ui, e, "radius", "corner radius", 10.0, 0.0..=200.0);
                changed |= prop_color(ui, app, e, "color", "border", "accent");
                changed |= prop_color(ui, app, e, "bg", "background", "#1a2840");
                changed |= prop_color(ui, app, e, "text_color", "text color", "#c8d2e6");
                changed |= prop_f32(ui, e, "size", "font size", 13.0, 6.0..=100.0);
                changed |= prop_bool(ui, e, "gloss", "gloss", true);
                changed |= prop_color(ui, app, e, "error_color", "error color", "#e05050");
            }
            "weather" => {
                shown.extend(["size", "color", "text_color", "popup_bg"]);
                changed |= prop_f32(ui, e, "size", "icon size", 46.0, 16.0..=400.0);
                changed |= prop_color(ui, app, e, "color", "icon color", "accent");
                changed |= prop_color(ui, app, e, "text_color", "text color", "#ffffff");
            }
            other => {
                let info = app.widgets.iter().find(|w| w.name == other).cloned();
                match &info {
                    Some(w) => {
                        ui.label("widget");
                        ui.label(egui::RichText::new(format!("{} ({})", w.label, w.name)).strong());
                        ui.end_row();
                        for (k, v) in &w.params {
                            let dv = json_to_value(v);
                            if k.contains("color") {
                                changed |= prop_color(ui, app, e, k, k, &value_to_string(&dv));
                            } else {
                                changed |= value_row(ui, e, k, Some(&dv));
                            }
                            widget_keys.push(k.clone());
                        }
                    }
                    None => {
                        ui.colored_label(Color32::LIGHT_RED, format!("widgets/{other}.py not found"));
                        ui.end_row();
                    }
                }
            }
        });
        if clear_focus {
            self.focus_text = false;
        }
        // visibility
        ui.add_space(6.0);
        ui.separator();
        egui::Grid::new("insp_vis").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            changed |= prop_choice(ui, e, "visible", "visible", "always", &VISIBLE);
            if matches!(e.s("visible").as_deref(), Some("after_seconds") | Some("before_seconds")) {
                changed |= prop_f32(ui, e, "seconds", "seconds", 60.0, 0.0..=86400.0);
            }
            changed |= prop_bool(ui, e, "enabled", "enabled", true);
        });
        // other keys (unknown / custom)
        let others: Vec<String> = e.props.keys().filter_map(|k| k.as_str().map(|s| s.to_string()))
            .filter(|k| !shown.contains(&k.as_str()) && !widget_keys.contains(k)).collect();
        ui.add_space(6.0);
        egui::CollapsingHeader::new(format!("Other keys ({})", others.len())).default_open(!others.is_empty()).show(ui, |ui| {
            egui::Grid::new("insp_other").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                for k in &others {
                    changed |= value_row(ui, e, k, None);
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_param.0).hint_text("key").desired_width(70.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_param.1).hint_text("value").desired_width(80.0));
                if ui.button("＋").clicked() && !self.new_param.0.trim().is_empty() {
                    let key = self.new_param.0.trim().to_string();
                    e.set(&key, parse_scalar(&self.new_param.1));
                    self.new_param = (String::new(), String::new());
                    changed = true;
                }
            });
        });
        changed
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let msg = match c {
            Confirm::Close => "You have unsaved changes.",
            Confirm::NewLayout => "Discard the current layout and start empty?",
            Confirm::Reload => "Discard unsaved changes and reload from disk?",
        };
        let mut action: Option<&str> = None;
        egui::Window::new("Unsaved changes").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(msg);
            ui.horizontal(|ui| {
                if matches!(self.confirm, Some(Confirm::Close)) && ui.button("Save").clicked() { action = Some("save"); }
                if ui.button("Discard").clicked() { action = Some("discard"); }
                if ui.button("Cancel").clicked() { action = Some("cancel"); }
            });
        });
        match action {
            Some("save") => {
                self.save_all();
                self.confirm = None;
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some("discard") => {
                match self.confirm.take() {
                    Some(Confirm::Close) => { self.allow_close = true; ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                    Some(Confirm::NewLayout) => self.new_layout(),
                    Some(Confirm::Reload) => { let p = self.layout_path.clone(); self.load_layout_file(&p); }
                    None => {}
                }
            }
            Some(_) => self.confirm = None,
            None => {}
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        if files.is_empty() {
            return;
        }
        let origin = self.canvas_min + self.origin;
        let at = ctx.pointer_latest_pos().map(|p| self.to_layout(origin, p));
        for (n, f) in files.iter().enumerate() {
            let Some(path) = &f.path else { continue };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => {
                    let pos = at.map(|p| p + Vec2::splat(n as f32 * 20.0));
                    self.add_image(path, pos);
                    self.say(format!("Added {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("image")));
                }
                "yaml" | "yml" => {
                    let p = path.clone();
                    self.load_layout_file(&p);
                }
                "py" => {
                    let dir = self.widgets_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    match path.file_name().map(|n| dir.join(n)) {
                        Some(dest) => match std::fs::copy(path, &dest) {
                            Ok(_) => {
                                self.say(format!("Installed widget {}", dest.display()));
                                self.refresh_widgets();
                                self.tab = Tab::Library;
                            }
                            Err(e) => self.say(format!("Could not install widget: {e}")),
                        },
                        None => {}
                    }
                }
                _ => self.say(format!("Don't know what to do with .{ext} files")),
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers { shift: true, ..Modifiers::COMMAND };
        if ctx.input_mut(|i| i.consume_key(cmd, Key::S)) { self.save_all(); }
        let typing = ctx.wants_keyboard_input();
        if typing {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(cmd_shift, Key::Z)) || ctx.input_mut(|i| i.consume_key(cmd, Key::Y)) { self.do_redo(); }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::Z)) { self.do_undo(); }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::D)) { self.duplicate_selected(); }
        if ctx.input_mut(|i| i.consume_key(cmd, Key::A)) { self.selected = self.layout.elements.iter().map(|e| e.uid).collect(); }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) { self.delete_selected(); }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F)) { self.fit_pending = true; }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) { self.selected.clear(); }
        for (key, dx, dy) in [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, -1.0), (Key::ArrowDown, 0.0, 1.0)] {
            if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, key)) { self.nudge(dx * 10.0, dy * 10.0); }
            else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) { self.nudge(dx, dy); }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ctx);
        self.shortcuts(ctx);
        if let Some(child) = &mut self.preview {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.preview = None;
                self.say("Preview closed");
            }
        }
        if self.autosave && self.dirty && matches!(self.drag, Drag::None) && self.last_change.elapsed() > Duration::from_millis(600) {
            self.save_layout();
        }
        if ctx.input(|i| i.viewport().close_requested()) && (self.dirty || !self.config.changed.is_empty()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm::Close);
        }
        let title = self.title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        self.top_bar(ctx);
        self.bottom_bar(ctx);
        self.left_panel(ctx);
        self.right_panel(ctx);
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG_DARK)).show(ctx, |ui| self.canvas_ui(ui));
        self.dialogs(ctx);
        if self.autosave && self.dirty || self.preview.is_some() {
            ctx.request_repaint_after(Duration::from_millis(300));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut c) = self.preview.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ───────────────────────────── main ─────────────────────────────

fn usage() {
    println!("ft-lock-layout-editor [LAYOUT.yaml] [--config config.yaml] [--project DIR]");
}

fn main() -> eframe::Result<()> {
    let mut layout: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut project: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--project" => project = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            _ => layout = Some(PathBuf::from(a)),
        }
    }
    let hints: Vec<PathBuf> = layout.iter().filter_map(|l| l.parent().map(|p| p.to_path_buf())).collect();
    let project = find_project(project, &hints);
    if config.is_none() {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        config = [home.join(".config/ft_lock/config.yaml"), project.join("config.yaml")].into_iter().find(|p| p.is_file());
    }
    let layout = layout.unwrap_or_else(|| {
        let cfg = Config::load(config.clone());
        let rel = cfg.top_str("layout").unwrap_or_else(|| "default.yaml".into());
        let p = PathBuf::from(&rel);
        if p.is_absolute() {
            p
        } else {
            let base = config.as_ref().and_then(|c| c.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| project.clone());
            if base.join(&p).exists() || !project.join(&p).exists() { base.join(p) } else { project.join(p) }
        }
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 900.0]).with_min_inner_size([900.0, 600.0]).with_title("ft_lock editor"),
        ..Default::default()
    };
    eframe::run_native("ft_lock layout editor", options, Box::new(move |cc| {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Ok(Box::new(App::new(layout, project, config)))
    }))
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# my header\nversion: 2\nscreen:\n  width: 1920\n  height: 1080\nelements:\n  - id: a\n    type: text\n    x: 10\n    y: 20.5\n    text: \"Locked {username}\"\n    color: accent\n    mystery: [1, 2]\n  - {id: w, type: battery, x: 5, y: 6, interval: 3}\n";

    #[test]
    fn layout_roundtrip_keeps_unknown_keys_and_types() {
        let (layout, upgraded) = parse_layout(SAMPLE).unwrap();
        assert!(!upgraded);
        assert_eq!(layout.elements.len(), 2);
        assert_eq!(layout.elements[0].y, 20.5);
        let out = serde_yaml::to_string(&layout).unwrap();
        let (again, _) = parse_layout(&out).unwrap();
        assert_eq!(again.elements[0].props.get("mystery"), layout.elements[0].props.get("mystery"));
        assert_eq!(again.elements[1].kind, "battery");
        assert_eq!(again.elements[1].f("interval"), Some(3.0));
        assert!(out.contains("x: 10\n"), "whole numbers must not become 10.0:\n{out}");
        assert_eq!(leading_comments(SAMPLE), "# my header\n");
    }

    #[test]
    fn v1_layout_is_upgraded_with_legacy_defaults() {
        let v1 = "version: 1\nelements:\n  - id: clock\n    type: text\n    x: 1900\n    y: 18\n    text: \"{clock}\"\n    color: \"#5a7ab0\"\n    size: 16\n    visible: always\n  - id: password_input\n    type: input\n    x: 230\n    y: 92\n";
        let (l, up) = parse_layout(v1).unwrap();
        assert!(up);
        assert_eq!(l.version, 2);
        assert_eq!(l.elements[0].anchor(), "ne");
        assert_eq!(l.elements[0].b("shadow"), Some(true));
        assert_eq!(l.elements[0].s("color").as_deref(), Some("#5a7ab0")); // explicit colour is kept
    }

    #[test]
    fn shipped_default_layout_loads() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../default.yaml")).unwrap();
        let (l, up) = parse_layout(&text).unwrap();
        assert!(!up);
        assert_eq!(l.elements.len(), 6);
        assert!(l.elements.iter().any(|e| e.kind == "input"));
    }

    #[test]
    fn placeholders() {
        let s = substitute("Hi {username}! {nope} {} { x } {a_1}", &|n| match n { "username" => Some("bob".into()), "a_1" => Some("1".into()), _ => None });
        assert_eq!(s, "Hi bob! {nope} {} { x } 1");
    }

    #[test]
    fn config_patch_preserves_comments() {
        let text = "version: 2\ndisplay:\n  message: \"Back sOOn..\"    # shown\n  accent_color: \"#5a7ab0\"\nweather:\n  city: Paris\n";
        let out = patch_yaml_scalar(text, "display", "message", &vstr("Hello there")).unwrap();
        let v0: Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(v0["display"]["message"], vstr("Hello there"));
        assert!(out.contains("# shown"), "{out}");
        let out = patch_yaml_scalar(&out, "lock", "timeout", &vnum(9.0)).unwrap();
        assert!(out.ends_with("lock:\n  timeout: 9\n"), "{out}");
        let out = patch_yaml_scalar(&out, "weather", "api_key", &vstr("k#1")).unwrap();
        let v: Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(v["weather"]["api_key"], vstr("k#1"));
        assert_eq!(v["weather"]["city"], vstr("Paris"));
        assert_eq!(v["lock"]["timeout"], vnum(9.0));
    }

    #[test]
    fn config_save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ftlock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yaml");
        std::fs::write(&p, "# top\nversion: 2\ndisplay:\n  message: \"x\"  # c\n").unwrap();
        let mut c = Config::load(Some(p.clone()));
        c.set("display", "message", vstr("y"));
        c.set("display", "accent_color", vstr("#ff0000"));
        c.save().unwrap();
        let back = std::fs::read_to_string(&p).unwrap();
        assert!(back.contains("# top") && back.contains("# c"), "{back}");
        let c2 = Config::load(Some(p));
        assert_eq!(c2.get_str("display", "message", ""), "y");
        assert_eq!(c2.get_str("display", "accent_color", ""), "#ff0000");
    }

    #[test]
    fn shipped_config_carousel_edit_keeps_comments() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../config.yaml");
        let dir = std::env::temp_dir().join(format!("ftlock_car_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.yaml");
        std::fs::copy(src, &p).unwrap();
        let mut c = Config::load(Some(p.clone()));
        c.set("carousel", "enabled", Value::Bool(true));
        c.set("carousel", "folder", vstr("~/Pictures/walls"));
        c.set("carousel", "mode", vstr("launch"));
        c.set("carousel", "interval", vnum(30.0));
        let note = c.save().unwrap();
        assert_eq!(note, "comments kept");
        let back = std::fs::read_to_string(&p).unwrap();
        assert!(back.contains("# launch   = a different image each time you lock"), "{back}");
        let c2 = Config::load(Some(p));
        assert_eq!(c2.get("carousel", "enabled").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(c2.get_str("carousel", "folder", ""), "~/Pictures/walls");
        assert_eq!(c2.get_str("carousel", "mode", ""), "launch");
        assert_eq!(c2.get("carousel", "interval").and_then(|v| v.as_f64()), Some(30.0));
        assert_eq!(c2.get_str("display", "message", ""), "Back sOOn..", "other sections untouched");
    }

    #[test]
    fn colours() {
        assert_eq!(parse_hex("#5a7ab0"), Some(Color32::from_rgb(0x5a, 0x7a, 0xb0)));
        assert_eq!(parse_hex("#fff"), Some(Color32::from_rgb(255, 255, 255)));
        assert_eq!(parse_hex("zzz"), None);
        assert_eq!(to_hex(Color32::from_rgb(1, 2, 3)), "#010203");
    }
}