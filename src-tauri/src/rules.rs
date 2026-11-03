//! Decides what the panel should show right now: per-app and time-of-day rules,
//! the carousel, night dimming, idle blanking and temperature alerts. Also gathers
//! the slow external data (now playing, weather). Ticks twice a second.

use crate::model::{Profile, Settings};
use crate::render::SharedLive;
use crate::sensors::SharedSensors;
use chrono::{Local, Timelike};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

#[derive(Default)]
pub struct Config {
    pub settings: Settings,
    pub profiles: Vec<Profile>,
    /// Bumped by the UI when it applies a manual choice, so the carousel restarts from it.
    pub manual_stamp: u64,
}

pub type SharedConfig = Arc<Mutex<Config>>;

pub fn foreground_exe() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(|s| s.to_lowercase())
    }
}

fn minutes(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.split_once(':')?;
    Some(h.trim().parse::<u32>().ok()? * 60 + m.trim().parse::<u32>().ok()?)
}

/// True if `now` falls in the window, including windows that cross midnight.
fn in_window(now: u32, from: &str, to: &str) -> bool {
    let (Some(a), Some(b)) = (minutes(from), minutes(to)) else { return false };
    if a <= b {
        now >= a && now < b
    } else {
        now >= a || now < b
    }
}

fn idle_minutes() -> u32 {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        (GetTickCount64() as u32).wrapping_sub(info.dwTime) / 60_000
    }
}

fn choose(cfg: &Config, exe: Option<&str>, now_min: u32, carousel_index: usize) -> (String, String) {
    let s = &cfg.settings;
    let exists = |id: &str| cfg.profiles.iter().any(|p| p.id == id);
    for r in s.rules.iter().filter(|r| r.enabled && r.kind == "app" && exists(&r.profile)) {
        let want = r.exe.trim().to_lowercase();
        if !want.is_empty() && exe == Some(want.as_str()) {
            return (r.profile.clone(), format!("App rule: {want}"));
        }
    }
    for r in s.rules.iter().filter(|r| r.enabled && r.kind == "time" && exists(&r.profile)) {
        if in_window(now_min, &r.from, &r.to) {
            return (r.profile.clone(), format!("Schedule: {}-{}", r.from, r.to));
        }
    }
    if s.carousel {
        let ids: Vec<&String> = s.carousel_ids.iter().filter(|id| exists(id)).collect();
        if !ids.is_empty() {
            return (ids[carousel_index % ids.len()].clone(), "Carousel".into());
        }
    }
    (s.active.clone(), String::new())
}

pub fn spawn(config: SharedConfig, live: SharedLive, sensors: SharedSensors) {
    {
        let c = config.clone();
        let l = live.clone();
        std::thread::Builder::new()
            .name("rules".into())
            .spawn(move || rules_loop(c, l, sensors))
            .expect("spawn rules thread");
    }
    std::thread::Builder::new()
        .name("extras".into())
        .spawn(move || extras_loop(config, live))
        .expect("spawn extras thread");
}

fn rules_loop(config: SharedConfig, live: SharedLive, sensors: SharedSensors) {
    let mut carousel_index = 0usize;
    let mut carousel_at = Instant::now();
    let mut last_stamp = 0;
    loop {
        {
            let cfg = config.lock().unwrap();
            let s = &cfg.settings;
            if cfg.manual_stamp != last_stamp {
                last_stamp = cfg.manual_stamp;
                carousel_at = Instant::now();
            }
            if s.carousel && carousel_at.elapsed() >= Duration::from_secs(s.carousel_seconds.max(3) as u64) {
                carousel_index = carousel_index.wrapping_add(1);
                carousel_at = Instant::now();
            }
            let exe = foreground_exe();
            let now = Local::now();
            let now_min = now.hour() * 60 + now.minute();
            let (id, reason) = choose(&cfg, exe.as_deref(), now_min, carousel_index);
            let profile = cfg.profiles.iter().find(|p| p.id == id).or(cfg.profiles.first()).cloned();

            let mut cap = 100;
            if s.sleep_enabled && in_window(now_min, &s.sleep_from, &s.sleep_to) {
                cap = if s.sleep_action == "off" { 0 } else { s.sleep_dim.min(100) };
            }
            if s.idle_off_minutes > 0 && idle_minutes() >= s.idle_off_minutes {
                cap = 0;
            }
            let alert = if s.alert_enabled {
                let snap = sensors.read().unwrap();
                snap.get(&s.alert_sensor)
                    .filter(|r| r.value >= s.alert_threshold as f64)
                    .map(|r| format!("{} {:.0}{}", s.alert_text, r.value, r.unit))
            } else {
                None
            };
            // An overheating warning has to be visible even when the panel is blanked.
            if alert.is_some() {
                cap = 100;
            }
            let mut l = live.lock().unwrap();
            l.profile = profile;
            l.reason = reason;
            l.brightness_cap = cap;
            l.alert = alert;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn widgets_use(cfg: &Config, token: &str) -> bool {
    cfg.profiles.iter().any(|p| p.widgets.iter().any(|w| w.visible && w.text.contains(token)))
}

fn extras_loop(config: SharedConfig, live: SharedLive) {
    let mut weather_at: Option<Instant> = None;
    let mut weather_key = (0f32, 0f32, false);
    loop {
        let (want_weather, lat, lon, fahrenheit) = {
            let c = config.lock().unwrap();
            let s = &c.settings;
            (s.weather_enabled && widgets_use(&c, "{weather."), s.weather_lat, s.weather_lon, s.weather_fahrenheit)
        };
        let key = (lat, lon, fahrenheit);
        if want_weather && (weather_key != key || weather_at.map_or(true, |t| t.elapsed() > Duration::from_secs(900))) {
            weather_key = key;
            weather_at = Some(Instant::now());
            match fetch_weather(lat, lon, fahrenheit) {
                Ok((temp, desc)) => {
                    let mut l = live.lock().unwrap();
                    l.weather_temp = temp;
                    l.weather_desc = desc;
                }
                Err(e) => crate::store::log(&format!("weather: {e}")),
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn weather_desc(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 | 2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61..=67 | 80..=82 => "Rain",
        71..=77 | 85 | 86 => "Snow",
        95..=99 => "Thunderstorm",
        _ => "",
    }
}

fn fetch_weather(lat: f32, lon: f32, fahrenheit: bool) -> Result<(String, String), String> {
    let unit = if fahrenheit { "fahrenheit" } else { "celsius" };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code&temperature_unit={unit}"
    );
    let body: serde_json::Value = ureq::get(&url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let current = &body["current"];
    let temp = current["temperature_2m"].as_f64().ok_or("no temperature in reply")?;
    let code = current["weather_code"].as_i64().unwrap_or(-1);
    Ok((format!("{temp:.0}\u{b0}{}", if fahrenheit { "F" } else { "C" }), weather_desc(code).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    #[test]
    fn windows_cross_midnight() {
        assert!(in_window(23 * 60, "22:00", "07:00"));
        assert!(in_window(6 * 60, "22:00", "07:00"));
        assert!(!in_window(12 * 60, "22:00", "07:00"));
        assert!(in_window(12 * 60, "09:00", "17:00"));
        assert!(!in_window(17 * 60, "09:00", "17:00"));
        assert!(!in_window(0, "garbage", "07:00"));
    }

    #[test]
    fn app_rules_beat_schedules_and_carousel() {
        let profile = |id: &str| Profile { id: id.into(), ..Default::default() };
        let mut cfg = Config::default();
        cfg.profiles = vec![profile("a"), profile("b"), profile("c"), profile("d")];
        cfg.settings.active = "a".into();
        cfg.settings.carousel = true;
        cfg.settings.carousel_ids = vec!["d".into()];
        cfg.settings.rules = vec![
            Rule { kind: "time".into(), from: "00:00".into(), to: "12:00".into(), profile: "b".into(), ..Default::default() },
            Rule { kind: "app".into(), exe: "Game.exe".into(), profile: "c".into(), ..Default::default() },
        ];
        assert_eq!(choose(&cfg, Some("game.exe"), 60, 0).0, "c");
        assert_eq!(choose(&cfg, Some("other.exe"), 60, 0).0, "b");
        assert_eq!(choose(&cfg, None, 13 * 60, 0).0, "d");
        cfg.settings.carousel = false;
        assert_eq!(choose(&cfg, None, 13 * 60, 0).0, "a");
    }
}
