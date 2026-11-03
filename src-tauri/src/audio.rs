//! Audio spectrum background, from a WASAPI loopback of the default output.

use crate::media::{parse_color, Shared};
use crate::model::Background;
use realfft::RealFftPlanner;
use std::time::{Duration, Instant};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

const FFT: usize = 2048;
const BANDS: usize = 48;

struct Capture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    channels: usize,
}

unsafe fn open() -> windows::core::Result<Capture> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let channels = (*format).nChannels as usize;
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 1_000_000, 0, format, None)?;
    CoTaskMemFree(Some(format as *const _));
    let capture = client.GetService()?;
    client.Start()?;
    Ok(Capture { client, capture, channels })
}

impl Capture {
    /// Appends newly captured mono samples. The shared-mode mix format is 32-bit float.
    unsafe fn drain(&self, into: &mut Vec<f32>) -> windows::core::Result<()> {
        loop {
            if self.capture.GetNextPacketSize()? == 0 {
                return Ok(());
            }
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
            self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            if silent || data.is_null() {
                into.resize(into.len() + frames as usize, 0.0);
            } else {
                let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * self.channels);
                into.extend(samples.chunks_exact(self.channels).map(|c| c.iter().sum::<f32>() / self.channels as f32));
            }
            self.capture.ReleaseBuffer(frames)?;
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub fn visualizer(s: &Shared, b: &Background, fps: u32) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = run(s, b, fps);
        CoUninitialize();
        result
    }
}

unsafe fn run(s: &Shared, b: &Background, fps: u32) -> Result<(), String> {
    let cap = open().map_err(|e| format!("audio capture: {e}"))?;
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let window: Vec<f32> = (0..FFT)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT - 1) as f32).cos())
        .collect();
    let mut samples: Vec<f32> = Vec::with_capacity(FFT * 4);
    let mut bars = [0f32; BANDS];
    let (bg, c1, c2) = (parse_color(&b.color), parse_color(&b.color2), parse_color(&b.color3));
    let (w, h) = (s.w as usize, s.h as usize);
    let mut frame = vec![255u8; w * h * 4];
    let frame_time = Duration::from_secs_f32(1.0 / fps.clamp(5, 60) as f32);
    let gain = b.speed.max(0.1);

    while !s.stop.load(std::sync::atomic::Ordering::Relaxed) {
        let t = Instant::now();
        cap.drain(&mut samples).map_err(|e| format!("audio capture: {e}"))?;
        if samples.len() > FFT {
            samples.drain(..samples.len() - FFT);
        }
        input.fill(0.0);
        let offset = FFT - samples.len();
        for (i, v) in samples.iter().enumerate() {
            input[offset + i] = v * window[offset + i];
        }
        if fft.process(&mut input, &mut spectrum).is_ok() {
            for (i, bar) in bars.iter_mut().enumerate() {
                // Log-spaced bands: music's energy sits in the low bins.
                let lo = (2.0f32 * (400.0f32).powf(i as f32 / BANDS as f32)) as usize;
                let hi = ((2.0f32 * (400.0f32).powf((i + 1) as f32 / BANDS as f32)) as usize).clamp(lo + 1, spectrum.len());
                let peak = spectrum[lo.min(spectrum.len() - 1)..hi].iter().map(|c| c.norm()).fold(0.0, f32::max);
                let target = ((peak * gain).max(1e-4).log10() / 2.6 + 0.35).clamp(0.0, 1.0);
                *bar = if target > *bar { *bar + (target - *bar) * 0.65 } else { (*bar - 0.035).max(target) };
            }
        }
        draw(&mut frame, w, h, &bars, bg, c1, c2, &b.style);
        s.publish(&frame);
        s.sleep(frame_time.saturating_sub(t.elapsed()));
    }
    Ok(())
}

fn draw(f: &mut [u8], w: usize, h: usize, bars: &[f32; BANDS], bg: [u8; 4], c1: [u8; 4], c2: [u8; 4], style: &str) {
    for px in f.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&bg[..3]);
    }
    let mix = |t: f32| [0, 1, 2].map(|i| (c1[i] as f32 + (c2[i] as f32 - c1[i] as f32) * t) as u8);
    if style == "radial" {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let inner = cx.min(cy) * 0.42;
        let span = cx.min(cy) * 0.55;
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let r = (dx * dx + dy * dy).sqrt();
                if r < inner || r > inner + span {
                    continue;
                }
                // Symmetric left/right so low and high bands do not meet in a hard seam.
                let a = (dy.atan2(dx) / std::f32::consts::PI).abs();
                let pos = a * BANDS as f32;
                let band = (pos as usize).min(BANDS - 1);
                if pos.fract() > 0.72 {
                    continue;
                }
                let t = (r - inner) / span;
                if t <= bars[band] {
                    let c = mix(t);
                    f[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&c);
                }
            }
        }
        return;
    }
    let mirror = style == "mirror";
    let band_w = w as f32 / BANDS as f32;
    for (i, &v) in bars.iter().enumerate() {
        let x0 = (i as f32 * band_w + band_w * 0.14) as usize;
        let x1 = (((i + 1) as f32 * band_w - band_w * 0.14) as usize).min(w);
        let extent = (v * if mirror { h as f32 / 2.0 } else { h as f32 * 0.92 }) as usize;
        for d in 0..extent {
            let c = mix(d as f32 / h as f32 * if mirror { 2.0 } else { 1.0 });
            let rows = if mirror { [h / 2 + d, (h / 2).saturating_sub(d + 1)] } else { [h - 1 - d, h - 1 - d] };
            for y in rows {
                if y < h {
                    for x in x0..x1 {
                        f[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&c);
                    }
                }
            }
        }
    }
}
