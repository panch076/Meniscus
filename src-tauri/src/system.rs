//! Coexistence with TRCC and Windows integration.
//!
//! Only one program can hold the panel. TRCC's USB helpers run elevated, so
//! stopping them takes a UAC prompt, and each step here happens only when the user
//! clicks it. Disabling TRCC's startup task is separate and fully reversible.

use std::os::windows::process::CommandExt;
use std::process::Command;
use windows::core::{w, HSTRING};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Meniscus";
const TRCC_TASK: &str = "TRCCAppStartup";

fn silent(cmd: &str, args: &[&str]) -> Option<std::process::Output> {
    Command::new(cmd).args(args).creation_flags(CREATE_NO_WINDOW).output().ok()
}

pub fn trcc_running() -> bool {
    silent("tasklist", &["/FO", "CSV", "/NH"])
        .map(|o| {
            let list = String::from_utf8_lossy(&o.stdout).to_lowercase();
            ["\"trcc.exe\"", "\"usblcd.exe\"", "\"usblcdnew.exe\""].iter().any(|n| list.contains(n))
        })
        .unwrap_or(false)
}

pub fn trcc_autostart_enabled() -> bool {
    silent("schtasks", &["/Query", "/TN", TRCC_TASK, "/FO", "LIST"])
        .filter(|o| o.status.success())
        .map(|o| !String::from_utf8_lossy(&o.stdout).contains("Disabled"))
        .unwrap_or(false)
}

fn elevated_cmd(script: &str) -> Result<(), String> {
    let args = HSTRING::from(format!("/c {script}"));
    let result = unsafe { ShellExecuteW(None, w!("runas"), w!("cmd.exe"), &args, None, SW_HIDE) };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err("Windows did not allow the change (the UAC prompt may have been declined).".into())
    }
}

/// Stops TRCC and, if asked, stops it starting with Windows.
pub fn take_over(disable_autostart: bool) -> Result<(), String> {
    let mut script = String::from("taskkill /F /IM TRCC.exe & taskkill /F /IM USBLCDNEW.exe & taskkill /F /IM USBLCD.exe & taskkill /F /IM HWINFO.exe");
    if disable_autostart {
        script.push_str(&format!(" & schtasks /Change /TN {TRCC_TASK} /DISABLE"));
    }
    elevated_cmd(&script)
}

pub fn restore_trcc_autostart() -> Result<(), String> {
    elevated_cmd(&format!("schtasks /Change /TN {TRCC_TASK} /ENABLE"))
}

pub fn autostart_enabled() -> bool {
    silent("reg", &["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false)
}

pub fn set_autostart(enable: bool) -> Result<(), String> {
    let output = if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let value = format!("\"{}\" --hidden", exe.display());
        silent("reg", &["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])
    } else {
        silent("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
    };
    match output {
        Some(o) if o.status.success() => Ok(()),
        _ => Err("Could not update the startup entry.".into()),
    }
}
