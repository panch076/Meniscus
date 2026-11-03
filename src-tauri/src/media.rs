//! Background producers. Each animated background runs on its own thread and
//! writes finished RGBA frames (already scaled to the canvas) into a shared slot;
//! the renderer picks up whichever frame is newest. Stopping a background is just
//! dropping its handle, so unused media costs nothing.

use crate::model::Background;
use fast_image_resize::{images::Image, FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Shared {
    pub w: u32,
    pub h: u32,
    pub seq: AtomicU64,
    pub rgba: Mutex<Vec<u8>>,
    pub stop: AtomicBool,
}

pub struct Handle(pub Arc<Shared>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    pub fn publish(&self, frame: &[u8]) {
        let mut d = self.rgba.lock().unwrap();
        d.clear();
        d.extend_from_slice(frame);
        drop(d);
        self.seq.fetch_add(1, Ordering::Release);
    }
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    /// Sleeps in short steps so a replaced background exits promptly.
    pub fn sleep(&self, mut d: Duration) {
        while d > Duration::ZERO && !self.stopped() {
            let step = d.min(Duration::from_millis(50));
            std::thread::sleep(step);
            d -= step;
        }
    }
}

pub fn start(bg: &Background, w: u32, h: u32, fps: u32, music: crate::music::SharedMusic) -> Handle {
    let shared = Arc::new(Shared {
        w,
        h,
        seq: AtomicU64::new(0),
        rgba: Mutex::new(vec![0; (w * h * 4) as usize]),
        stop: AtomicBool::new(false),
    });
    let (s, b) = (shared.clone(), bg.clone());
    let _ = std::thread::Builder::new().name(format!("bg-{}", bg.kind)).spawn(move || {
        // Media not chosen yet is a normal state while editing, not an error.
        let unset = match b.kind.as_str() {
            "image" | "gif" | "video" => b.path.is_empty(),
            "slideshow" => b.dir.is_empty(),
            _ => false,
        };
        if unset {
            let _ = solid(&s, &b);
            return;
        }
        let result = match b.kind.as_str() {
            "image" => image_source(&s, &b),
            "gif" => gif_source(&s, &b),
            "video" => crate::video::run(&s, &b),
            "slideshow" => slideshow(&s, &b),
            "gradient" => gradient(&s, &b, fps),
            "liquid" => liquid(&s, &b, fps),
            "screen" => crate::capture::screen(&s, &b, fps),
            "albumart" => art_backdrop(&s, &b, fps, &music),
            "visualizer" => crate::audio::visualizer(&s, &b, fps),
            _ => solid(&s, &b),
        };
        if let Err(e) = result {
            crate::store::log(&format!("background {}: {e}", b.kind));
            // A broken file must not leave the panel showing the previous profile.
            let _ = solid(&s, &b);
        }
    });
    Handle(shared)
}

pub fn parse_color(c: &str) -> [u8; 4] {
    let h = c.trim().trim_start_matches('#');
    let b = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match h.len() {
        3 => {
            let d = |i: usize| h.get(i..i + 1).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0) * 17;
            [d(0), d(1), d(2), 255]
        }
        8 => [b(0).unwrap_or(0), b(2).unwrap_or(0), b(4).unwrap_or(0), b(6).unwrap_or(255)],
        _ => [b(0).unwrap_or(0), b(2).unwrap_or(0), b(4).unwrap_or(0), 255],
    }
}

/// Scales arbitrary source frames into the canvas, honouring fit/zoom/offset.
pub struct Scaler {
    resizer: Resizer,
    dw: u32,
    dh: u32,
    fit: String,
    zoom: f32,
    ox: f32,
    oy: f32,
    fill: [u8; 4],
    pub out: Vec<u8>,
    scratch: Vec<u8>,
}

impl Scaler {
    pub fn new(b: &Background, dw: u32, dh: u32) -> Self {
        Self {
            resizer: Resizer::new(),
            dw,
            dh,
            fit: b.fit.clone(),
            zoom: b.zoom.max(0.1),
            ox: b.offset_x,
            oy: b.offset_y,
            fill: parse_color(&b.color),
            out: vec![0; (dw * dh * 4) as usize],
            scratch: Vec::new(),
        }
    }

    pub fn scale(&mut self, src: &[u8], sw: u32, sh: u32) -> Result<(), String> {
        let (dw, dh) = (self.dw as f64, self.dh as f64);
        let (fw, fh) = (sw as f64, sh as f64);
        let zoom = self.zoom as f64;
        // Region of the source that is shown, and the canvas area it lands on.
        let (mut cw, mut ch, mut tw, mut th) = (fw, fh, dw, dh);
        match self.fit.as_str() {
            "stretch" => {}
            "contain" => {
                let s = (dw / fw).min(dh / fh) * zoom;
                tw = (fw * s).min(dw).max(1.0);
                th = (fh * s).min(dh).max(1.0);
                cw = (tw / s).min(fw);
                ch = (th / s).min(fh);
            }
            _ => {
                let s = (dw / fw).max(dh / fh) * zoom;
                cw = (dw / s).min(fw);
                ch = (dh / s).min(fh);
            }
        }
        if self.fit == "stretch" {
            cw = fw / zoom;
            ch = fh / zoom;
        }
        let left = ((fw - cw) / 2.0 * (1.0 + self.ox as f64)).clamp(0.0, fw - cw);
        let top = ((fh - ch) / 2.0 * (1.0 + self.oy as f64)).clamp(0.0, fh - ch);

        let (tw, th) = (tw.round().max(1.0) as u32, th.round().max(1.0) as u32);
        // Image::from_slice_u8 needs a mutable slice; the source is not modified when used read-only.
        let src_image = fast_image_resize::images::ImageRef::new(sw, sh, src, PixelType::U8x4).map_err(|e| e.to_string())?;
        let options = ResizeOptions::new()
            .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
            .crop(left, top, cw, ch);
        if (tw, th) == (self.dw, self.dh) {
            let mut dst =
                Image::from_slice_u8(tw, th, &mut self.out, PixelType::U8x4).map_err(|e| e.to_string())?;
            self.resizer.resize(&src_image, &mut dst, &options).map_err(|e| e.to_string())?;
        } else {
            self.scratch.resize((tw * th * 4) as usize, 0);
            let mut dst =
                Image::from_slice_u8(tw, th, &mut self.scratch, PixelType::U8x4).map_err(|e| e.to_string())?;
            self.resizer.resize(&src_image, &mut dst, &options).map_err(|e| e.to_string())?;
            for px in self.out.chunks_exact_mut(4) {
                px.copy_from_slice(&self.fill);
            }
            let (x0, y0) = ((self.dw - tw) / 2, (self.dh - th) / 2);
            let stride = (self.dw * 4) as usize;
            for row in 0..th as usize {
                let d = (y0 as usize + row) * stride + x0 as usize * 4;
                let s = row * (tw * 4) as usize;
                self.out[d..d + (tw * 4) as usize].copy_from_slice(&self.scratch[s..s + (tw * 4) as usize]);
            }
        }
        Ok(())
    }
}

/// Flattens transparency onto the fill colour; the canvas itself is opaque.
fn flatten(rgba: &mut [u8], fill: [u8; 4]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a < 255 {
            for c in 0..3 {
                px[c] = ((px[c] as u32 * a + fill[c] as u32 * (255 - a)) / 255) as u8;
            }
            px[3] = 255;
        }
    }
}

fn solid(s: &Shared, b: &Background) -> Result<(), String> {
    let c = parse_color(&b.color);
    let mut f = vec![0u8; (s.w * s.h * 4) as usize];
    for px in f.chunks_exact_mut(4) {
        px.copy_from_slice(&[c[0], c[1], c[2], 255]);
    }
    s.publish(&f);
    Ok(())
}

fn load_rgba(path: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let img = image::open(path).map_err(|e| format!("{path}: {e}"))?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}

fn image_source(s: &Shared, b: &Background) -> Result<(), String> {
    let (mut px, w, h) = load_rgba(&b.path)?;
    flatten(&mut px, parse_color(&b.color));
    let mut scaler = Scaler::new(b, s.w, s.h);
    scaler.scale(&px, w, h)?;
    s.publish(&scaler.out);
    Ok(())
}

fn gif_source(s: &Shared, b: &Background) -> Result<(), String> {
    use image::{codecs::gif::GifDecoder, AnimationDecoder};
    let fill = parse_color(&b.color);
    let mut scaler = Scaler::new(b, s.w, s.h);
    let speed = b.speed.max(0.05) as f64;
    while !s.stopped() {
        // Frames are streamed rather than cached, so a long GIF costs one frame of memory.
        let file = std::fs::File::open(&b.path).map_err(|e| format!("{}: {e}", b.path))?;
        let decoder = GifDecoder::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
        let mut shown = 0;
        let mut next = Instant::now();
        for frame in decoder.into_frames() {
            if s.stopped() {
                return Ok(());
            }
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            // Browsers treat very short delays as 100 ms; matching them keeps GIFs at the speed people expect.
            let ms = if num <= 10 * den { 100.0 } else { num as f64 / den as f64 };
            let buf = frame.into_buffer();
            let (w, h) = buf.dimensions();
            let mut px = buf.into_raw();
            flatten(&mut px, fill);
            scaler.scale(&px, w, h)?;
            s.publish(&scaler.out);
            shown += 1;
            next += Duration::from_secs_f64(ms / 1000.0 / speed);
            s.sleep(next.saturating_duration_since(Instant::now()));
        }
        if shown <= 1 {
            s.sleep(Duration::from_secs(3600));
        }
    }
    Ok(())
}

fn slideshow(s: &Shared, b: &Background) -> Result<(), String> {
    let mut files: Vec<_> = std::fs::read_dir(&b.dir)
        .map_err(|e| format!("{}: {e}", b.dir))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
            matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "webp")
        })
        .collect();
    if files.is_empty() {
        return Err(format!("no images in {}", b.dir));
    }
    files.sort();
    let fill = parse_color(&b.color);
    let mut scaler = Scaler::new(b, s.w, s.h);
    let mut seed = chrono::Local::now().timestamp_subsec_nanos() as u64 | 1;
    let mut i = 0usize;
    while !s.stopped() {
        if b.shuffle {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            i = (seed % files.len() as u64) as usize;
        }
        match load_rgba(&files[i % files.len()].to_string_lossy()) {
            Ok((mut px, w, h)) => {
                flatten(&mut px, fill);
                scaler.scale(&px, w, h)?;
                s.publish(&scaler.out);
            }
            Err(e) => crate::store::log(&e),
        }
        i = i.wrapping_add(1);
        s.sleep(Duration::from_secs_f32(b.interval.max(1.0)));
    }
    Ok(())
}

fn gradient(s: &Shared, b: &Background, fps: u32) -> Result<(), String> {
    let (a, c) = (parse_color(&b.color), parse_color(&b.color2));
    let (w, h) = (s.w as usize, s.h as usize);
    let mut f = vec![255u8; w * h * 4];
    let start = Instant::now();
    let animated = b.speed > 0.0;
    let frame_time = Duration::from_secs_f32(1.0 / fps.clamp(1, 15) as f32);
    loop {
        let angle = (b.angle + if animated { start.elapsed().as_secs_f32() * 12.0 * b.speed } else { 0.0 }).to_radians();
        let (dx, dy) = (angle.cos(), angle.sin());
        let extent = (dx.abs() * w as f32 + dy.abs() * h as f32).max(1.0);
        for y in 0..h {
            let row = &mut f[y * w * 4..(y + 1) * w * 4];
            let base = (y as f32 - h as f32 / 2.0) * dy;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let t = (((x as f32 - w as f32 / 2.0) * dx + base) / extent + 0.5).clamp(0.0, 1.0);
                for i in 0..3 {
                    px[i] = (a[i] as f32 + (c[i] as f32 - a[i] as f32) * t) as u8;
                }
            }
        }
        s.publish(&f);
        if !animated || s.stopped() {
            return Ok(());
        }
        s.sleep(frame_time);
    }
}

const BACKDROP_SOURCE: usize = 48;

/// The artwork, heavily blurred and saturated, on a tiny grid. Lyrical's fallback colours when there is none.
fn backdrop_source(art: Option<&crate::music::Art>) -> Vec<f32> {
    let n = BACKDROP_SOURCE;
    let mut rgb = vec![0f32; n * n * 3];
    let Some(art) = art else {
        for y in 0..n {
            for x in 0..n {
                let t = (x + y) as f32 / (2 * n - 2) as f32;
                let (a, c) = ([74.0, 26.0, 107.0], [31.0, 15.0, 61.0]);
                for ch in 0..3 {
                    rgb[(y * n + x) * 3 + ch] = a[ch] + (c[ch] - a[ch]) * t;
                }
            }
        }
        return rgb;
    };
    let mut small = vec![0u8; n * n * 4];
    if let (Ok(src), Ok(mut dst)) = (
        fast_image_resize::images::ImageRef::new(art.side, art.side, &art.rgba, PixelType::U8x4),
        Image::from_slice_u8(n as u32, n as u32, &mut small, PixelType::U8x4),
    ) {
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Box));
        let _ = Resizer::new().resize(&src, &mut dst, &options);
    }
    for (i, px) in small.chunks_exact(4).enumerate() {
        // Saturation 1.35, as in Lyrical.
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        for (ch, v) in [r, g, b].into_iter().enumerate() {
            rgb[i * 3 + ch] = (luma + (v - luma) * 1.35).clamp(0.0, 255.0);
        }
    }
    // Three box passes approximate a wide gaussian.
    let mut temp = rgb.clone();
    for _ in 0..3 {
        const R: isize = 4;
        for ch in 0..3 {
            for y in 0..n {
                for x in 0..n {
                    let mut acc = 0.0;
                    for d in -R..=R {
                        let sx = (x as isize + d).clamp(0, n as isize - 1) as usize;
                        acc += rgb[(y * n + sx) * 3 + ch];
                    }
                    temp[(y * n + x) * 3 + ch] = acc / (2 * R + 1) as f32;
                }
            }
            for y in 0..n {
                for x in 0..n {
                    let mut acc = 0.0;
                    for d in -R..=R {
                        let sy = (y as isize + d).clamp(0, n as isize - 1) as usize;
                        acc += temp[(sy * n + x) * 3 + ch];
                    }
                    rgb[(y * n + x) * 3 + ch] = acc / (2 * R + 1) as f32;
                }
            }
        }
    }
    rgb
}

/// Blurred artwork drifting behind everything, with a dark shade so white text reads.
/// Slow-moving and smooth, so it is computed small and scaled up.
fn art_backdrop(s: &Shared, b: &Background, fps: u32, music: &crate::music::SharedMusic) -> Result<(), String> {
    const GRID: usize = 96;
    const DRIFT_SECONDS: f32 = 28.0;
    const CROSSFADE_SECONDS: f32 = 1.2;
    let n = BACKDROP_SOURCE;
    let mut seq = u64::MAX;
    let mut current = backdrop_source(None);
    let mut previous = current.clone();
    let mut swapped = Instant::now() - Duration::from_secs(10);
    let mut small = vec![255u8; GRID * GRID * 4];
    let mut out = vec![0u8; (s.w * s.h * 4) as usize];
    let mut resizer = Resizer::new();
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    let start = Instant::now();
    let frame_time = Duration::from_secs_f32(1.0 / fps.clamp(1, 20) as f32);
    let shade = (1.0 - b.dim.clamp(0.0, 1.0)) * 0.74;
    let speed = b.speed.max(0.05);

    while !s.stopped() {
        let tick = Instant::now();
        let art = music.read().unwrap().art.clone();
        let art_seq = art.as_ref().map_or(0, |a| a.seq);
        if art_seq != seq {
            // The very first artwork appears at once; later ones crossfade.
            let first = seq == u64::MAX;
            seq = art_seq;
            previous = std::mem::replace(&mut current, backdrop_source(art.as_deref()));
            if !first {
                swapped = Instant::now();
            }
        }
        let mix = (swapped.elapsed().as_secs_f32() / CROSSFADE_SECONDS).min(1.0);

        let t = start.elapsed().as_secs_f32() * speed;
        let phase = 0.5 - 0.5 * (std::f32::consts::PI * t / DRIFT_SECONDS).cos();
        let zoom = 1.35 * (1.0 + 0.1 * phase);
        let (sin, cos) = (-5f32 + 10.0 * phase).to_radians().sin_cos();
        let last = (n - 1) as f32;
        for y in 0..GRID {
            let v = (y as f32 + 0.5) / GRID as f32 - 0.5;
            for x in 0..GRID {
                let u = (x as f32 + 0.5) / GRID as f32 - 0.5;
                let su = ((u * cos - v * sin) / zoom + 0.5) * last;
                let sv = ((u * sin + v * cos) / zoom + 0.5) * last;
                let (ix, iy) = ((su as usize).min(n - 2), (sv as usize).min(n - 2));
                let (dx, dy) = (su - ix as f32, sv - iy as f32);
                let r = (u * u + v * v).sqrt() / 0.707;
                let vignette = 1.0 - 0.35 * ((r - 0.35) / 0.65).clamp(0.0, 1.0);
                let i = (y * GRID + x) * 4;
                for ch in 0..3 {
                    let sample = |g: &[f32]| {
                        let at = |ax: usize, ay: usize| g[(ay * n + ax) * 3 + ch];
                        (at(ix, iy) * (1.0 - dx) + at(ix + 1, iy) * dx) * (1.0 - dy)
                            + (at(ix, iy + 1) * (1.0 - dx) + at(ix + 1, iy + 1) * dx) * dy
                    };
                    let mut value = sample(&current);
                    if mix < 1.0 {
                        value = sample(&previous) + (value - sample(&previous)) * mix;
                    }
                    small[i + ch] = (value * shade * vignette) as u8;
                }
            }
        }
        let src = fast_image_resize::images::ImageRef::new(GRID as u32, GRID as u32, &small, PixelType::U8x4).map_err(|e| e.to_string())?;
        let mut dst = Image::from_slice_u8(s.w, s.h, &mut out, PixelType::U8x4).map_err(|e| e.to_string())?;
        resizer.resize(&src, &mut dst, &options).map_err(|e| e.to_string())?;
        s.publish(&out);
        s.sleep(frame_time.saturating_sub(tick.elapsed()));
    }
    Ok(())
}

/// Soft drifting colour fields. Computed on a small grid and upscaled, which is
/// both cheap and exactly the smooth look this effect wants.
fn liquid(s: &Shared, b: &Background, fps: u32) -> Result<(), String> {
    const GRID: u32 = 64;
    let cols = [parse_color(&b.color), parse_color(&b.color2), parse_color(&b.color3)];
    let mut small = vec![255u8; (GRID * GRID * 4) as usize];
    let mut resizer = Resizer::new();
    let mut out = vec![0u8; (s.w * s.h * 4) as usize];
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    let start = Instant::now();
    let frame_time = Duration::from_secs_f32(1.0 / fps.clamp(1, 24) as f32);
    let speed = b.speed.max(0.01);
    while !s.stopped() {
        let t = start.elapsed().as_secs_f32() * 0.35 * speed;
        let blobs = [
            ((t * 0.9).sin() * 0.45 + 0.5, (t * 0.7 + 1.0).cos() * 0.45 + 0.5),
            ((t * 0.6 + 2.0).cos() * 0.45 + 0.5, (t * 1.1).sin() * 0.45 + 0.5),
        ];
        for y in 0..GRID {
            let v = y as f32 / (GRID - 1) as f32;
            for x in 0..GRID {
                let u = x as f32 / (GRID - 1) as f32;
                let i = ((y * GRID + x) * 4) as usize;
                let wave = ((u * 3.0 + t).sin() + (v * 4.0 - t * 0.8).cos()) * 0.04;
                let d1 = (((u - blobs[0].0).powi(2) + (v - blobs[0].1).powi(2)) * 3.2).min(1.0);
                let d2 = (((u - blobs[1].0).powi(2) + (v - blobs[1].1).powi(2)) * 3.2).min(1.0);
                let w1 = ((1.0 - d1).powi(2) + wave).clamp(0.0, 1.0);
                let w2 = ((1.0 - d2).powi(2) - wave).clamp(0.0, 1.0);
                for ch in 0..3 {
                    let base = cols[0][ch] as f32;
                    let mixed = base + (cols[1][ch] as f32 - base) * w1;
                    small[i + ch] = (mixed + (cols[2][ch] as f32 - mixed) * w2).clamp(0.0, 255.0) as u8;
                }
            }
        }
        let src = fast_image_resize::images::ImageRef::new(GRID, GRID, &small, PixelType::U8x4).map_err(|e| e.to_string())?;
        let mut dst = Image::from_slice_u8(s.w, s.h, &mut out, PixelType::U8x4).map_err(|e| e.to_string())?;
        resizer.resize(&src, &mut dst, &options).map_err(|e| e.to_string())?;
        s.publish(&out);
        s.sleep(frame_time);
    }
    Ok(())
}
