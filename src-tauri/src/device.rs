//! USB transport for the Thermalright bulk LCD family (87AD:70DB).
//!
//! The panel falls back to its boot logo about 2-3 s after the last frame, so
//! the newest frame is re-sent every 150 ms even when nothing changed. Re-sending
//! costs no rendering or encoding: it is the same cached JPEG.

use nusb::transfer::{Buffer, Bulk, In, Out};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use nusb::MaybeFuture;

const VID: u16 = 0x87AD;
const PID: u16 = 0x70DB;
const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const MAGIC: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
const CHUNK: usize = 16 * 1024;
const KEEPALIVE: Duration = Duration::from_millis(150);
const IO_TIMEOUT: Duration = Duration::from_millis(3000);
const FILLER_STEP: usize = 64 * 1024;
/// Covers the largest frame this app can have been part-way through.
const FILLER_STEPS: usize = 16;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    /// searching | busy | stuck | connected
    pub state: String,
    pub detail: String,
    pub pm: u8,
    pub sub: u8,
    pub width: u32,
    pub height: u32,
    pub jpeg: bool,
    pub frames: u64,
    pub errors: u64,
}

/// Native panel geometry as reported by the handshake.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Panel {
    pub width: u32,
    pub height: u32,
    pub jpeg: bool,
    pub big_endian: bool,
}

/// PM byte to panel size, per the table documented by the trcc-linux project.
/// Any PM not listed is the family's 480x480 base.
pub fn panel_for(pm: u8, sub: u8) -> Panel {
    let (width, height) = match (pm, sub) {
        (1, 48) | (63, _) | (64, _) => (1600, 720),
        (1, 49) | (65, _) | (66, _) => (1920, 462),
        (5, _) | (50, _) => (320, 240),
        (7, _) | (14, _) => (640, 480),
        (9, _) | (11, _) | (20, _) => (854, 480),
        (10, _) | (16, _) => (960, 540),
        (12, _) => (800, 480),
        (13, _) | (17, _) | (18, _) => (960, 320),
        (15, _) => (640, 172),
        (32, _) => (320, 320),
        (68, _) => (1280, 480),
        (69, _) => (1920, 440),
        _ => (480, 480),
    };
    Panel { width, height, jpeg: pm != 32 && pm != 50, big_endian: pm == 50 }
}

/// A finished frame: the wire payload plus its encoding-independent identity.
pub struct Frame {
    pub seq: u64,
    pub payload: Vec<u8>,
    /// False means raw RGB565.
    pub jpeg: bool,
    pub big_endian: bool,
    pub width: u32,
    pub height: u32,
}

/// Latest-wins mailbox between the renderer and the USB thread: the renderer is
/// never blocked by a slow transfer, and stale frames are simply skipped.
#[derive(Default)]
pub struct FrameSlot {
    latest: Mutex<Option<Arc<Frame>>>,
    wake: Condvar,
    /// Held while a frame is on the wire, so quitting can wait for it to finish.
    in_flight: Mutex<()>,
    quitting: AtomicBool,
    pub sent: AtomicU64,
    pub bytes: AtomicU64,
}

impl FrameSlot {
    pub fn publish(&self, frame: Frame) {
        *self.latest.lock().unwrap() = Some(Arc::new(frame));
        self.wake.notify_one();
    }
    /// Stops sending and waits out the frame in progress. Dying mid-frame leaves the
    /// firmware waiting for the rest of it, and it then ignores handshakes.
    pub fn quiesce(&self) {
        self.quitting.store(true, Ordering::SeqCst);
        drop(self.in_flight.lock());
    }
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.latest.lock().unwrap().clone()
    }
    fn wait_newer(&self, than: u64, timeout: Duration) -> Option<Arc<Frame>> {
        let guard = self.latest.lock().unwrap();
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, timeout, |f| f.as_ref().map_or(true, |f| f.seq == than))
            .unwrap();
        guard.clone()
    }
}

pub type StatusCell = Arc<Mutex<DeviceStatus>>;

pub fn spawn(slot: Arc<FrameSlot>, status: StatusCell, panel: Arc<Mutex<Panel>>) {
    std::thread::Builder::new()
        .name("usb".into())
        .spawn(move || run(slot, status, panel))
        .expect("spawn usb thread");
}

fn set_state(status: &StatusCell, state: &str, detail: &str) {
    let mut s = status.lock().unwrap();
    if s.state != state || s.detail != detail {
        crate::store::log(&format!("device: {state} {detail}"));
    }
    s.state = state.into();
    s.detail = detail.into();
}

fn run(slot: Arc<FrameSlot>, status: StatusCell, panel: Arc<Mutex<Panel>>) {
    set_state(&status, "searching", "");
    let mut failures = 0u32;
    loop {
        // Recovery floods the panel with filler, so after a few tries it becomes occasional.
        let recover = failures > 0 && (failures <= 3 || failures % 30 == 0);
        match session(&slot, &status, &panel, recover) {
            Ok(()) => set_state(&status, "searching", ""),
            Err(SessionEnd::NotFound) => {
                failures = 0;
                set_state(&status, "searching", "");
            }
            Err(SessionEnd::Busy(e)) => {
                failures = 0;
                set_state(&status, "busy", &e);
            }
            Err(SessionEnd::Failed(e)) => {
                failures += 1;
                status.lock().unwrap().errors += 1;
                set_state(&status, if failures >= 3 { "stuck" } else { "searching" }, &e);
            }
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}

enum SessionEnd {
    NotFound,
    Busy(String),
    Failed(String),
}

fn session(slot: &FrameSlot, status: &StatusCell, panel: &Mutex<Panel>, recover: bool) -> Result<(), SessionEnd> {
    let info = nusb::list_devices()
        .wait()
        .map_err(|e| SessionEnd::Failed(e.to_string()))?
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
        .ok_or(SessionEnd::NotFound)?;
    let device = info.open().wait().map_err(|e| SessionEnd::Busy(e.to_string()))?;
    let iface = device.claim_interface(0).wait().map_err(|e| SessionEnd::Busy(e.to_string()))?;
    let mut out = iface.endpoint::<Bulk, Out>(EP_OUT).map_err(|e| SessionEnd::Failed(e.to_string()))?;
    let mut inp = iface.endpoint::<Bulk, In>(EP_IN).map_err(|e| SessionEnd::Failed(e.to_string()))?;

    let (pm, sub) = handshake(&mut out, &mut inp, recover)?;
    let geometry = panel_for(pm, sub);
    *panel.lock().unwrap() = geometry;
    {
        let mut s = status.lock().unwrap();
        s.pm = pm;
        s.sub = sub;
        s.width = geometry.width;
        s.height = geometry.height;
        s.jpeg = geometry.jpeg;
    }
    set_state(status, "connected", "");

    let mut last_seq = u64::MAX;
    let mut last_send = Instant::now() - KEEPALIVE;
    loop {
        let wait = KEEPALIVE.saturating_sub(last_send.elapsed());
        let frame = match slot.wait_newer(last_seq, wait) {
            Some(f) => f,
            None => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        if frame.seq == last_seq && last_send.elapsed() < KEEPALIVE {
            continue;
        }
        {
            let _sending = slot.in_flight.lock().unwrap();
            if slot.quitting.load(Ordering::SeqCst) {
                return Ok(());
            }
            send_frame(&mut out, &frame).map_err(SessionEnd::Failed)?;
        }
        last_seq = frame.seq;
        last_send = Instant::now();
        slot.sent.fetch_add(1, Ordering::Relaxed);
        slot.bytes.fetch_add(frame.payload.len() as u64, Ordering::Relaxed);
        status.lock().unwrap().frames += 1;
    }
}

fn handshake(out: &mut nusb::Endpoint<Bulk, Out>, inp: &mut nusb::Endpoint<Bulk, In>, recover: bool) -> Result<(u8, u8), SessionEnd> {
    if !recover {
        return query(out, inp, IO_TIMEOUT).map_err(|e| SessionEnd::Failed(format!("handshake: {e}")));
    }
    // A panel abandoned mid-frame treats whatever arrives as the rest of that image.
    // Filler lets it finish, after which it answers again.
    crate::store::log("device: attempting recovery");
    let _ = inp.clear_halt().wait();
    let _ = out.clear_halt().wait();
    let filler = vec![0u8; FILLER_STEP];
    let mut last = String::new();
    for step in 0..FILLER_STEPS {
        match query(out, inp, Duration::from_millis(400)) {
            Ok(found) => {
                crate::store::log(&format!("device: recovered after {} KB of filler", step * FILLER_STEP / 1024));
                return Ok(found);
            }
            Err(e) => last = e,
        }
        if let Err(e) = write_all(out, &filler) {
            return Err(SessionEnd::Failed(format!("recovery: {e}")));
        }
    }
    Err(SessionEnd::Failed(format!("handshake: {last}")))
}

fn query(out: &mut nusb::Endpoint<Bulk, Out>, inp: &mut nusb::Endpoint<Bulk, In>, timeout: Duration) -> Result<(u8, u8), String> {
    let mut req = [0u8; 64];
    req[..4].copy_from_slice(&MAGIC);
    req[56] = 1;
    write_all(out, &req)?;

    let mut buf = Buffer::new(1024);
    buf.set_requested_len(1024);
    inp.submit(buf);
    let Some(done) = inp.wait_next_complete(timeout) else {
        // The cancelled read must be collected, or the next attempt would receive it as its reply.
        inp.cancel_all();
        while inp.pending() > 0 && inp.wait_next_complete(Duration::from_millis(500)).is_some() {}
        return Err("no reply".into());
    };
    done.status.map_err(|e| e.to_string())?;
    let resp = &done.buffer[..done.actual_len];
    if resp.len() < 41 || resp[24] == 0 {
        return Err(format!("unrecognised reply ({} bytes)", resp.len()));
    }
    Ok((resp[24], resp[36]))
}

fn send_frame(out: &mut nusb::Endpoint<Bulk, Out>, frame: &Frame) -> Result<(), String> {
    let payload = &frame.payload;
    let mut wire = Vec::with_capacity(64 + payload.len());
    wire.extend_from_slice(&MAGIC);
    wire.extend_from_slice(&(if frame.jpeg { 2u32 } else { 3u32 }).to_le_bytes());
    wire.extend_from_slice(&frame.width.to_le_bytes());
    wire.extend_from_slice(&frame.height.to_le_bytes());
    wire.resize(56, 0);
    wire.extend_from_slice(&2u32.to_le_bytes());
    wire.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wire.extend_from_slice(payload);
    write_all(out, &wire)
}

fn write_all(out: &mut nusb::Endpoint<Bulk, Out>, data: &[u8]) -> Result<(), String> {
    for chunk in data.chunks(CHUNK) {
        let mut buf = out.allocate(chunk.len());
        buf.extend_from_slice(chunk);
        out.submit(buf);
        let done = out.wait_next_complete(IO_TIMEOUT).ok_or_else(|| {
            out.cancel_all();
            "write timed out".to_string()
        })?;
        done.status.map_err(|e| e.to_string())?;
        if done.actual_len < chunk.len() {
            return Err(format!("short write {}/{}", done.actual_len, chunk.len()));
        }
    }
    // A transfer ending exactly on a packet boundary needs a zero-length terminator.
    if data.len() % 512 == 0 {
        out.submit(out.allocate(0));
        let done = out.wait_next_complete(IO_TIMEOUT).ok_or("zlp timed out")?;
        done.status.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_table() {
        assert_eq!(panel_for(0, 0).width, 480);
        assert_eq!((panel_for(1, 48).width, panel_for(1, 48).height), (1600, 720));
        assert!(!panel_for(32, 0).jpeg);
        assert!(panel_for(50, 0).big_endian);
        assert!(panel_for(9, 0).jpeg);
    }
}
