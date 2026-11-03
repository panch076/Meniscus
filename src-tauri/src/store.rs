//! Durable storage under %APPDATA%\Meniscus.
//!
//! Writes go to a temp file, are flushed, then renamed over the target, so a
//! crash or power cut can never leave a half-written profile. The previous good
//! copy is kept as `.bak` and used if the main file ever fails to parse.

use crate::model::{Profile, Settings};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn root() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Meniscus")
}
fn profiles_dir() -> PathBuf {
    root().join("profiles")
}
pub fn assets_dir() -> PathBuf {
    root().join("assets")
}
fn settings_path() -> PathBuf {
    root().join("settings.json")
}

pub fn init() {
    let _ = fs::create_dir_all(profiles_dir());
    let _ = fs::create_dir_all(assets_dir());
}

pub fn log(msg: &str) {
    let path = root().join("meniscus.log");
    if fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = fs::rename(&path, root().join("meniscus.old.log"));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%F %T"), msg);
    }
}

fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("tmp{}", SEQ.fetch_add(1, Ordering::Relaxed)));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(&json).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    if path.exists() {
        let _ = fs::copy(path, path.with_extension("bak"));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

fn read_with_backup<T: DeserializeOwned>(path: &Path) -> Option<T> {
    if let Ok(bytes) = fs::read(path) {
        match serde_json::from_slice(&bytes) {
            Ok(v) => return Some(v),
            Err(e) => log(&format!("corrupt {}: {e}; trying backup", path.display())),
        }
    }
    serde_json::from_slice(&fs::read(path.with_extension("bak")).ok()?).ok()
}

pub fn load_settings() -> Settings {
    read_with_backup(&settings_path()).unwrap_or_default()
}
pub fn save_settings(s: &Settings) -> Result<(), String> {
    write_atomic(&settings_path(), s)
}

/// Ids reach file names, so anything but a plain token is refused.
fn profile_path(id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("invalid profile id: {id}"));
    }
    Ok(profiles_dir().join(format!("{id}.json")))
}

pub fn load_profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    if let Ok(dir) = fs::read_dir(profiles_dir()) {
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("json") {
                if let Some(mut profile) = read_with_backup::<Profile>(&p) {
                    // Earlier defaults left visible artifacts around text.
                    if profile.quality == 88 || profile.quality == 95 {
                        profile.quality = 100;
                    }
                    out.push(profile);
                }
            }
        }
    }
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

pub fn save_profile(p: &Profile) -> Result<(), String> {
    write_atomic(&profile_path(&p.id)?, p)
}

pub fn delete_profile(id: &str) -> Result<(), String> {
    let path = profile_path(id)?;
    let _ = fs::remove_file(path.with_extension("bak"));
    fs::remove_file(path).map_err(|e| e.to_string())
}

/// Copies media into the app's asset folder under a content-derived name, so a
/// profile keeps working after the original file is moved or deleted.
pub fn import_asset(src: &str) -> Result<String, String> {
    let src = Path::new(src);
    let mut f = fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut h = DefaultHasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf[..n].hash(&mut h);
    }
    let ext = src.extension().and_then(|x| x.to_str()).unwrap_or("bin").to_lowercase();
    let dest = assets_dir().join(format!("{:016x}.{ext}", h.finish()));
    if !dest.exists() {
        fs::copy(src, &dest).map_err(|e| e.to_string())?;
    }
    Ok(dest.to_string_lossy().into_owned())
}

/// A shareable bundle: `profile.json` plus every asset the profile references.
pub fn export_profile(p: &Profile, dest: &str) -> Result<(), String> {
    let mut zip = zip::ZipWriter::new(fs::File::create(dest).map_err(|e| e.to_string())?);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut out = p.clone();
    let rename = |path: &mut String, zip: &mut zip::ZipWriter<fs::File>| -> Result<(), String> {
        let Some(name) = Path::new(path.as_str()).file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return Ok(());
        };
        if let Ok(bytes) = fs::read(path.as_str()) {
            let entry = format!("assets/{name}");
            if zip.start_file(entry.as_str(), stored).is_ok() {
                zip.write_all(&bytes).map_err(|e| e.to_string())?;
            }
            *path = entry;
        }
        Ok(())
    };
    rename(&mut out.bg.path, &mut zip)?;
    for w in &mut out.widgets {
        rename(&mut w.path, &mut zip)?;
    }
    zip.start_file("profile.json", stored).map_err(|e| e.to_string())?;
    zip.write_all(&serde_json::to_vec_pretty(&out).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn import_profile(src: &str, new_id: String) -> Result<Profile, String> {
    let file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut p: Profile = {
        let mut entry = zip.by_name("profile.json").map_err(|e| e.to_string())?;
        let mut json = String::new();
        entry.read_to_string(&mut json).map_err(|e| e.to_string())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())?
    };
    p.id = new_id;
    let mut restore = |path: &mut String| -> Result<(), String> {
        if !path.starts_with("assets/") {
            // Only bundled assets are honoured: a bundle must not point at arbitrary local files.
            path.clear();
            return Ok(());
        }
        // Take only the file name, so a crafted entry cannot escape the asset folder.
        let name = Path::new(path.as_str()).file_name().and_then(|n| n.to_str()).ok_or("bad asset name")?.to_string();
        let mut entry = zip.by_name(path).map_err(|e| e.to_string())?;
        let dest = assets_dir().join(name);
        let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        *path = dest.to_string_lossy().into_owned();
        Ok(())
    };
    if !p.bg.path.is_empty() {
        restore(&mut p.bg.path)?;
    }
    for w in &mut p.widgets {
        if !w.path.is_empty() {
            restore(&mut w.path)?;
        }
    }
    save_profile(&p)?;
    Ok(p)
}
