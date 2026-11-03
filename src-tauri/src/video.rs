//! Video backgrounds through Windows Media Foundation: every codec the OS has,
//! with no bundled decoder. Audio is never decoded.

use crate::media::{Scaler, Shared};
use crate::model::Background;
use std::time::{Duration, Instant};
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

const STREAM_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;

pub fn run(s: &Shared, b: &Background) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let started = MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|e| e.to_string());
        let result = started.and_then(|_| play(s, b).map_err(|e| e.to_string()));
        let _ = MFShutdown();
        CoUninitialize();
        result
    }
}

unsafe fn play(s: &Shared, b: &Background) -> windows::core::Result<()> {
    let mut attrs = None;
    MFCreateAttributes(&mut attrs, 2)?;
    let attrs = attrs.unwrap();
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    let reader = MFCreateSourceReaderFromURL(&HSTRING::from(b.path.as_str()), &attrs)?;

    reader.SetStreamSelection(ALL_STREAMS, false)?;
    reader.SetStreamSelection(STREAM_VIDEO, true)?;
    let wanted = MFCreateMediaType()?;
    wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    reader.SetCurrentMediaType(STREAM_VIDEO, None, &wanted)?;

    let actual = reader.GetCurrentMediaType(STREAM_VIDEO)?;
    let size = actual.GetUINT64(&MF_MT_FRAME_SIZE)?;
    let (vw, vh) = ((size >> 32) as u32, size as u32);
    let default_stride = actual.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or(vw as i32 * 4);

    let mut scaler = Scaler::new(b, s.w, s.h);
    let mut rgba = vec![0u8; (vw * vh * 4) as usize];
    let speed = b.speed.max(0.05) as f64;
    // Target frame cadence: decoding faster than the panel can show is wasted work.
    let min_gap = Duration::from_secs_f64(1.0 / 60.0);

    let mut clock = Instant::now();
    let mut last_show = Instant::now() - min_gap;
    let mut shown_any = false;

    while !s_stopped(s) {
        let (mut flags, mut ts) = (0u32, 0i64);
        let mut sample = None;
        reader.ReadSample(STREAM_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;

        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            if !shown_any {
                return Err(windows::core::Error::new(windows::core::HRESULT(-1), "video has no frames"));
            }
            let zero = PROPVARIANT::from(0i64);
            reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &zero)?;
            let _ = reader.Flush(ALL_STREAMS);
            clock = Instant::now();
            continue;
        }
        let Some(sample) = sample else { continue };

        let due = clock + Duration::from_secs_f64(ts.max(0) as f64 / 1e7 / speed);
        let now = Instant::now();
        // Fell behind, or this frame would exceed the display cadence: drop it undecoded-to-RGB.
        if shown_any && (now > due + Duration::from_millis(80) || now.duration_since(last_show) < min_gap / 2) {
            continue;
        }
        if due > now {
            s.sleep(due - now);
        }

        let buffer = sample.ConvertToContiguousBuffer()?;
        copy_frame(&buffer, vw, vh, default_stride, &mut rgba)?;
        if scaler.scale(&rgba, vw, vh).is_ok() {
            s.publish(&scaler.out);
            shown_any = true;
            last_show = Instant::now();
        }
    }
    Ok(())
}

fn s_stopped(s: &Shared) -> bool {
    s.stop.load(std::sync::atomic::Ordering::Relaxed)
}

unsafe fn copy_frame(buffer: &IMFMediaBuffer, w: u32, h: u32, default_stride: i32, out: &mut [u8]) -> windows::core::Result<()> {
    let row = (w * 4) as usize;
    let copy = |scan0: *const u8, stride: isize, out: &mut [u8]| {
        for y in 0..h as usize {
            let src = std::slice::from_raw_parts(scan0.offset(y as isize * stride), row);
            let dst = &mut out[y * row..(y + 1) * row];
            for (d, p) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d[0] = p[2];
                d[1] = p[1];
                d[2] = p[0];
                d[3] = 255;
            }
        }
    };
    if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
        let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
        two_d.Lock2D(&mut scan0, &mut pitch)?;
        copy(scan0, pitch as isize, out);
        two_d.Unlock2D()?;
    } else {
        let mut data = std::ptr::null_mut();
        let mut len = 0;
        buffer.Lock(&mut data, None, Some(&mut len))?;
        if (len as usize) >= row * h as usize {
            // A negative default stride means bottom-up rows.
            let (scan0, stride) = if default_stride < 0 {
                (data.add(row * (h as usize - 1)) as *const u8, -(row as isize))
            } else {
                (data as *const u8, default_stride as isize)
            };
            copy(scan0, stride, out);
        }
        buffer.Unlock()?;
    }
    Ok(())
}
