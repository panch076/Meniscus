# Meniscus
# pure, vibecoded, claude-made, slop -- i made it for my use case, so if you need help, [here](https://claude.ai/new) is the link 
A fast, liquid-glass replacement for Thermalright's TRCC, for AIO and cooler LCDs on the USB bulk protocol (`87AD:70DB`).

## Build

Requires Rust (MSVC) and Node.

```
cd src-tauri
cargo test
cargo build --release              # target\release\meniscus.exe
npx @tauri-apps/cli@2 build        # installer in target\release\bundle\nsis
```

## Architecture

| Part | File | Notes |
|---|---|---|
| USB transport | `device.rs` | Handshake, frame send, 150 ms keepalive, reconnects after unplug |
| Render loop | `render.rs` | Composes and encodes only when something visible changed |
| Backgrounds | `media.rs`, `video.rs`, `capture.rs`, `audio.rs` | One thread each, stopped when unused |
| Sensors | `sensors.rs` | Afterburner and HWiNFO shared memory, plus Win32 |
| Automation | `rules.rs` | App and time rules, carousel, quiet hours, alerts, weather |
| Music | `music.rs`, `lyricview.rs` | Now playing, artwork, synced lyrics and their animation |
| Storage | `store.rs` | Atomic writes with a `.bak` fallback, in `%APPDATA%\Meniscus` |
| UI | `ui/` | Plain JS, no framework. The window is destroyed on close, ending WebView2 |

## Sensors

Windows has no user-mode CPU temperature API. Run **MSI Afterburner**, or **HWiNFO** with Shared Memory Support on, to get temperatures. Load, memory, network and disk work without either.

## Music

The Lyrical and Album Cover templates follow whatever appears in the Windows media flyout. While a profile on the panel uses a lyrics or album art widget, the song title and artist are sent to [LRCLIB](https://lrclib.net) for lyrics, and to the iTunes Search API when the player supplies no artwork. Set `MENISCUS_DEMO=1` to try the view with a built-in track and no network use.

## Scope

- Fan, pump and RGB control are absent: this USB interface only accepts frames.
- Brightness is applied to the image, so a black screen may still show backlight glow.
- Other Thermalright protocols (SCSI, HID, LY) are not implemented.

## Credits

Protocol facts come from the documentation of [thermalright-trcc-linux](https://github.com/Lexonight1/thermalright-trcc-linux) (GPL-3.0). This is an independent implementation, not affiliated with Thermalright. Review that project's licence before distributing Meniscus publicly.
