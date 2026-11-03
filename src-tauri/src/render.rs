//! The render loop.
//!
//! The expensive steps are skipped whenever their inputs did not change. Widgets
//! are redrawn only when a displayed value differs, and the frame is composited
//! and JPEG-encoded only when the background or the widget layer changed. A static
//! layout therefore encodes about once a second, however high the target FPS is.

use crate::device::{Frame, FrameSlot, Panel};
use crate::lyricview::{active_index, ArtView, LyricView};
use crate::media::{self, parse_color};
use crate::music::{Music, SharedMusic};
use crate::model::{Background, Profile, Widget};
use crate::sensors::SharedSensors;
use chrono::{Local, Timelike};
use fontdue::{Font, FontSettings};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, Paint, PathBuilder, Pixmap, PixmapPaint, Point, Rect,
    SpreadMode, Stroke, Transform,
};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStats {
    pub encodes_per_sec: f32,
    pub render_ms: f32,
    pub encode_ms: f32,
    pub frame_kb: f32,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub seq: u64,
    pub profile: String,
    pub reason: String,
}

/// Values the rules thread computes for the renderer.
#[derive(Clone, Debug, Default)]
pub struct Live {
    pub profile: Option<Profile>,
    pub reason: String,
    /// Brightness cap from the night schedule (0..=100). 0 means a black panel.
    pub brightness_cap: u32,
    pub alert: Option<String>,
    pub media_title: String,
    pub media_artist: String,
    pub weather_temp: String,
    pub weather_desc: String,
}

pub type SharedLive = Arc<Mutex<Live>>;
pub type SharedStats = Arc<Mutex<EngineStats>>;

pub fn font_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Segoe UI", "segoeui.ttf"),
        ("Segoe UI Light", "segoeuil.ttf"),
        ("Segoe UI Semilight", "segoeuisl.ttf"),
        ("Segoe UI Semibold", "seguisb.ttf"),
        ("Segoe UI Bold", "segoeuib.ttf"),
        ("Segoe UI Black", "seguibl.ttf"),
        ("Bahnschrift", "bahnschrift.ttf"),
        ("Consolas", "consola.ttf"),
        ("Consolas Bold", "consolab.ttf"),
        ("Cascadia Code", "CascadiaCode.ttf"),
        ("Arial", "arial.ttf"),
        ("Arial Black", "ariblk.ttf"),
        ("Impact", "impact.ttf"),
        ("Georgia", "georgia.ttf"),
        ("Verdana", "verdana.ttf"),
        ("Trebuchet MS", "trebuc.ttf"),
        ("Tahoma", "tahoma.ttf"),
    ]
}

#[derive(Default)]
struct Fonts {
    loaded: HashMap<String, Option<Arc<Font>>>,
    fallback: Option<Arc<Font>>,
    /// Loaded only when a song actually needs them: each of these is large.
    script_fonts: HashMap<&'static str, Option<Arc<Font>>>,
    script_choice: HashMap<char, Option<Arc<Font>>>,
}

impl Fonts {
    fn load_file(path: &std::path::Path) -> Option<Arc<Font>> {
        let bytes = std::fs::read(path).ok()?;
        Font::from_bytes(bytes, FontSettings { scale: 64.0, ..Default::default() }).ok().map(Arc::new)
    }
    fn windows_font(file: &str) -> std::path::PathBuf {
        std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts").join(file)
    }
    /// Lyrics come in every language, so characters the chosen font lacks use a
    /// Windows font for that script. Scripts needing shaping may not join correctly.
    fn pick(&mut self, primary: &Arc<Font>, c: char) -> Arc<Font> {
        if c.is_whitespace() || primary.lookup_glyph_index(c) != 0 {
            return primary.clone();
        }
        if let Some(hit) = self.script_choice.get(&c) {
            return hit.clone().unwrap_or_else(|| primary.clone());
        }
        let files: &[&'static str] = match c as u32 {
            0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F => &["malgunbd.ttf"],
            0x3000..=0x30FF | 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF => &["YuGothB.ttc", "msyhbd.ttc"],
            0x0900..=0x0DFF => &["Nirmala.ttc"],
            0x0400..=0x052F | 0x0370..=0x03FF | 0x0100..=0x024F | 0x2000..=0x2BFF => &["segoeuib.ttf", "seguisym.ttf"],
            _ => &[],
        };
        let mut found = None;
        for file in files {
            let font = self.script_fonts.entry(file).or_insert_with(|| Self::load_file(&Self::windows_font(file))).clone();
            if let Some(font) = font.filter(|f| f.lookup_glyph_index(c) != 0) {
                found = Some(font);
                break;
            }
        }
        self.script_choice.insert(c, found.clone());
        found.unwrap_or_else(|| primary.clone())
    }

    fn get(&mut self, name: &str) -> Option<Arc<Font>> {
        if let Some(f) = self.loaded.get(name) {
            if f.is_some() {
                return f.clone();
            }
        } else {
            let path = font_catalog()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, f)| Self::windows_font(f))
                .unwrap_or_else(|| std::path::PathBuf::from(name));
            let font = Self::load_file(&path);
            self.loaded.insert(name.to_string(), font.clone());
            if font.is_some() {
                return font;
            }
        }
        if self.fallback.is_none() {
            self.fallback = Self::load_file(&Self::windows_font("segoeui.ttf"));
        }
        self.fallback.clone()
    }
}

/// Rasterised glyphs are cached: most frames repeat the same digits at the same size.
#[derive(Default)]
struct GlyphCache {
    map: HashMap<(usize, char, u32), Arc<(fontdue::Metrics, Vec<u8>)>>,
}

impl GlyphCache {
    fn get(&mut self, font: &Arc<Font>, c: char, px: f32) -> Arc<(fontdue::Metrics, Vec<u8>)> {
        if self.map.len() > 4096 {
            self.map.clear();
        }
        self.map
            .entry((Arc::as_ptr(font) as usize, c, (px * 4.0) as u32))
            .or_insert_with(|| Arc::new(font.rasterize(c, px)))
            .clone()
    }
}

fn skia_color(c: [u8; 4], opacity: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (c[3] as f32 * opacity.clamp(0.0, 1.0)) as u8)
}

fn paint_of(c: [u8; 4], opacity: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(skia_color(c, opacity));
    p.anti_alias = true;
    p
}

/// Circular arc as cubic segments. Angles are radians, clockwise from 12 o'clock.
fn arc_path(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let steps = ((sweep.abs() / (std::f32::consts::FRAC_PI_2)).ceil() as usize).max(1);
    let step = sweep / steps as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let at = |a: f32| (cx + r * a.sin(), cy - r * a.cos());
    let (sx, sy) = at(start);
    pb.move_to(sx, sy);
    for i in 0..steps {
        let a0 = start + step * i as f32;
        let a1 = a0 + step;
        let (x0, y0) = at(a0);
        let (x1, y1) = at(a1);
        pb.cubic_to(x0 + k * r * a0.cos(), y0 + k * r * a0.sin(), x1 - k * r * a1.cos(), y1 - k * r * a1.sin(), x1, y1);
    }
    pb.finish()
}

pub(crate) struct TextStyle<'a> {
    font: &'a str,
    px: f32,
    color: [u8; 4],
    opacity: f32,
    shadow: bool,
    align: &'a str,
}

pub(crate) struct Painter {
    fonts: Fonts,
    glyphs: GlyphCache,
    images: HashMap<String, Option<Pixmap>>,
    lyric_views: HashMap<String, LyricView>,
    art_views: HashMap<String, ArtView>,
}

impl Painter {
    pub(crate) fn font(&mut self, name: &str) -> Option<Arc<Font>> {
        self.fonts.get(name)
    }
    pub(crate) fn glyph(&mut self, primary: &Arc<Font>, c: char, px: f32) -> Arc<(fontdue::Metrics, Vec<u8>)> {
        let font = self.fonts.pick(primary, c);
        self.glyphs.get(&font, c, px)
    }
    pub(crate) fn release_script_fonts(&mut self) {
        if !self.fonts.script_fonts.is_empty() {
            self.fonts.script_fonts.clear();
            self.fonts.script_choice.clear();
            // Cached glyphs are keyed by font address, which may now be reused.
            self.glyphs.map.clear();
        }
    }
    pub(crate) fn advance(&mut self, primary: &Arc<Font>, c: char, px: f32) -> f32 {
        self.fonts.pick(primary, c).metrics(c, px).advance_width
    }

    /// Shortens text with an ellipsis so it fits, for track titles of any length.
    fn fit(&mut self, font: &Arc<Font>, text: &str, px: f32, max: f32) -> String {
        if self.measure(font, text, px) <= max {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut low, mut high) = (0, chars.len());
        while low < high {
            let mid = (low + high + 1) / 2;
            let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "\u{2026}";
            if self.measure(font, &candidate, px) <= max {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        chars[..low].iter().collect::<String>().trim_end().to_string() + "\u{2026}"
    }

    /// A status line in a lyrics widget, wrapped to its width and centred.
    pub(crate) fn message(&mut self, pm: &mut Pixmap, text: &str, w: &Widget, x: f32, y: f32, px: f32, max: f32) {
        let Some(font) = self.fonts.get(&w.font) else { return };
        let mut lines: Vec<String> = vec![String::new()];
        for word in text.split_whitespace() {
            let joined = if lines.last().unwrap().is_empty() { word.to_string() } else { format!("{} {word}", lines.last().unwrap()) };
            if self.measure(&font, &joined, px) <= max || lines.last().unwrap().is_empty() {
                *lines.last_mut().unwrap() = joined;
            } else {
                lines.push(word.to_string());
            }
        }
        let height = px * 1.3;
        let top = y - height * (lines.len() as f32 - 1.0) / 2.0;
        let st = TextStyle { font: &w.font, px, color: [255, 255, 255, 170], opacity: w.opacity, shadow: false, align: "center" };
        for (n, line) in lines.iter().enumerate() {
            self.text(pm, line, x, top + n as f32 * height, &st);
        }
    }

    fn measure(&mut self, font: &Arc<Font>, text: &str, px: f32) -> f32 {
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            if let Some(p) = prev {
                w += font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            w += self.fonts.pick(font, c).metrics(c, px).advance_width;
            prev = Some(c);
        }
        w
    }

    /// Draws a line of text centred vertically on `y`, anchored per `align` at `x`.
    fn text(&mut self, pm: &mut Pixmap, text: &str, x: f32, y: f32, st: &TextStyle) {
        let Some(font) = self.fonts.get(st.font) else { return };
        let width = self.measure(&font, text, st.px);
        let start = match st.align {
            "left" => x,
            "right" => x - width,
            _ => x - width / 2.0,
        };
        // Centre on the digit height rather than the line box, so numbers sit where they look centred.
        let cap = font.metrics('0', st.px).height as f32;
        let base = y + cap / 2.0;
        if st.shadow {
            // Several faint copies approximate a soft shadow. One solid copy leaves a hard
            // dark edge beside each glyph, which JPEG then rings around.
            let off = (st.px * 0.03).max(1.0);
            for (dx, dy) in [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 2.0), (2.0, 2.0)] {
                self.run(pm, &font, text, start + dx * off, base + dy * off, st.px, [0, 0, 0, 255], st.opacity * 0.14);
            }
        }
        self.run(pm, &font, text, start, base, st.px, st.color, st.opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&mut self, pm: &mut Pixmap, font: &Arc<Font>, text: &str, x: f32, base: f32, px: f32, color: [u8; 4], opacity: f32) {
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let data = pm.data_mut();
        let alpha = color[3] as u32 * (opacity.clamp(0.0, 1.0) * 255.0) as u32 / 255;
        let mut pen = x;
        let mut prev = None;
        for c in text.chars() {
            if let Some(p) = prev {
                pen += font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            prev = Some(c);
            let chosen = self.fonts.pick(font, c);
            let glyph = self.glyphs.get(&chosen, c, px);
            let (m, mask) = (&glyph.0, &glyph.1);
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (base - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height as i32 {
                let py = gy + row;
                if py < 0 || py >= h {
                    continue;
                }
                for col in 0..m.width as i32 {
                    let px_x = gx + col;
                    if px_x < 0 || px_x >= w {
                        continue;
                    }
                    let cov = mask[(row as usize) * m.width + col as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let a = cov * alpha / 255;
                    let i = ((py * w + px_x) * 4) as usize;
                    // Source-over on premultiplied pixels.
                    let inv = 255 - a;
                    for ch in 0..3 {
                        data[i + ch] = ((color[ch] as u32 * a + data[i + ch] as u32 * inv) / 255) as u8;
                    }
                    data[i + 3] = (a + data[i + 3] as u32 * inv / 255) as u8;
                }
            }
            pen += m.advance_width;
        }
    }

    fn image(&mut self, path: &str) -> Option<&Pixmap> {
        self.images
            .entry(path.to_string())
            .or_insert_with(|| {
                let img = image::open(path).ok()?.into_rgba8();
                let (w, h) = img.dimensions();
                let mut raw = img.into_raw();
                for px in raw.chunks_exact_mut(4) {
                    let a = px[3] as u32;
                    for c in 0..3 {
                        px[c] = (px[c] as u32 * a / 255) as u8;
                    }
                }
                Pixmap::from_vec(raw, tiny_skia::IntSize::from_wh(w, h)?)
            })
            .as_ref()
    }
}

fn fmt_value(v: f64, decimals: u32) -> String {
    format!("{:.*}", decimals as usize, v)
}

fn threshold_color(w: &Widget, v: f64) -> [u8; 4] {
    if w.crit > 0.0 && v >= w.crit as f64 {
        parse_color(&w.crit_color)
    } else if w.warn > 0.0 && v >= w.warn as f64 {
        parse_color(&w.warn_color)
    } else {
        parse_color(&w.color)
    }
}

struct Inputs<'a> {
    sensors: &'a crate::sensors::Snapshot,
    live: &'a Live,
    now: chrono::DateTime<Local>,
    profile_name: &'a str,
    history: &'a HashMap<String, VecDeque<f32>>,
    music: &'a Music,
}

fn expand_tokens(text: &str, i: &Inputs) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &rest[open + 1..open + close];
        match token {
            "time" => out.push_str(&i.now.format("%H:%M").to_string()),
            "date" => out.push_str(&i.now.format("%a %d %b").to_string()),
            "profile" => out.push_str(i.profile_name),
            "media.title" => out.push_str(&i.live.media_title),
            "media.artist" => out.push_str(&i.live.media_artist),
            "weather.temp" => out.push_str(&i.live.weather_temp),
            "weather.desc" => out.push_str(&i.live.weather_desc),
            t if t.starts_with("s:") => {
                let (id, dec) = match t[2..].rsplit_once('|') {
                    Some((id, d)) => (id, d.parse().unwrap_or(0)),
                    None => (&t[2..], 0),
                };
                match i.sensors.get(id) {
                    Some(r) => out.push_str(&format!("{}{}", fmt_value(r.value, dec), r.unit)),
                    None => out.push_str("--"),
                }
            }
            other => {
                out.push('{');
                out.push_str(other);
                out.push('}');
            }
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// Hash of everything the widget layer's pixels depend on. Equal hash, same pixels.
fn overlay_key(p: &Profile, cw: u32, ch: u32, i: &Inputs) -> u64 {
    let mut h = DefaultHasher::new();
    (cw, ch).hash(&mut h);
    format!("{:?}", p.widgets).hash(&mut h);
    i.live.alert.hash(&mut h);
    for w in p.widgets.iter().filter(|w| w.visible) {
        match w.kind.as_str() {
            "text" => expand_tokens(&w.text, i).hash(&mut h),
            "clock" | "date" | "analog" => {
                let fmt = i.now.format(if w.kind == "date" { "%F" } else { "%H:%M:%S" }).to_string();
                // A clock without seconds only needs redrawing once a minute.
                let needs_seconds = w.kind == "analog" || w.format.contains("%S") || w.format.contains("%T");
                let visible = if needs_seconds || w.kind == "date" { &fmt[..] } else { &fmt[..5] };
                visible.hash(&mut h);
            }
            "spark" => {
                if let Some(q) = i.history.get(&w.id) {
                    for v in q {
                        v.to_bits().hash(&mut h);
                    }
                }
            }
            "sensor" | "ring" | "bar" => {
                if let Some(r) = i.sensors.get(&w.sensor) {
                    fmt_value(r.value, w.decimals + 1).hash(&mut h);
                }
            }
            "lyrics" => {
                let m = i.music;
                (&m.track_id, m.state, m.present, m.lines.len(), m.plain.len()).hash(&mut h);
                active_index(&m.lines, m.time() + w.offset as f64).hash(&mut h);
            }
            "albumart" => {
                (i.music.art.as_ref().map(|a| a.seq), i.music.playing, i.music.present).hash(&mut h);
            }
            "track" => (&i.music.title, &i.music.artist, i.music.present).hash(&mut h),
            "progress" => {
                let (elapsed, remaining, fill) = progress_of(i.music);
                (elapsed, remaining, (fill * (w.w * cw.min(ch) as f32)) as u32).hash(&mut h);
            }
            _ => {}
        }
    }
    h.finish()
}

/// Elapsed seconds, remaining seconds and the filled fraction of the bar.
fn progress_of(m: &Music) -> (u32, u32, f32) {
    let time = m.time().max(0.0);
    match m.duration {
        Some(d) if d > 0.0 => (time as u32, (d - time).max(0.0) as u32, (time / d).min(1.0) as f32),
        _ => (time as u32, 0, 0.0),
    }
}

fn clock_text(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn draw_widget(pt: &mut Painter, pm: &mut Pixmap, w: &Widget, i: &Inputs) {
    let (cw, ch) = (pm.width() as f32, pm.height() as f32);
    let unit = cw.min(ch);
    let (x, y) = (w.x * cw, w.y * ch);
    let (bw, bh) = (w.w * unit, w.h * unit);
    let px = (w.size * unit).max(4.0);
    let col = parse_color(&w.color);
    let style = |px: f32, color: [u8; 4]| TextStyle {
        font: &w.font,
        px,
        color,
        opacity: w.opacity,
        shadow: w.shadow,
        align: &w.align,
    };
    let sensor = i.sensors.get(&w.sensor);
    let label = if !w.label.is_empty() { w.label.clone() } else { sensor.map(|r| r.name.clone()).unwrap_or_default() };

    match w.kind.as_str() {
        "text" => {
            let t = expand_tokens(&w.text, i);
            for (n, line) in t.lines().enumerate() {
                pt.text(pm, line, x, y + n as f32 * px * 1.25, &style(px, col));
            }
        }
        "clock" => {
            let fmt = if w.format.is_empty() { "%H:%M" } else { &w.format };
            let t = safe_time_format(&i.now, fmt);
            pt.text(pm, &t, x, y, &style(px, col));
        }
        "date" => {
            let fmt = if w.format.is_empty() { "%A, %d %B" } else { &w.format };
            let t = safe_time_format(&i.now, fmt);
            pt.text(pm, &t, x, y, &style(px, col));
        }
        "analog" => draw_analog(pm, w, x, y, bw.min(bh) / 2.0, i),
        "sensor" => {
            let (value, unit_s, c) = match sensor {
                Some(r) => (fmt_value(r.value, w.decimals), r.unit.as_str(), threshold_color(w, r.value)),
                None => ("--".to_string(), "", parse_color(&w.color2)),
            };
            let text = if w.show_unit { format!("{value}{unit_s}") } else { value };
            pt.text(pm, &text, x, y, &style(px, c));
            if !label.is_empty() {
                pt.text(pm, &label, x, y + px * 0.86, &style(px * 0.34, parse_color(&w.color2)));
            }
        }
        "ring" => {
            let r = bw.min(bh) / 2.0;
            let thick = (w.thickness * unit).max(1.0);
            let start = -135f32.to_radians();
            let sweep = 270f32.to_radians();
            let v = sensor.map(|r| r.value).unwrap_or(w.min as f64);
            let frac = ((v as f32 - w.min) / (w.max - w.min).max(0.001)).clamp(0.0, 1.0);
            let stroke = Stroke { width: thick, line_cap: LineCap::Round, ..Default::default() };
            if let Some(path) = arc_path(x, y, r - thick / 2.0, start, sweep) {
                pm.stroke_path(&path, &paint_of(parse_color(&w.track_color), w.opacity), &stroke, Transform::identity(), None);
            }
            let c = sensor.map(|r| threshold_color(w, r.value)).unwrap_or(col);
            if frac > 0.002 {
                if let Some(path) = arc_path(x, y, r - thick / 2.0, start, sweep * frac) {
                    let mut paint = paint_of(c, w.opacity);
                    // The gradient only applies in the normal range: warnings read as one flat colour.
                    if c == col {
                        if let Some(shader) = LinearGradient::new(
                            Point::from_xy(x - r, y + r),
                            Point::from_xy(x + r, y - r),
                            vec![
                                GradientStop::new(0.0, skia_color(col, w.opacity)),
                                GradientStop::new(1.0, skia_color(parse_color(&w.color2), w.opacity)),
                            ],
                            SpreadMode::Pad,
                            Transform::identity(),
                        ) {
                            paint.shader = shader;
                        }
                    }
                    pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
            let (value, unit_s) = match sensor {
                Some(r) => (fmt_value(r.value, w.decimals), r.unit.as_str()),
                None => ("--".to_string(), ""),
            };
            let text = if w.show_unit { format!("{value}{unit_s}") } else { value };
            let centered = |px: f32, color: [u8; 4]| TextStyle { align: "center", ..style(px, color) };
            pt.text(pm, &text, x, y - px * 0.1, &centered(px, parse_color("#ffffff")));
            if !label.is_empty() {
                pt.text(pm, &label, x, y + px * 0.72, &centered(px * 0.42, parse_color(&w.color2)));
            }
        }
        "bar" => {
            let (left, top) = (x - bw / 2.0, y - bh / 2.0);
            let radius = bh / 2.0;
            let v = sensor.map(|r| r.value).unwrap_or(w.min as f64);
            let frac = ((v as f32 - w.min) / (w.max - w.min).max(0.001)).clamp(0.0, 1.0);
            if let Some(path) = rounded_rect(left, top, bw, bh, radius) {
                pm.fill_path(&path, &paint_of(parse_color(&w.track_color), w.opacity), FillRule::Winding, Transform::identity(), None);
            }
            if let Some(path) = rounded_rect(left, top, (bw * frac).max(bh), bh, radius) {
                if frac > 0.002 {
                    let c = sensor.map(|r| threshold_color(w, r.value)).unwrap_or(col);
                    pm.fill_path(&path, &paint_of(c, w.opacity), FillRule::Winding, Transform::identity(), None);
                }
            }
            let caption = match sensor {
                Some(r) if w.show_unit => format!("{label}  {}{}", fmt_value(r.value, w.decimals), r.unit),
                Some(r) => format!("{label}  {}", fmt_value(r.value, w.decimals)),
                None => label,
            };
            if !caption.trim().is_empty() {
                let st = TextStyle { align: "left", ..style(px, parse_color(&w.color2)) };
                pt.text(pm, &caption, left, top - px * 0.7, &st);
            }
        }
        "spark" => {
            let (left, top) = (x - bw / 2.0, y - bh / 2.0);
            if let Some(q) = i.history.get(&w.id).filter(|q| q.len() >= 2) {
                let cap = w.history.max(2) as f32;
                let (lo, hi) = (w.min, w.max.max(w.min + 0.001));
                let point = |n: usize, v: f32| {
                    let px_x = left + bw * (cap - q.len() as f32 + n as f32) / (cap - 1.0);
                    let py = top + bh * (1.0 - ((v - lo) / (hi - lo)).clamp(0.0, 1.0));
                    (px_x, py)
                };
                let mut line = PathBuilder::new();
                let mut area = PathBuilder::new();
                let (x0, _) = point(0, 0.0);
                area.move_to(x0, top + bh);
                for (n, v) in q.iter().enumerate() {
                    let (px_x, py) = point(n, *v);
                    if n == 0 {
                        line.move_to(px_x, py);
                    } else {
                        line.line_to(px_x, py);
                    }
                    area.line_to(px_x, py);
                }
                let (x1, _) = point(q.len() - 1, 0.0);
                area.line_to(x1, top + bh);
                area.close();
                if let Some(path) = area.finish() {
                    let mut paint = paint_of(col, w.opacity);
                    if let Some(shader) = LinearGradient::new(
                        Point::from_xy(0.0, top),
                        Point::from_xy(0.0, top + bh),
                        vec![
                            GradientStop::new(0.0, skia_color([col[0], col[1], col[2], 110], w.opacity)),
                            GradientStop::new(1.0, skia_color([col[0], col[1], col[2], 0], w.opacity)),
                        ],
                        SpreadMode::Pad,
                        Transform::identity(),
                    ) {
                        paint.shader = shader;
                    }
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                }
                if let Some(path) = line.finish() {
                    let stroke = Stroke { width: (w.thickness * unit).max(1.0), line_cap: LineCap::Round, ..Default::default() };
                    pm.stroke_path(&path, &paint_of(col, w.opacity), &stroke, Transform::identity(), None);
                }
            }
            if !label.is_empty() {
                let text = match sensor {
                    Some(r) => format!("{label}  {}{}", fmt_value(r.value, w.decimals), if w.show_unit { r.unit.as_str() } else { "" }),
                    None => label,
                };
                let st = TextStyle { align: "left", ..style(px, parse_color(&w.color2)) };
                pt.text(pm, &text, left, top - px * 0.7, &st);
            }
        }
        "lyrics" => {
            let mut view = pt.lyric_views.remove(&w.id).unwrap_or_default();
            view.draw(pt, pm, w, i.music);
            pt.lyric_views.insert(w.id.clone(), view);
        }
        "albumart" => {
            pt.art_views.entry(w.id.clone()).or_default().draw(pm, w, i.music);
        }
        "track" => {
            let Some(font) = pt.font(&w.font) else { return };
            let title = if i.music.present { i.music.title.as_str() } else { "Not Playing" };
            let title = pt.fit(&font, title, px, bw);
            let artist = pt.fit(&font, &i.music.artist, px * 0.86, bw);
            let anchor = match w.align.as_str() {
                "left" => x - bw / 2.0,
                "right" => x + bw / 2.0,
                _ => x,
            };
            pt.text(pm, &title, anchor, y - px * 0.62, &style(px, col));
            pt.text(pm, &artist, anchor, y + px * 0.68, &style(px * 0.86, parse_color(&w.color2)));
        }
        "progress" => {
            let (elapsed, remaining, fill) = progress_of(i.music);
            let (left, top) = (x - bw / 2.0, y - bh / 2.0);
            if let Some(path) = rounded_rect(left, top, bw, bh, bh / 2.0) {
                pm.fill_path(&path, &paint_of(parse_color(&w.track_color), w.opacity), FillRule::Winding, Transform::identity(), None);
            }
            if fill > 0.001 {
                if let Some(path) = rounded_rect(left, top, (bw * fill).max(bh), bh, bh / 2.0) {
                    pm.fill_path(&path, &paint_of(col, w.opacity), FillRule::Winding, Transform::identity(), None);
                }
            }
            let below = y + bh / 2.0 + px * 0.95;
            let muted = parse_color(&w.color2);
            let start = TextStyle { align: "left", ..style(px, muted) };
            pt.text(pm, &clock_text(elapsed), left, below, &start);
            if i.music.duration.is_some() {
                let end = TextStyle { align: "right", ..style(px, muted) };
                pt.text(pm, &format!("-{}", clock_text(remaining)), left + bw, below, &end);
            }
        }
        "image" => {
            if let Some(img) = pt.image(&w.path) {
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                let s = (bw / iw).min(bh / ih);
                let transform = Transform::from_row(s, 0.0, 0.0, s, x - iw * s / 2.0, y - ih * s / 2.0);
                let paint = PixmapPaint { opacity: w.opacity, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
                pm.draw_pixmap(0, 0, img.as_ref(), &paint, transform, None);
            }
        }
        _ => {}
    }
}

/// chrono panics while formatting an invalid specifier, and these strings are
/// user-typed, so they are validated first.
fn safe_time_format(now: &chrono::DateTime<Local>, fmt: &str) -> String {
    use chrono::format::{Item, StrftimeItems};
    if StrftimeItems::new(fmt).any(|i| matches!(i, Item::Error)) {
        return fmt.to_string();
    }
    now.format(fmt).to_string()
}

pub(crate) fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * 0.5523;
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    p.line_to(x, y + r);
    p.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    p.close();
    p.finish()
}

fn draw_analog(pm: &mut Pixmap, w: &Widget, x: f32, y: f32, r: f32, i: &Inputs) {
    let col = parse_color(&w.color);
    let accent = parse_color(&w.color2);
    let tick = |pm: &mut Pixmap, angle: f32, r0: f32, r1: f32, width: f32, c: [u8; 4]| {
        let (s, co) = angle.sin_cos();
        let mut p = PathBuilder::new();
        p.move_to(x + s * r0, y - co * r0);
        p.line_to(x + s * r1, y - co * r1);
        if let Some(path) = p.finish() {
            let stroke = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
            pm.stroke_path(&path, &paint_of(c, w.opacity), &stroke, Transform::identity(), None);
        }
    };
    let tau = std::f32::consts::TAU;
    for n in 0..60 {
        let major = n % 5 == 0;
        tick(pm, tau * n as f32 / 60.0, r * if major { 0.86 } else { 0.92 }, r * 0.98, if major { r * 0.022 } else { r * 0.008 }, col);
    }
    let (h, m, s) = (i.now.hour12().1 as f32, i.now.minute() as f32, i.now.second() as f32);
    tick(pm, tau * (h + m / 60.0) / 12.0, 0.0, r * 0.52, r * 0.045, col);
    tick(pm, tau * (m + s / 60.0) / 60.0, 0.0, r * 0.76, r * 0.03, col);
    tick(pm, tau * s / 60.0, -r * 0.12, r * 0.86, r * 0.012, accent);
    if let Some(path) = PathBuilder::from_circle(x, y, r * 0.035) {
        pm.fill_path(&path, &paint_of(accent, w.opacity), FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_alert(pt: &mut Painter, pm: &mut Pixmap, text: &str, phase: bool) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let band = h * 0.26;
    if let Some(rect) = Rect::from_xywh(0.0, (h - band) / 2.0, w, band) {
        let c = if phase { [214, 40, 40, 235] } else { [140, 16, 16, 235] };
        pm.fill_rect(rect, &paint_of(c, 1.0), Transform::identity(), None);
    }
    let st = TextStyle { font: "Segoe UI Bold", px: band * 0.42, color: [255, 255, 255, 255], opacity: 1.0, shadow: false, align: "center" };
    pt.text(pm, text, w / 2.0, h / 2.0, &st);
}

/// Final per-pixel pass: premultiplied RGBA to the panel's rotated RGB buffer.
fn post(src: &Pixmap, rotation: u32, flip: bool, brightness: u32, pw: u32, ph: u32, out: &mut Vec<u8>) {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (pw, ph) = (pw as usize, ph as usize);
    out.resize(pw * ph * 3, 0);
    let data = src.data();
    let b = brightness.min(100) * 256 / 100;
    for py in 0..ph {
        for px in 0..pw {
            let (x, y) = match rotation {
                90 => (py, pw - 1 - px),
                180 => (pw - 1 - px, ph - 1 - py),
                270 => (ph - 1 - py, px),
                _ => (px, py),
            };
            let x = if flip { sw - 1 - x.min(sw - 1) } else { x.min(sw - 1) };
            let i = (y.min(sh - 1) * sw + x) * 4;
            let o = (py * pw + px) * 3;
            out[o] = ((data[i] as u32 * b) >> 8) as u8;
            out[o + 1] = ((data[i + 1] as u32 * b) >> 8) as u8;
            out[o + 2] = ((data[i + 2] as u32 * b) >> 8) as u8;
        }
    }
}

fn encode_panel(rgb: &[u8], w: u32, h: u32, panel: Panel, jpeg: bool, quality: u8, sharp: bool, out: &mut Vec<u8>) -> Result<(), String> {
    out.clear();
    if jpeg {
        let mut enc = jpeg_encoder::Encoder::new(&mut *out, quality.clamp(40, 100));
        enc.set_sampling_factor(if sharp { jpeg_encoder::SamplingFactor::F_1_1 } else { jpeg_encoder::SamplingFactor::F_2_2 });
        enc.encode(rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).map_err(|e| e.to_string())
    } else {
        out.reserve(rgb.len() / 3 * 2);
        for p in rgb.chunks_exact(3) {
            let v = ((p[0] as u16 & 0xF8) << 8) | ((p[1] as u16 & 0xFC) << 3) | (p[2] as u16 >> 3);
            let bytes = if panel.big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
            out.extend_from_slice(&bytes);
        }
        Ok(())
    }
}

pub struct EngineHandles {
    pub slot: Arc<FrameSlot>,
    pub panel: Arc<Mutex<Panel>>,
    pub sensors: SharedSensors,
    pub live: SharedLive,
    pub stats: SharedStats,
    pub music: SharedMusic,
}

pub fn spawn(h: EngineHandles) {
    std::thread::Builder::new().name("render".into()).spawn(move || run(h)).expect("spawn render thread");
}

fn run(h: EngineHandles) {
    let mut painter = Painter {
        fonts: Fonts::default(),
        glyphs: GlyphCache::default(),
        images: HashMap::new(),
        lyric_views: HashMap::new(),
        art_views: HashMap::new(),
    };
    let mut anim_tick = 0u64;
    let mut bg_cfg: Option<(Background, u32, u32, u32)> = None;
    let mut bg: Option<media::Handle> = None;
    let mut bg_seq = u64::MAX;
    let mut base: Option<Pixmap> = None;
    let mut overlay: Option<Pixmap> = None;
    let mut composed: Option<Pixmap> = None;
    let mut overlay_hash = 0u64;
    let mut history: HashMap<String, VecDeque<f32>> = HashMap::new();
    let mut hist_seq = 0u64;
    let mut rgb = Vec::new();
    let mut jpeg = Vec::new();
    let mut frame_seq = 0u64;
    let mut last_output_key = 0u64;
    let mut encodes = 0u32;
    let mut stat_at = Instant::now();
    let (mut render_ms, mut encode_ms) = (0f32, 0f32);

    loop {
        let tick = Instant::now();
        let live = h.live.lock().unwrap().clone();
        let panel = *h.panel.lock().unwrap();
        let Some(profile) = live.profile.clone() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let portrait = profile.rotation == 90 || profile.rotation == 270;
        let (cw, ch) = if portrait { (panel.height, panel.width) } else { (panel.width, panel.height) };

        // Restart the background only if something it depends on changed.
        let wanted = (profile.bg.clone(), cw, ch, profile.fps);
        if bg_cfg.as_ref() != Some(&wanted) {
            bg = Some(media::start(&profile.bg, cw, ch, profile.fps, h.music.clone()));
            bg_cfg = Some(wanted);
            bg_seq = u64::MAX;
            base = Pixmap::new(cw, ch);
            composed = Pixmap::new(cw, ch);
            overlay = Pixmap::new(cw, ch);
            overlay_hash = 0;
        }
        let (Some(b), Some(ov), Some(comp)) = (base.as_mut(), overlay.as_mut(), composed.as_mut()) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };

        let render_start = Instant::now();
        let mut dirty = false;
        if let Some(handle) = &bg {
            let seq = handle.0.seq.load(Ordering::Acquire);
            if seq != bg_seq && seq != 0 {
                bg_seq = seq;
                let src = handle.0.rgba.lock().unwrap();
                if src.len() == b.data().len() {
                    let dim = ((1.0 - profile.bg.dim.clamp(0.0, 1.0)) * 256.0) as u32;
                    let dst = b.data_mut();
                    if dim >= 256 {
                        dst.copy_from_slice(&src);
                    } else {
                        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                            d[0] = ((s[0] as u32 * dim) >> 8) as u8;
                            d[1] = ((s[1] as u32 * dim) >> 8) as u8;
                            d[2] = ((s[2] as u32 * dim) >> 8) as u8;
                            d[3] = 255;
                        }
                    }
                    dirty = true;
                }
            }
        }

        let sensors = h.sensors.read().unwrap();
        if sensors.seq != hist_seq {
            hist_seq = sensors.seq;
            for w in profile.widgets.iter().filter(|w| w.kind == "spark") {
                if let Some(r) = sensors.get(&w.sensor) {
                    let q = history.entry(w.id.clone()).or_default();
                    q.push_back(r.value as f32);
                    while q.len() > w.history.max(2) as usize {
                        q.pop_front();
                    }
                }
            }
        }
        let now = Local::now();
        let music_needs = crate::music::needs(&profile);
        let music = if music_needs.media { h.music.read().unwrap().clone() } else { Music::default() };
        let inputs = Inputs { sensors: &sensors, live: &live, now, profile_name: &profile.name, history: &history, music: &music };
        // Text and art animate between data changes, so those frames must redraw.
        let music_animating = profile.widgets.iter().any(|w| match w.kind.as_str() {
            "lyrics" => painter.lyric_views.get(&w.id).map_or(true, |v| v.animating(&music, w.offset)),
            "albumart" => painter.art_views.get(&w.id).is_some_and(|v| v.animating()),
            _ => false,
        });
        let alert_phase = live.alert.is_some() && now.timestamp_subsec_millis() < 500;
        let mut key = overlay_key(&profile, cw, ch, &inputs);
        key ^= (alert_phase as u64) << 63;
        if music_animating {
            anim_tick += 1;
            key ^= anim_tick.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        if key != overlay_hash {
            overlay_hash = key;
            ov.fill(Color::TRANSPARENT);
            for w in profile.widgets.iter().filter(|w| w.visible) {
                draw_widget(&mut painter, ov, w, &inputs);
            }
            if let Some(text) = &live.alert {
                draw_alert(&mut painter, ov, text, alert_phase);
            }
            dirty = true;
        }
        drop(sensors);

        // Brightness, orientation and quality change the output without changing either layer.
        let mut hasher = DefaultHasher::new();
        (profile.rotation, profile.flip, profile.quality, profile.sharp_text, profile.brightness.min(live.brightness_cap)).hash(&mut hasher);
        (panel.width, panel.height, panel.jpeg).hash(&mut hasher);
        let output_key = hasher.finish();
        if output_key != last_output_key {
            last_output_key = output_key;
            dirty = true;
        }

        if dirty {
            comp.data_mut().copy_from_slice(b.data());
            comp.draw_pixmap(0, 0, ov.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            let brightness = profile.brightness.min(live.brightness_cap);
            post(comp, profile.rotation, profile.flip, brightness, panel.width, panel.height, &mut rgb);
            render_ms = render_start.elapsed().as_secs_f32() * 1000.0;
            let enc_start = Instant::now();
            // Each panel takes one format. The JPEG panels accept raw frames but only show their logo.
            let use_jpeg = panel.jpeg;
            match encode_panel(&rgb, panel.width, panel.height, panel, use_jpeg, profile.quality, profile.sharp_text, &mut jpeg) {
                Ok(()) => {
                    encode_ms = enc_start.elapsed().as_secs_f32() * 1000.0;
                    frame_seq += 1;
                    encodes += 1;
                    h.slot.publish(Frame {
                        seq: frame_seq,
                        payload: jpeg.clone(),
                        jpeg: use_jpeg,
                        big_endian: panel.big_endian,
                        width: panel.width,
                        height: panel.height,
                    });
                }
                Err(e) => crate::store::log(&format!("encode: {e}")),
            }
        }

        if stat_at.elapsed() >= Duration::from_secs(1) {
            let mut s = h.stats.lock().unwrap();
            s.encodes_per_sec = encodes as f32 / stat_at.elapsed().as_secs_f32();
            s.render_ms = render_ms;
            s.encode_ms = encode_ms;
            s.frame_kb = jpeg.len() as f32 / 1024.0;
            s.canvas_w = cw;
            s.canvas_h = ch;
            s.seq = frame_seq;
            s.profile = profile.id.clone();
            s.reason = live.reason.clone();
            encodes = 0;
            stat_at = Instant::now();
        }

        // Animated backgrounds poll at the target rate; static ones only need to
        // catch the next sensor sample or clock tick.
        let animated = music_animating
            || (music_needs.media && music.playing)
            || matches!(profile.bg.kind.as_str(), "gif" | "video" | "liquid" | "screen" | "visualizer" | "gradient" | "slideshow" | "albumart");
        let period = if animated { Duration::from_secs_f32(1.0 / profile.fps.clamp(1, 60) as f32) } else { Duration::from_millis(200) };
        std::thread::sleep(period.saturating_sub(tick.elapsed()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canvas with one marked pixel, returning where that pixel lands on the panel.
    fn landing(rotation: u32, flip: bool, cw: u32, ch: u32, pw: u32, ph: u32, mark: (u32, u32)) -> (usize, usize) {
        let mut canvas = Pixmap::new(cw, ch).unwrap();
        let i = ((mark.1 * cw + mark.0) * 4) as usize;
        canvas.data_mut()[i] = 255;
        let mut out = Vec::new();
        post(&canvas, rotation, flip, 100, pw, ph, &mut out);
        let at = out.chunks_exact(3).position(|p| p[0] > 0).expect("pixel survived");
        (at % pw as usize, at / pw as usize)
    }

    #[test]
    fn rotation_moves_top_left_to_the_expected_corner() {
        assert_eq!(landing(0, false, 4, 4, 4, 4, (0, 0)), (0, 0));
        assert_eq!(landing(90, false, 4, 4, 4, 4, (0, 0)), (3, 0));
        assert_eq!(landing(180, false, 4, 4, 4, 4, (0, 0)), (3, 3));
        assert_eq!(landing(270, false, 4, 4, 4, 4, (0, 0)), (0, 3));
    }

    #[test]
    fn rotation_handles_non_square_panels() {
        // A 2x4 portrait canvas shown on a 4x2 landscape panel.
        assert_eq!(landing(90, false, 2, 4, 4, 2, (0, 0)), (3, 0));
        assert_eq!(landing(270, false, 2, 4, 4, 2, (0, 0)), (0, 1));
    }

    #[test]
    fn brightness_scales_output() {
        let mut canvas = Pixmap::new(1, 1).unwrap();
        canvas.data_mut().copy_from_slice(&[200, 200, 200, 255]);
        let mut out = Vec::new();
        post(&canvas, 0, false, 50, 1, 1, &mut out);
        assert_eq!(out[0], 100);
        post(&canvas, 0, false, 0, 1, 1, &mut out);
        assert_eq!(out[0], 0);
    }

    #[test]
    fn invalid_time_format_does_not_panic() {
        assert_eq!(safe_time_format(&Local::now(), "%Q bad"), "%Q bad");
    }
}
