//! Screen mirroring. GDI's StretchBlt scales straight into a canvas-sized buffer,
//! so a 4K desktop costs no more memory than a 480x480 one.

use crate::media::Shared;
use crate::model::Background;
use serde::Serialize;
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::*;

#[derive(Clone, Serialize)]
pub struct MonitorInfo {
    pub index: u32,
    pub label: String,
}

fn monitor_rects() -> Vec<RECT> {
    unsafe extern "system" fn cb(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        (*(data.0 as *mut Vec<RECT>)).push(*rect);
        true.into()
    }
    let mut rects = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut rects as *mut _ as isize));
    }
    rects
}

pub fn monitors() -> Vec<MonitorInfo> {
    monitor_rects()
        .iter()
        .enumerate()
        .map(|(i, r)| MonitorInfo {
            index: i as u32,
            label: format!("Display {} ({}x{})", i + 1, r.right - r.left, r.bottom - r.top),
        })
        .collect()
}

pub fn screen(s: &Shared, b: &Background, fps: u32) -> Result<(), String> {
    let rects = monitor_rects();
    let rect = *rects.get(b.monitor as usize).or(rects.first()).ok_or("no display")?;
    let (sw, sh) = (rect.right - rect.left, rect.bottom - rect.top);
    let (dw, dh) = (s.w as i32, s.h as i32);

    // Centre-crop the desktop to the canvas shape unless the user asked to stretch.
    let (cw, ch) = if b.fit == "stretch" {
        (sw, sh)
    } else {
        let scale = (dw as f32 / sw as f32).max(dh as f32 / sh as f32) * b.zoom.max(0.1);
        (((dw as f32 / scale) as i32).min(sw), ((dh as f32 / scale) as i32).min(sh))
    };
    let (cx, cy) = (rect.left + (sw - cw) / 2, rect.top + (sh - ch) / 2);
    let frame_time = Duration::from_secs_f32(1.0 / fps.clamp(1, 30) as f32);

    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: dw,
                biHeight: -dh,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
        let old = SelectObject(mem, dib.into());
        SetStretchBltMode(mem, HALFTONE);
        let mut out = vec![255u8; (dw * dh * 4) as usize];
        while !s.stop.load(std::sync::atomic::Ordering::Relaxed) {
            let t = Instant::now();
            if StretchBlt(mem, 0, 0, dw, dh, Some(screen), cx, cy, cw, ch, SRCCOPY).as_bool() {
                let src = std::slice::from_raw_parts(bits as *const u8, out.len());
                for (d, p) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d[0] = p[2];
                    d[1] = p[1];
                    d[2] = p[0];
                }
                s.publish(&out);
            }
            s.sleep(frame_time.saturating_sub(t.elapsed()));
        }
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
    Ok(())
}
