//! The Lyrical look, drawn natively for the panel: rows roll so the current line sits
//! 30% of the way down, later rows start slightly after earlier ones, the sung line
//! fills from dim to white along a soft edge, and distance from it adds blur.
//!
//! Each row is rasterised once into an alpha mask, and blurred copies are made only
//! when first needed. A frame only samples those masks, so animating costs little.

use crate::media::parse_color;
use crate::model::Widget;
use crate::music::{Line, LyricState, Music};
use crate::render::Painter;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;
use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, Pixmap, PixmapPaint, Point, SpreadMode, Transform};

const ROLL_SECONDS: f32 = 0.75;
const STYLE_SECONDS: f32 = 0.5;
const ROW_STAGGER: f32 = 0.04;
const INACTIVE_SCALE: f32 = 0.95;
const ANCHOR: f32 = 0.3;
const BLUR_LEVELS: usize = 4;
const GLOW_SIGMA: f32 = 0.2;

/// Margin around each row's mask. Wide enough that the glow reaches zero before the
/// mask ends, or it would show as a faint rectangle.
fn row_pad(px: f32) -> usize {
    (px * GLOW_SIGMA * 4.0).ceil() as usize
}

/// Index of the last line at or before `time`, or -1 before the first line.
pub fn active_index(lines: &[Line], time: f64) -> isize {
    if lines.is_empty() || time < lines[0].time {
        return -1;
    }
    lines.partition_point(|l| l.time <= time) as isize - 1
}

/// How far through a line we are. Lyrics are timed per line, so the sweep is paced by
/// text length and capped by when the next line starts.
fn sweep_span(row: &Row) -> f64 {
    let gap = row.next - row.time;
    if row.dots {
        gap
    } else {
        gap.min(row.chars as f64 * 0.085 + 0.9)
    }
}

/// A slight overshoot, like the springy roll in Lyrical.
fn spring(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    const C: f32 = 0.9;
    1.0 + (C + 1.0) * (t - 1.0).powi(3) + C * (t - 1.0).powi(2)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// One visual line of a row. `path` is where it starts along the sweep's reading path.
struct Span {
    path: f32,
}

struct Row {
    time: f64,
    next: f64,
    chars: usize,
    dots: bool,
    w: usize,
    h: usize,
    masks: [Option<Vec<u8>>; BLUR_LEVELS],
    glow: Option<Vec<u8>>,
    spans: Vec<Span>,
    length: f32,
    layout_y: f32,
    y_from: f32,
    y_to: f32,
    blur_from: f32,
    blur_to: f32,
    lit_from: f32,
    lit_to: f32,
    scale_from: f32,
    scale_to: f32,
    delay: f32,
}

impl Row {
    fn y(&self, age: f32) -> f32 {
        lerp(self.y_from, self.y_to, spring((age - self.delay) / ROLL_SECONDS))
    }
    fn scale(&self, age: f32) -> f32 {
        lerp(self.scale_from, self.scale_to, spring((age - self.delay) / ROLL_SECONDS))
    }
    fn blur(&self, age: f32) -> f32 {
        lerp(self.blur_from, self.blur_to, (age / STYLE_SECONDS).clamp(0.0, 1.0))
    }
    fn lit(&self, age: f32) -> f32 {
        lerp(self.lit_from, self.lit_to, (age / STYLE_SECONDS).clamp(0.0, 1.0))
    }
}

fn gaussian(src: &[u8], w: usize, h: usize, sigma: f32) -> Vec<u8> {
    let radius = (sigma * 2.6).ceil().max(1.0) as isize;
    let kernel: Vec<f32> = {
        let raw: Vec<f32> = (-radius..=radius).map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = raw.iter().sum();
        raw.into_iter().map(|v| v / sum).collect()
    };
    let mut temp = vec![0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut acc = 0.0;
            for (k, weight) in kernel.iter().enumerate() {
                let sx = (x as isize + k as isize - radius).clamp(0, w as isize - 1) as usize;
                acc += row[sx] as f32 * weight;
            }
            temp[y * w + x] = acc;
        }
    }
    let mut out = vec![0u8; w * h];
    for x in 0..w {
        for y in 0..h {
            let mut acc = 0.0;
            for (k, weight) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - radius).clamp(0, h as isize - 1) as usize;
                acc += temp[sy * w + x] * weight;
            }
            out[y * w + x] = acc.round().min(255.0) as u8;
        }
    }
    out
}

#[derive(Default)]
pub struct LyricView {
    key: u64,
    rows: Vec<Row>,
    focus: isize,
    /// Row index of the line in `lines`, offset by the intro dots if there are any.
    intro: bool,
    plain: bool,
    total_height: f32,
    px: f32,
    pad: usize,
    changed_at: Option<Instant>,
}

impl LyricView {
    fn build(&mut self, pt: &mut Painter, w: &Widget, m: &Music, box_w: f32, px: f32) {
        let started = Instant::now();
        self.rows.clear();
        self.focus = -2;
        self.changed_at = None;
        self.px = px;
        self.pad = row_pad(px);
        self.plain = m.state == LyricState::Plain;
        self.intro = false;

        let line_h = px * 1.22;
        let gap = px * 0.42;
        let mut y = 0.0f32;
        let mut add = |row: Option<Row>| {
            if let Some(mut row) = row {
                row.layout_y = y;
                y += row.h as f32 - 2.0 * row_pad(px) as f32 + gap;
                self.rows.push(row);
            }
        };
        match m.state {
            LyricState::Synced => {
                let lines: &[Line] = &m.lines;
                self.intro = lines.first().is_some_and(|l| l.time > 3.0);
                if self.intro {
                    add(Some(dots_row(px, 0.0, lines[0].time)));
                }
                for (i, line) in lines.iter().enumerate() {
                    let next = lines.get(i + 1).map_or(line.time + 5.0, |n| n.time);
                    if line.text.is_empty() {
                        add(Some(dots_row(px, line.time, next)));
                    } else {
                        add(text_row(pt, &w.font, &line.text, px, line_h, box_w, line.time, next));
                    }
                }
            }
            LyricState::Plain => {
                for line in m.plain.iter() {
                    let text = if line.trim().is_empty() { " " } else { line.as_str() };
                    add(text_row(pt, &w.font, text, px, line_h, box_w, 0.0, 0.0));
                }
            }
            _ => {}
        }
        self.total_height = y;
        // The rows are rasterised now, so any large script fonts can go straight away.
        pt.release_script_fonts();
        if !self.rows.is_empty() {
            crate::store::log(&format!("lyrics: laid out {} rows in {} ms", self.rows.len(), started.elapsed().as_millis()));
        }
    }

    fn set_focus(&mut self, position: isize, anchor_y: f32, active: bool, now: Instant) {
        let age = self.changed_at.map_or(1e6, |t| now.duration_since(t).as_secs_f32());
        let first = self.focus == -2;
        let anchor_layout = self.rows.get(position.max(0) as usize).map_or(0.0, |r| r.layout_y);
        for (i, row) in self.rows.iter_mut().enumerate() {
            let distance = i as isize - position;
            let is_active = distance == 0 && active;
            let (y, blur, lit, scale) = (row.y(age), row.blur(age), row.lit(age), row.scale(age));
            row.y_to = anchor_y + row.layout_y - anchor_layout;
            row.y_from = if first { row.y_to } else { y };
            row.blur_to = if is_active { 0.0 } else { distance.unsigned_abs().min(BLUR_LEVELS - 1) as f32 };
            row.blur_from = if first { row.blur_to } else { blur };
            row.lit_to = if is_active { 1.0 } else { 0.0 };
            row.lit_from = if first { row.lit_to } else { lit };
            row.scale_to = if is_active { 1.0 } else { INACTIVE_SCALE };
            row.scale_from = if first { row.scale_to } else { scale };
            // Lines from just above the focus downward start progressively later.
            row.delay = (distance + 1).clamp(0, 8) as f32 * ROW_STAGGER;
        }
        self.focus = position;
        self.changed_at = Some(now);
    }

    /// True while pixels keep changing with no change in the widget's data.
    pub fn animating(&self, m: &Music, offset: f32) -> bool {
        if self.changed_at.is_some_and(|t| t.elapsed().as_secs_f32() < ROLL_SECONDS + 0.4) {
            return true;
        }
        if !m.playing || self.rows.is_empty() {
            return false;
        }
        if self.plain {
            return true;
        }
        let time = m.time() + offset as f64;
        let index = active_index(&m.lines, time);
        let position = position_of(index, self.intro);
        self.rows.get(position.max(0) as usize).is_some_and(|row| (time - row.time) < sweep_span(row) + 0.1)
    }

    pub fn draw(&mut self, pt: &mut Painter, pm: &mut Pixmap, w: &Widget, m: &Music) {
        let (cw, ch) = (pm.width() as f32, pm.height() as f32);
        let unit = cw.min(ch);
        let (bw, bh) = (w.w * unit, w.h * unit);
        let (left, top) = (w.x * cw - bw / 2.0, w.y * ch - bh / 2.0);
        let px = (w.size * unit).max(6.0);

        let mut hash = DefaultHasher::new();
        (&m.track_id, m.state, Arc::as_ptr(&m.lines) as usize, Arc::as_ptr(&m.plain) as usize).hash(&mut hash);
        (bw as u32, px as u32, &w.font).hash(&mut hash);
        let key = hash.finish();
        if key != self.key {
            self.key = key;
            self.build(pt, w, m, bw, px);
        }

        let message = if !m.present {
            Some("Play a song in any app to see lyrics")
        } else {
            match m.state {
                LyricState::Idle | LyricState::Loading => Some("Finding lyrics\u{2026}"),
                LyricState::Unavailable => Some("Lyrics aren't available for this song"),
                LyricState::Instrumental => Some("Instrumental"),
                _ => None,
            }
        };
        if let Some(text) = message {
            pt.message(pm, text, w, left + bw / 2.0, top + bh / 2.0, px * 0.5, bw);
            return;
        }

        let now = Instant::now();
        let time = m.time() + w.offset as f64;
        let lit_color = parse_color(&w.color);
        let dim_color = parse_color(&w.color2);
        let fade = fade_table(bh as usize);
        let clip = Clip { left: left.floor() as i32, top: top.floor() as i32, right: (left + bw).ceil() as i32, bottom: (top + bh).ceil() as i32 };

        if self.plain {
            let frac = m.duration.filter(|d| *d > 0.0).map_or(0.0, |d| (time / d).clamp(0.0, 1.0)) as f32;
            let scroll = frac * (self.total_height - bh * 0.5).max(0.0);
            let pad = self.pad as f32;
            for row in &mut self.rows {
                let y = top + bh * 0.08 + row.layout_y - scroll;
                if y + row.h as f32 > top && y - pad < top + bh {
                    let (rw, rh) = (row.w, row.h);
                    let mask = row_mask(row, 0, px);
                    blit(pm, mask, None, 0.0, rw, rh, left - pad, y - pad, 1.0, &clip, &fade, |_, _| (lit_color, w.opacity * 0.8));
                }
            }
            return;
        }

        let index = active_index(&m.lines, time);
        let position = position_of(index, self.intro);
        let active = index >= 0 || self.intro;
        if position != self.focus {
            self.set_focus(position, top + bh * ANCHOR, active, now);
        }
        let age = self.changed_at.map_or(1e6, |t| now.duration_since(t).as_secs_f32());

        let pad = self.pad as f32;
        let opacity = w.opacity;
        let feather = px * 0.45;
        let focus = self.focus;
        for (i, row) in self.rows.iter_mut().enumerate() {
            let y = row.y(age);
            let scale = row.scale(age);
            let row_h = row.h as f32 * scale;
            if y + row_h < top - pad || y - pad > top + bh + pad {
                continue;
            }
            let ox = left - pad * scale;
            let oy = y - pad * scale;
            let lit = row.lit(age);

            if lit > 0.01 && !row.dots {
                let (rw, rh) = (row.w, row.h);
                let glow = row.glow.get_or_insert_with(|| gaussian(row.masks[0].as_ref().unwrap(), rw, rh, px * GLOW_SIGMA));
                let a = lit * 0.3 * opacity;
                blit(pm, glow, None, 0.0, rw, rh, ox, oy, scale, &clip, &fade, |_, _| (lit_color, a));
            }

            let blur = row.blur(age);
            let low = blur.floor() as usize;
            let high = (low + 1).min(BLUR_LEVELS - 1);
            let k = blur - low as f32;
            row_mask(row, low, px);
            if k > 0.01 {
                row_mask(row, high, px);
            }

            // Only the current row sweeps; a finished or upcoming row is flat.
            let is_focus = i as isize == focus && lit > 0.0;
            let sweep = if is_focus && sweep_span(row) > 0.0 { ((time - row.time) / sweep_span(row)).clamp(0.0, 1.0) as f32 } else { 1.0 };
            let edge = -feather + sweep * (row.length + feather * 2.0);
            let (mask_a, mask_b) = (row.masks[low].as_ref().unwrap(), row.masks[high].as_ref());
            let spans = &row.spans;
            let line_h = (row.h as f32 - pad * 2.0) / spans.len().max(1) as f32;
            blit(pm, mask_a, if k > 0.01 { mask_b } else { None }, k, row.w, row.h, ox, oy, scale, &clip, &fade, |sx, sy| {
                let span = &spans[(((sy - pad) / line_h).max(0.0) as usize).min(spans.len() - 1)];
                let along = span.path + (sx - pad);
                let fill = if lit > 0.0 { ((edge - along + feather) / (2.0 * feather)).clamp(0.0, 1.0) * lit } else { 0.0 };
                let mut c = [0u8; 4];
                for n in 0..3 {
                    c[n] = lerp(dim_color[n] as f32, lit_color[n] as f32, fill) as u8;
                }
                c[3] = 255;
                let alpha = lerp(dim_color[3] as f32 / 255.0, lit_color[3] as f32 / 255.0, fill) * opacity;
                (c, alpha)
            });
        }
    }
}

fn position_of(index: isize, intro: bool) -> isize {
    if intro {
        index + 1
    } else {
        index.max(0)
    }
}

/// Lazily blurred copies, so rows that never get near the middle cost nothing.
fn row_mask(row: &mut Row, level: usize, px: f32) -> &Vec<u8> {
    const SIGMA_PER_LEVEL: [f32; BLUR_LEVELS] = [0.0, 0.018, 0.04, 0.065];
    if row.masks[level].is_none() {
        let blurred = gaussian(row.masks[0].as_ref().unwrap(), row.w, row.h, SIGMA_PER_LEVEL[level] * px);
        row.masks[level] = Some(blurred);
    }
    row.masks[level].as_ref().unwrap()
}

fn new_row(time: f64, next: f64, chars: usize, dots: bool, w: usize, h: usize, mask: Vec<u8>, spans: Vec<Span>, length: f32) -> Row {
    Row {
        time,
        next,
        chars,
        dots,
        w,
        h,
        masks: [Some(mask), None, None, None],
        glow: None,
        spans,
        length,
        layout_y: 0.0,
        y_from: 0.0,
        y_to: 0.0,
        blur_from: 0.0,
        blur_to: 0.0,
        lit_from: 0.0,
        lit_to: 0.0,
        scale_from: INACTIVE_SCALE,
        scale_to: INACTIVE_SCALE,
        delay: 0.0,
    }
}

fn dots_row(px: f32, time: f64, next: f64) -> Row {
    let pad = row_pad(px);
    let d = px * 0.3;
    let spacing = d * 1.6;
    let (body_w, body_h) = (spacing * 2.0 + d, (px * 1.1).ceil());
    let (w, h) = ((body_w.ceil() as usize + pad * 2).max(1), body_h as usize + pad * 2);
    let mut mask = vec![0u8; w * h];
    let cy = pad as f32 + body_h / 2.0;
    for n in 0..3 {
        let cx = pad as f32 + d / 2.0 + spacing * n as f32;
        for y in 0..h {
            for x in 0..w {
                let dist = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (d / 2.0 - dist + 0.5).clamp(0.0, 1.0);
                let i = y * w + x;
                mask[i] = mask[i].max((cov * 255.0) as u8);
            }
        }
    }
    let spans = vec![Span { path: 0.0 }];
    new_row(time, next, 0, true, w, h, mask, spans, body_w)
}

#[allow(clippy::too_many_arguments)]
fn text_row(pt: &mut Painter, font_name: &str, text: &str, px: f32, line_h: f32, max_w: f32, time: f64, next: f64) -> Option<Row> {
    let font = pt.font(font_name)?;
    let tracking = -0.02 * px;
    let pad = row_pad(px);
    let advance = |pt: &mut Painter, c: char| pt.advance(&font, c, px) + tracking;

    // Word wrap, breaking inside a word only if it is wider than the row by itself.
    let mut lines: Vec<(String, f32)> = vec![(String::new(), 0.0)];
    let space = advance(pt, ' ');
    for word in text.split_whitespace() {
        let word_w: f32 = word.chars().map(|c| advance(pt, c)).sum();
        let (current, current_w) = lines.last().unwrap();
        let joined = if current.is_empty() { word_w } else { current_w + space + word_w };
        if joined <= max_w || current.is_empty() && word_w <= max_w {
            let last = lines.last_mut().unwrap();
            if !last.0.is_empty() {
                last.0.push(' ');
            }
            last.0.push_str(word);
            last.1 = joined;
        } else if word_w <= max_w {
            lines.push((word.to_string(), word_w));
        } else {
            if !lines.last().unwrap().0.is_empty() {
                lines.push((String::new(), 0.0));
            }
            for c in word.chars() {
                let a = advance(pt, c);
                let last = lines.last_mut().unwrap();
                if last.1 + a > max_w && !last.0.is_empty() {
                    lines.push((c.to_string(), a));
                } else {
                    last.0.push(c);
                    last.1 += a;
                }
            }
        }
    }

    let w = max_w.ceil() as usize + pad * 2;
    let h = (lines.len() as f32 * line_h).ceil() as usize + pad * 2;
    let mut mask = vec![0u8; w * h];
    let ascent = font.horizontal_line_metrics(px).map_or(px * 0.8, |m| m.ascent);
    let lead = (line_h - px) / 2.0;
    let mut spans = Vec::new();
    let mut path = 0.0f32;
    for (n, (line, width)) in lines.iter().enumerate() {
        let top = n as f32 * line_h;
        let base = pad as f32 + top + lead + ascent * 0.95;
        let mut pen = pad as f32;
        for c in line.chars() {
            let glyph = pt.glyph(&font, c, px);
            let (metrics, bitmap) = (&glyph.0, &glyph.1);
            let gx = (pen + metrics.xmin as f32).round() as isize;
            let gy = (base - metrics.height as f32 - metrics.ymin as f32).round() as isize;
            for row in 0..metrics.height as isize {
                for col in 0..metrics.width as isize {
                    let (x, y) = (gx + col, gy + row);
                    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                        let i = y as usize * w + x as usize;
                        mask[i] = mask[i].max(bitmap[row as usize * metrics.width + col as usize]);
                    }
                }
            }
            pen += metrics.advance_width + tracking;
        }
        spans.push(Span { path });
        path += width + px * 0.45;
    }
    let length = (path - px * 0.45).max(1.0);
    let chars = text.chars().count();
    Some(new_row(time, next, chars, false, w, h, mask, spans, length))
}

struct Clip {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Alpha multiplier per row of the widget: fades out towards the top and bottom edges.
fn fade_table(h: usize) -> Vec<f32> {
    (0..h.max(1))
        .map(|y| {
            let t = y as f32 / h.max(1) as f32;
            (t / 0.14).min((1.0 - t) / 0.2).clamp(0.0, 1.0)
        })
        .collect()
}

/// Draws a mask with subpixel position and scale onto premultiplied pixels. `shade`
/// returns the colour and opacity for a point in mask coordinates.
#[allow(clippy::too_many_arguments)]
fn blit(
    pm: &mut Pixmap,
    a: &[u8],
    b: Option<&Vec<u8>>,
    k: f32,
    w: usize,
    h: usize,
    ox: f32,
    oy: f32,
    scale: f32,
    clip: &Clip,
    fade: &[f32],
    shade: impl Fn(f32, f32) -> ([u8; 4], f32),
) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let x0 = (ox.floor() as i32).max(clip.left).max(0);
    let x1 = ((ox + w as f32 * scale).ceil() as i32).min(clip.right).min(pw);
    let y0 = (oy.floor() as i32).max(clip.top).max(0);
    let y1 = ((oy + h as f32 * scale).ceil() as i32).min(clip.bottom).min(ph);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let inv = 1.0 / scale;
    let data = pm.data_mut();
    let sample = |m: &[u8], sx: f32, sy: f32| -> f32 {
        let (fx, fy) = (sx - 0.5, sy - 0.5);
        if fx < 0.0 || fy < 0.0 {
            return 0.0;
        }
        let (ix, iy) = (fx as usize, fy as usize);
        if ix + 1 >= w || iy + 1 >= h {
            return 0.0;
        }
        let (dx, dy) = (fx - ix as f32, fy - iy as f32);
        let at = |x: usize, y: usize| m[y * w + x] as f32;
        (at(ix, iy) * (1.0 - dx) + at(ix + 1, iy) * dx) * (1.0 - dy) + (at(ix, iy + 1) * (1.0 - dx) + at(ix + 1, iy + 1) * dx) * dy
    };
    for y in y0..y1 {
        let fade_a = fade[((y - clip.top) as usize).min(fade.len() - 1)];
        if fade_a <= 0.0 {
            continue;
        }
        let sy = (y as f32 + 0.5 - oy) * inv;
        for x in x0..x1 {
            let sx = (x as f32 + 0.5 - ox) * inv;
            let mut cov = sample(a, sx, sy);
            if let Some(b) = b {
                cov = lerp(cov, sample(b, sx, sy), k);
            }
            if cov < 0.5 {
                continue;
            }
            let (c, opacity) = shade(sx, sy);
            let alpha = (cov / 255.0 * opacity * fade_a).clamp(0.0, 1.0);
            let inv_a = 1.0 - alpha;
            let i = ((y * pw + x) * 4) as usize;
            for n in 0..3 {
                data[i + n] = (c[n] as f32 * alpha + data[i + n] as f32 * inv_a) as u8;
            }
            data[i + 3] = (alpha * 255.0 + data[i + 3] as f32 * inv_a) as u8;
        }
    }
}

// ---------- album art ----------

const ART_RADIUS: f32 = 0.05;
const ART_FADE_SECONDS: f32 = 0.6;
const PAUSE_SCALE: f32 = 0.88;
const PAUSE_SECONDS: f32 = 0.55;

#[derive(Default)]
pub struct ArtView {
    seq: u64,
    side: u32,
    current: Option<Pixmap>,
    previous: Option<Pixmap>,
    swapped_at: Option<Instant>,
    playing: Option<bool>,
    scale_from: f32,
    scale_to: f32,
    toggled_at: Option<Instant>,
}

impl ArtView {
    pub fn animating(&self) -> bool {
        self.swapped_at.is_some_and(|t| t.elapsed().as_secs_f32() < ART_FADE_SECONDS)
            || self.toggled_at.is_some_and(|t| t.elapsed().as_secs_f32() < PAUSE_SECONDS)
    }

    pub fn draw(&mut self, pm: &mut Pixmap, w: &Widget, m: &Music) {
        let (cw, ch) = (pm.width() as f32, pm.height() as f32);
        let unit = cw.min(ch);
        let side = (w.w.min(w.h) * unit).max(8.0);
        let (cx, cy) = (w.x * cw, w.y * ch);
        let now = Instant::now();

        let scale_now = self.scale(now);
        let playing = m.playing || !m.present;
        if self.playing != Some(playing) {
            let first = self.playing.is_none();
            self.playing = Some(playing);
            self.scale_to = if playing { 1.0 } else { PAUSE_SCALE };
            self.scale_from = if first { self.scale_to } else { scale_now };
            self.toggled_at = (!first).then_some(now);
        }
        let scale = self.scale(now);

        let seq = m.art.as_ref().map_or(0, |a| a.seq);
        if seq != self.seq || side as u32 != self.side {
            let resized_only = seq == self.seq;
            self.seq = seq;
            self.side = side as u32;
            let next = m.art.as_ref().and_then(|a| rounded_art(a, side as u32));
            self.previous = if resized_only { None } else { self.current.take() };
            self.swapped_at = (!resized_only && self.previous.is_some()).then_some(now);
            self.current = next;
        }

        let drawn = side * scale;
        let (left, top) = (cx - drawn / 2.0, cy - drawn / 2.0);
        // A soft shadow from stacked faint outlines, as tiny-skia has no blur.
        for layer in 1..=5 {
            let grow = layer as f32 * side * 0.012;
            if let Some(path) = crate::render::rounded_rect(left - grow, top - grow + side * 0.02, drawn + grow * 2.0, drawn + grow * 2.0, drawn * ART_RADIUS + grow) {
                let mut p = Paint::default();
                p.set_color_rgba8(0, 0, 0, 16);
                p.anti_alias = true;
                pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
        if self.current.is_none() {
            if let Some(path) = crate::render::rounded_rect(left, top, drawn, drawn, drawn * ART_RADIUS) {
                let mut p = Paint::default();
                p.anti_alias = true;
                if let Some(shader) = LinearGradient::new(
                    Point::from_xy(left, top),
                    Point::from_xy(left + drawn, top + drawn),
                    vec![
                        GradientStop::new(0.0, tiny_skia::Color::from_rgba8(250, 61, 92, 255)),
                        GradientStop::new(1.0, tiny_skia::Color::from_rgba8(74, 26, 107, 255)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    p.shader = shader;
                }
                pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
        let fade = self.swapped_at.map_or(1.0, |t| (now.duration_since(t).as_secs_f32() / ART_FADE_SECONDS).min(1.0));
        let place = Transform::from_row(scale, 0.0, 0.0, scale, left, top);
        if fade < 1.0 {
            if let Some(prev) = &self.previous {
                let paint = PixmapPaint { opacity: w.opacity, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
                pm.draw_pixmap(0, 0, prev.as_ref(), &paint, place, None);
            }
        }
        if let Some(current) = &self.current {
            let paint = PixmapPaint { opacity: w.opacity * fade, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
            pm.draw_pixmap(0, 0, current.as_ref(), &paint, place, None);
        }
    }

    fn scale(&self, now: Instant) -> f32 {
        match self.toggled_at {
            Some(t) => lerp(self.scale_from, self.scale_to, spring(now.duration_since(t).as_secs_f32() / PAUSE_SECONDS)),
            None if self.playing.is_some() => self.scale_to,
            None => 1.0,
        }
    }
}

fn rounded_art(art: &crate::music::Art, side: u32) -> Option<Pixmap> {
    use fast_image_resize::{images::Image, images::ImageRef, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    let mut out = vec![0u8; (side * side * 4) as usize];
    let src = ImageRef::new(art.side, art.side, &art.rgba, PixelType::U8x4).ok()?;
    let mut dst = Image::from_slice_u8(side, side, &mut out, PixelType::U8x4).ok()?;
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new().resize(&src, &mut dst, &options).ok()?;
    let s = side as f32;
    let r = s * ART_RADIUS;
    for y in 0..side as usize {
        for x in 0..side as usize {
            let qx = ((x as f32 + 0.5) - s / 2.0).abs() - (s / 2.0 - r);
            let qy = ((y as f32 + 0.5) - s / 2.0).abs() - (s / 2.0 - r);
            let dist = qx.max(0.0).hypot(qy.max(0.0)) - r;
            let cov = (0.5 - dist).clamp(0.0, 1.0);
            let i = (y * side as usize + x) * 4;
            // Artwork is treated as opaque, then premultiplied by the corner coverage.
            for c in 0..3 {
                out[i + c] = (out[i + c] as f32 * cov) as u8;
            }
            out[i + 3] = (cov * 255.0) as u8;
        }
    }
    Pixmap::from_vec(out, tiny_skia::IntSize::from_wh(side, side)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(times: &[f64]) -> Vec<Line> {
        times.iter().map(|t| Line { time: *t, text: "x".into() }).collect()
    }

    #[test]
    fn active_line_follows_time() {
        let l = lines(&[5.0, 10.0, 15.0]);
        assert_eq!(active_index(&l, 1.0), -1);
        assert_eq!(active_index(&l, 5.0), 0);
        assert_eq!(active_index(&l, 12.0), 1);
        assert_eq!(active_index(&l, 99.0), 2);
        assert_eq!(active_index(&[], 1.0), -1);
    }

    #[test]
    fn intro_dots_take_the_first_row() {
        assert_eq!(position_of(-1, true), 0);
        assert_eq!(position_of(0, true), 1);
        assert_eq!(position_of(-1, false), 0);
        assert_eq!(position_of(3, false), 3);
    }

    #[test]
    fn spring_settles_exactly_at_its_target() {
        assert_eq!(spring(1.0), 1.0);
        assert!(spring(0.0).abs() < 1e-5);
        assert_eq!(spring(5.0), 1.0);
    }

    #[test]
    fn blur_keeps_the_mask_size_and_softens_edges() {
        let mut src = vec![0u8; 21 * 21];
        src[10 * 21 + 10] = 255;
        let out = gaussian(&src, 21, 21, 1.5);
        assert_eq!(out.len(), src.len());
        assert!(out[10 * 21 + 10] < 255 && out[10 * 21 + 11] > 0);
    }

    #[test]
    fn fade_is_opaque_in_the_middle_and_clear_at_the_edges() {
        let t = fade_table(100);
        assert_eq!(t[0], 0.0);
        assert_eq!(t[50], 1.0);
        assert!(t[99] < 0.1);
    }
}
