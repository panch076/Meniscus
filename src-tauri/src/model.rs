//! Persisted data shapes. Every field has a serde default so profiles written by
//! older versions keep loading after new fields are added.

use serde::{Deserialize, Serialize};

fn s(v: &str) -> String {
    v.to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Background {
    /// color | gradient | liquid | image | gif | video | slideshow | screen | visualizer | albumart
    pub kind: String,
    pub color: String,
    pub color2: String,
    pub color3: String,
    pub angle: f32,
    pub path: String,
    pub dir: String,
    /// cover | contain | stretch
    pub fit: String,
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub speed: f32,
    pub interval: f32,
    pub shuffle: bool,
    pub monitor: u32,
    /// bars | mirror | radial
    pub style: String,
    /// Darkens media so widgets stay legible (0..1).
    pub dim: f32,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            kind: s("liquid"),
            color: s("#0a1a3a"),
            color2: s("#1f6feb"),
            color3: s("#a855f7"),
            angle: 135.0,
            path: String::new(),
            dir: String::new(),
            fit: s("cover"),
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            speed: 1.0,
            interval: 10.0,
            shuffle: false,
            monitor: 0,
            style: s("bars"),
            dim: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Widget {
    pub id: String,
    /// text | clock | analog | date | sensor | ring | bar | spark | image | lyrics | albumart | track | progress
    pub kind: String,
    pub visible: bool,
    /// Centre of the widget as a fraction of the canvas, so layouts survive
    /// rotation and resolution changes.
    pub x: f32,
    pub y: f32,
    /// Width and height as a fraction of the canvas' shorter side.
    pub w: f32,
    pub h: f32,
    pub color: String,
    pub color2: String,
    pub track_color: String,
    pub font: String,
    /// Text size as a fraction of the canvas' shorter side.
    pub size: f32,
    pub text: String,
    pub label: String,
    pub format: String,
    pub sensor: String,
    pub min: f32,
    pub max: f32,
    pub decimals: u32,
    pub show_unit: bool,
    pub thickness: f32,
    /// Colour changes above these values. Disabled when warn >= crit is meaningless (both 0).
    pub warn: f32,
    pub crit: f32,
    pub warn_color: String,
    pub crit_color: String,
    pub opacity: f32,
    pub align: String,
    pub path: String,
    pub shadow: bool,
    pub history: u32,
    /// Seconds added to the playback position, for lyrics that run early or late.
    pub offset: f32,
}

impl Default for Widget {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: s("text"),
            visible: true,
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
            color: s("#ffffff"),
            color2: s("#7cc7ff"),
            track_color: s("#ffffff26"),
            font: s("Segoe UI Semibold"),
            size: 0.1,
            text: s("Text"),
            label: String::new(),
            format: String::new(),
            sensor: String::new(),
            min: 0.0,
            max: 100.0,
            decimals: 0,
            show_unit: true,
            thickness: 0.06,
            warn: 0.0,
            crit: 0.0,
            warn_color: s("#ffb020"),
            crit_color: s("#ff4d4f"),
            opacity: 1.0,
            align: s("center"),
            path: String::new(),
            shadow: true,
            history: 60,
            offset: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub bg: Background,
    pub widgets: Vec<Widget>,
    pub fps: u32,
    pub brightness: u32,
    pub rotation: u32,
    pub flip: bool,
    pub quality: u8,
    /// JPEG with full colour resolution (4:4:4), which keeps text edges clean.
    pub sharp_text: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: s("Profile"),
            bg: Background::default(),
            widgets: Vec::new(),
            fps: 30,
            brightness: 100,
            rotation: 0,
            flip: false,
            quality: 100,
            sharp_text: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Rule {
    /// app | time
    pub kind: String,
    pub enabled: bool,
    pub exe: String,
    pub from: String,
    pub to: String,
    pub profile: String,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            kind: s("app"),
            enabled: true,
            exe: String::new(),
            from: s("22:00"),
            to: s("07:00"),
            profile: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub active: String,
    pub rules: Vec<Rule>,
    pub carousel: bool,
    pub carousel_seconds: u32,
    pub carousel_ids: Vec<String>,
    pub alert_enabled: bool,
    pub alert_sensor: String,
    pub alert_threshold: f32,
    pub alert_text: String,
    pub sleep_enabled: bool,
    pub sleep_from: String,
    pub sleep_to: String,
    /// off | dim
    pub sleep_action: String,
    pub sleep_dim: u32,
    pub idle_off_minutes: u32,
    pub close_to_tray: bool,
    pub start_hidden: bool,
    pub weather_lat: f32,
    pub weather_lon: f32,
    pub weather_enabled: bool,
    pub weather_fahrenheit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            active: String::new(),
            rules: Vec::new(),
            carousel: false,
            carousel_seconds: 30,
            carousel_ids: Vec::new(),
            alert_enabled: false,
            alert_sensor: s("mahm:CPU temperature"),
            alert_threshold: 90.0,
            alert_text: s("CPU HOT"),
            sleep_enabled: false,
            sleep_from: s("23:00"),
            sleep_to: s("07:00"),
            sleep_action: s("dim"),
            sleep_dim: 20,
            idle_off_minutes: 0,
            close_to_tray: true,
            start_hidden: false,
            weather_lat: 0.0,
            weather_lon: 0.0,
            weather_enabled: false,
            weather_fahrenheit: false,
        }
    }
}
