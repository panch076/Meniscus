#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod capture;
mod device;
mod media;
mod lyricview;
mod model;
mod music;
mod render;
mod rules;
mod sensors;
mod store;
mod system;
mod video;

use device::{DeviceStatus, FrameSlot, Panel, StatusCell};
use model::{Profile, Settings};
use render::{EngineStats, Live, SharedLive, SharedStats};
use rules::{Config, SharedConfig};
use sensors::{Reading, SelfUsage, SharedSensors, Snapshot};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const WINDOW: &str = "main";

struct App {
    config: SharedConfig,
    live: SharedLive,
    stats: SharedStats,
    sensors: SharedSensors,
    status: StatusCell,
    slot: Arc<FrameSlot>,
    usage: Mutex<SelfUsage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    device: DeviceStatus,
    engine: EngineStats,
    cpu_percent: f64,
    memory_mb: f64,
    usb_frames: u64,
    usb_bytes: u64,
    sensor_count: usize,
    brightness_cap: u32,
    alert: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Coexistence {
    trcc_running: bool,
    trcc_autostart: bool,
    autostart: bool,
}

fn new_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("p{:x}{:x}", chrono::Local::now().timestamp_millis(), N.fetch_add(1, Ordering::Relaxed))
}

fn refresh_tray(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("tray") {
        if let Ok(menu) = build_tray_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

#[tauri::command]
fn get_status(app: State<App>) -> Status {
    let (cpu_percent, memory_mb) = app.usage.lock().unwrap().sample();
    let live = app.live.lock().unwrap();
    Status {
        device: app.status.lock().unwrap().clone(),
        engine: app.stats.lock().unwrap().clone(),
        cpu_percent,
        memory_mb,
        usb_frames: app.slot.sent.load(Ordering::Relaxed),
        usb_bytes: app.slot.bytes.load(Ordering::Relaxed),
        sensor_count: app.sensors.read().unwrap().list.len(),
        brightness_cap: live.brightness_cap,
        alert: live.alert.clone(),
    }
}

#[tauri::command]
fn get_sensors(app: State<App>) -> Vec<Reading> {
    app.sensors.read().unwrap().list.clone()
}

#[tauri::command]
fn get_profiles(app: State<App>) -> Vec<Profile> {
    app.config.lock().unwrap().profiles.clone()
}

#[tauri::command]
fn get_settings(app: State<App>) -> Settings {
    app.config.lock().unwrap().settings.clone()
}

#[tauri::command]
fn save_settings(app: State<App>, handle: AppHandle, settings: Settings) -> Result<(), String> {
    store::save_settings(&settings)?;
    let mut c = app.config.lock().unwrap();
    if c.settings.active != settings.active {
        c.manual_stamp += 1;
    }
    c.settings = settings;
    drop(c);
    refresh_tray(&handle);
    Ok(())
}

#[tauri::command]
fn save_profile(app: State<App>, handle: AppHandle, mut profile: Profile) -> Result<Profile, String> {
    if profile.id.is_empty() {
        profile.id = new_id();
    }
    store::save_profile(&profile)?;
    let mut c = app.config.lock().unwrap();
    let renamed = match c.profiles.iter_mut().find(|p| p.id == profile.id) {
        Some(existing) => {
            let renamed = existing.name != profile.name;
            *existing = profile.clone();
            renamed
        }
        None => {
            c.profiles.push(profile.clone());
            true
        }
    };
    drop(c);
    if renamed {
        refresh_tray(&handle);
    }
    Ok(profile)
}

#[tauri::command]
fn delete_profile(app: State<App>, handle: AppHandle, id: String) -> Result<(), String> {
    let mut c = app.config.lock().unwrap();
    if c.profiles.len() <= 1 {
        return Err("Keep at least one profile.".into());
    }
    store::delete_profile(&id)?;
    c.profiles.retain(|p| p.id != id);
    if c.settings.active == id {
        c.settings.active = c.profiles[0].id.clone();
        store::save_settings(&c.settings)?;
    }
    drop(c);
    refresh_tray(&handle);
    Ok(())
}

#[tauri::command]
fn import_asset(path: String) -> Result<String, String> {
    store::import_asset(&path)
}

#[tauri::command]
fn export_profile(app: State<App>, id: String, dest: String) -> Result<(), String> {
    let c = app.config.lock().unwrap();
    let p = c.profiles.iter().find(|p| p.id == id).ok_or("No such profile")?;
    store::export_profile(p, &dest)
}

#[tauri::command]
fn import_profile(app: State<App>, handle: AppHandle, path: String) -> Result<Profile, String> {
    let p = store::import_profile(&path, new_id())?;
    app.config.lock().unwrap().profiles.push(p.clone());
    refresh_tray(&handle);
    Ok(p)
}

#[tauri::command]
fn get_fonts() -> Vec<String> {
    let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
    render::font_catalog().into_iter().filter(|(_, f)| dir.join(f).exists()).map(|(n, _)| n.to_string()).collect()
}

#[tauri::command]
fn get_monitors() -> Vec<capture::MonitorInfo> {
    capture::monitors()
}

#[tauri::command]
fn foreground_app() -> Option<String> {
    rules::foreground_exe()
}

#[tauri::command]
async fn coexistence() -> Coexistence {
    // These shell out, so they stay off the UI thread.
    Coexistence {
        trcc_running: system::trcc_running(),
        trcc_autostart: system::trcc_autostart_enabled(),
        autostart: system::autostart_enabled(),
    }
}

#[tauri::command]
fn take_over(disable_autostart: bool) -> Result<(), String> {
    system::take_over(disable_autostart)
}

#[tauri::command]
fn restore_trcc_autostart() -> Result<(), String> {
    system::restore_trcc_autostart()
}

#[tauri::command]
fn set_autostart(enable: bool) -> Result<(), String> {
    system::set_autostart(enable)
}

#[tauri::command]
fn data_folder() -> String {
    store::root().to_string_lossy().into_owned()
}

fn open_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("index.html".into()))
        .title("Meniscus")
        .inner_size(1240.0, 800.0)
        .min_inner_size(1020.0, 660.0)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .center()
        .build();
    match built {
        Ok(w) => {
            let _ = w.set_effects(tauri::window::EffectsBuilder::new().effect(tauri::window::Effect::Acrylic).build());
            let handle = app.clone();
            w.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { api, .. } = e {
                    let to_tray = handle.state::<App>().config.lock().unwrap().settings.close_to_tray;
                    if to_tray {
                        // Destroying the window ends its WebView2 processes, so the
                        // background cost is only the engine.
                        api.prevent_close();
                        if let Some(w) = handle.get_webview_window(WINDOW) {
                            let _ = w.destroy();
                        }
                    } else {
                        handle.exit(0);
                    }
                }
            });
        }
        Err(e) => store::log(&format!("window: {e}")),
    }
}

fn build_tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let (profiles, active) = {
        let state = app.state::<App>();
        let c = state.config.lock().unwrap();
        (c.profiles.clone(), c.settings.active.clone())
    };
    let open = MenuItem::with_id(app, "open", "Open Meniscus", true, None::<&str>)?;
    let submenu = Submenu::with_id(app, "profiles", "Profile", true)?;
    for p in &profiles {
        let item = CheckMenuItem::with_id(app, format!("profile:{}", p.id), &p.name, true, p.id == active, None::<&str>)?;
        submenu.append(&item)?;
    }
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    Menu::with_items(app, &[&open, &submenu, &sep, &quit])
}

fn on_tray_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => open_window(app),
        "quit" => app.exit(0),
        id => {
            if let Some(pid) = id.strip_prefix("profile:") {
                let state = app.state::<App>();
                let mut c = state.config.lock().unwrap();
                c.settings.active = pid.to_string();
                c.manual_stamp += 1;
                if let Err(e) = store::save_settings(&c.settings) {
                    store::log(&format!("settings: {e}"));
                }
                drop(c);
                refresh_tray(app);
            }
        }
    }
}

/// Serves the newest panel frame to the preview. The UI shows the same bytes that
/// go down the wire, so the preview costs no extra rendering or encoding.
fn frame_response(slot: &FrameSlot) -> tauri::http::Response<Vec<u8>> {
    let frame = slot.latest();
    let (status, seq, body, mime) = match frame {
        Some(f) if f.jpeg => (200, f.seq, f.payload.clone(), "image/jpeg"),
        Some(f) => (200, f.seq, rgb565_bmp(&f), "image/bmp"),
        None => (404, 0, Vec::new(), "image/jpeg"),
    };
    tauri::http::Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Cache-Control", "no-store")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Expose-Headers", "X-Seq")
        .header("X-Seq", seq.to_string())
        .body(body)
        .unwrap()
}

/// Wraps raw panel pixels in a BMP header, which browsers decode without any conversion.
fn rgb565_bmp(f: &device::Frame) -> Vec<u8> {
    const HEADER: u32 = 14 + 40 + 12;
    let mut bmp = Vec::with_capacity(HEADER as usize + f.payload.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(HEADER + f.payload.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&HEADER.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(f.width as i32).to_le_bytes());
    // Negative height means top-down rows, matching the panel.
    bmp.extend_from_slice(&(-(f.height as i32)).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&16u16.to_le_bytes());
    bmp.extend_from_slice(&3u32.to_le_bytes());
    bmp.extend_from_slice(&(f.payload.len() as u32).to_le_bytes());
    bmp.extend_from_slice(&[0u8; 16]);
    for mask in [0xF800u32, 0x07E0, 0x001F] {
        bmp.extend_from_slice(&mask.to_le_bytes());
    }
    if f.big_endian {
        bmp.extend(f.payload.chunks_exact(2).flat_map(|p| [p[1], p[0]]));
    } else {
        bmp.extend_from_slice(&f.payload);
    }
    bmp
}

fn starter_profile() -> Profile {
    use model::Widget;
    let ring = |id: &str, x: f32, sensor: &str, label: &str, c1: &str, c2: &str| Widget {
        id: id.into(),
        kind: "ring".into(),
        x,
        y: 0.56,
        w: 0.44,
        h: 0.44,
        sensor: sensor.into(),
        label: label.into(),
        color: c1.into(),
        color2: c2.into(),
        size: 0.1,
        thickness: 0.035,
        warn: 75.0,
        crit: 88.0,
        ..Default::default()
    };
    Profile {
        id: new_id(),
        name: "Glass Dashboard".into(),
        widgets: vec![
            Widget {
                id: "clock".into(),
                kind: "clock".into(),
                x: 0.5,
                y: 0.17,
                size: 0.15,
                font: "Segoe UI Light".into(),
                ..Default::default()
            },
            ring("cpu", 0.27, "mahm:CPU temperature", "CPU", "#7cc7ff", "#c58bff"),
            ring("gpu", 0.73, "mahm:GPU1 temperature", "GPU", "#5ef2c2", "#7cc7ff"),
            Widget {
                id: "load".into(),
                kind: "bar".into(),
                x: 0.5,
                y: 0.89,
                w: 0.8,
                h: 0.018,
                size: 0.04,
                sensor: "sys:cpu_load".into(),
                label: "CPU load".into(),
                color: "#7cc7ff".into(),
                color2: "#ffffffcc".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

fn main() {
    store::init();
    store::log("starting");

    let mut settings = store::load_settings();
    let mut profiles = store::load_profiles();
    if profiles.is_empty() {
        let p = starter_profile();
        if let Err(e) = store::save_profile(&p) {
            store::log(&format!("starter profile: {e}"));
        }
        profiles.push(p);
    }
    if !profiles.iter().any(|p| p.id == settings.active) {
        settings.active = profiles[0].id.clone();
        let _ = store::save_settings(&settings);
    }
    let start_hidden = settings.start_hidden || std::env::args().any(|a| a == "--hidden");

    let slot = Arc::new(FrameSlot::default());
    let status: StatusCell = Arc::new(Mutex::new(DeviceStatus::default()));
    let panel = Arc::new(Mutex::new(device::panel_for(0, 0)));
    let sensors_cell: SharedSensors = Arc::new(RwLock::new(Snapshot::default()));
    let live: SharedLive = Arc::new(Mutex::new(Live { brightness_cap: 100, ..Default::default() }));
    let stats: SharedStats = Arc::new(Mutex::new(EngineStats::default()));
    let config: SharedConfig = Arc::new(Mutex::new(Config { settings, profiles, manual_stamp: 0 }));

    let music_cell: music::SharedMusic = Default::default();
    sensors::spawn(sensors_cell.clone());
    music::spawn(music_cell.clone(), live.clone());
    rules::spawn(config.clone(), live.clone(), sensors_cell.clone());
    render::spawn(render::EngineHandles {
        slot: slot.clone(),
        panel: panel.clone() as Arc<Mutex<Panel>>,
        sensors: sensors_cell.clone(),
        live: live.clone(),
        stats: stats.clone(),
        music: music_cell,
    });
    let (usb_slot, usb_status, exit_slot) = (slot.clone(), status.clone(), slot.clone());

    let preview_slot = slot.clone();
    let result = tauri::Builder::default()
        // Must stay first: a second copy would fight this one for the panel, so it
        // just asks this copy to show its window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--quit") {
                app.exit(0);
            } else {
                open_window(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("lcd", move |_ctx, _req| frame_response(&preview_slot))
        .manage(App {
            config,
            live,
            stats,
            sensors: sensors_cell,
            status,
            slot,
            usage: Mutex::new(SelfUsage::new()),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_sensors,
            get_profiles,
            get_settings,
            save_settings,
            save_profile,
            delete_profile,
            import_asset,
            export_profile,
            import_profile,
            get_fonts,
            get_monitors,
            foreground_app,
            coexistence,
            take_over,
            restore_trcc_autostart,
            set_autostart,
            data_folder,
        ])
        .setup(move |tauri_app| {
            // `--quit` with no copy running has nothing to stop.
            if std::env::args().any(|a| a == "--quit") {
                std::process::exit(0);
            }
            // Here, not earlier: a duplicate launch exits before setup and must never touch the panel.
            device::spawn(usb_slot, usb_status, panel);
            let handle = tauri_app.handle().clone();
            let menu = build_tray_menu(&handle)?;
            let mut tray = TrayIconBuilder::with_id("tray")
                .tooltip("Meniscus")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| on_tray_menu(app, e.id().as_ref()))
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        open_window(tray.app_handle());
                    }
                });
            if let Some(icon) = tauri_app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(tauri_app)?;
            if !start_hidden {
                open_window(&handle);
            }
            Ok(())
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(move |_, e| match e {
            // Closing the only window must not end the process: the engine keeps the panel alive.
            tauri::RunEvent::ExitRequested { api, code, .. } if code.is_none() => api.prevent_exit(),
            tauri::RunEvent::Exit => exit_slot.quiesce(),
            _ => {}
        }),
        Err(e) => store::log(&format!("fatal: {e}")),
    }
}
