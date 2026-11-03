//! What is playing, its artwork and its lyrics. Detection and lookup behave like the
//! Lyrical desktop app: Windows' media controls see every player in the media flyout,
//! lyrics come from LRCLIB and missing artwork from the iTunes Search API.
//!
//! Nothing here runs, and no track details leave the machine, unless the profile on
//! the panel uses a music widget.

use crate::model::Profile;
use crate::render::SharedLive;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time: f64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Hash)]
pub enum LyricState {
    #[default]
    Idle,
    Loading,
    Synced,
    Plain,
    Instrumental,
    Unavailable,
}

/// Square artwork, RGBA.
pub struct Art {
    pub seq: u64,
    pub side: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct Music {
    pub present: bool,
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub duration: Option<f64>,
    pub position: f64,
    pub timestamp_ms: f64,
    pub playing: bool,
    pub art: Option<Arc<Art>>,
    pub state: LyricState,
    pub lines: Arc<Vec<Line>>,
    pub plain: Arc<Vec<String>>,
}

impl Music {
    /// Current playback position: the last report plus the time since it was made.
    pub fn time(&self) -> f64 {
        let t = if self.playing { self.position + (now_ms() - self.timestamp_ms).max(0.0) / 1000.0 } else { self.position };
        self.duration.map_or(t, |d| t.min(d))
    }
}

pub type SharedMusic = Arc<RwLock<Music>>;

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Needs {
    pub media: bool,
    pub lyrics: bool,
    pub art: bool,
}

pub fn needs(p: &Profile) -> Needs {
    let mut n = Needs { art: p.bg.kind == "albumart", ..Default::default() };
    for w in p.widgets.iter().filter(|w| w.visible) {
        match w.kind.as_str() {
            "lyrics" => n.lyrics = true,
            "albumart" => n.art = true,
            "track" | "progress" => n.media = true,
            _ => n.media |= w.text.contains("{media."),
        }
    }
    n.media |= n.lyrics || n.art;
    n
}

pub fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

#[derive(Clone, PartialEq)]
struct Snapshot {
    track_id: String,
    title: String,
    artist: String,
    album: String,
    duration: Option<f64>,
    position: f64,
    timestamp_ms: f64,
    playing: bool,
}

/// 100 ns ticks between the Windows epoch (1601) and the Unix epoch (1970).
const EPOCH_DIFFERENCE_TICKS: i64 = 116_444_736_000_000_000;
/// Players briefly refuse property reads while switching tracks.
const MAX_READ_FAILURES: u32 = 4;

#[derive(Default)]
struct Backend {
    manager: Option<Manager>,
    session: Option<Session>,
    last: Option<Snapshot>,
    failures: u32,
}

impl Backend {
    /// Prefers a session that is actively playing, then the one Windows considers current.
    fn pick_session(&mut self) -> Option<Session> {
        if self.manager.is_none() {
            self.manager = Manager::RequestAsync().ok()?.get().ok();
        }
        let manager = self.manager.as_ref()?;
        if let Ok(sessions) = manager.GetSessions() {
            for session in sessions {
                let playing = session.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|s| s == Status::Playing);
                if playing {
                    return Some(session);
                }
            }
        }
        manager.GetCurrentSession().ok()
    }

    fn read(session: &Session) -> windows::core::Result<Option<Snapshot>> {
        let props = session.TryGetMediaPropertiesAsync()?.get()?;
        let title = props.Title()?.to_string();
        if title.trim().is_empty() {
            return Ok(None);
        }
        let artist = props.Artist()?.to_string();
        let album = props.AlbumTitle()?.to_string();
        let source = session.SourceAppUserModelId()?.to_string();
        let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;

        let timeline = session.GetTimelineProperties()?;
        let start = timeline.StartTime()?.Duration as f64 / 1e7;
        let end = timeline.EndTime()?.Duration as f64 / 1e7;
        let position = (timeline.Position()?.Duration as f64 / 1e7 - start).max(0.0);
        let updated = timeline.LastUpdatedTime()?.UniversalTime;
        let timestamp_ms = if updated > EPOCH_DIFFERENCE_TICKS { (updated - EPOCH_DIFFERENCE_TICKS) as f64 / 10_000.0 } else { now_ms() };
        Ok(Some(Snapshot {
            track_id: format!("{source}|{title}|{artist}"),
            title,
            artist,
            album,
            duration: (end - start > 1.0).then_some(end - start),
            position,
            timestamp_ms,
            playing,
        }))
    }

    fn snapshot(&mut self) -> Option<Snapshot> {
        let Some(session) = self.pick_session() else {
            self.last = None;
            return None;
        };
        let snapshot = match Self::read(&session) {
            Ok(s) => {
                self.failures = 0;
                s
            }
            Err(_) if self.failures < MAX_READ_FAILURES => {
                self.failures += 1;
                self.last.clone()
            }
            Err(_) => None,
        };
        self.session = Some(session);
        self.last = snapshot.clone();
        snapshot
    }

    fn thumbnail(&self) -> Option<Vec<u8>> {
        let read = || -> windows::core::Result<Vec<u8>> {
            let props = self.session.as_ref().ok_or_else(windows::core::Error::empty)?.TryGetMediaPropertiesAsync()?.get()?;
            let stream = props.Thumbnail()?.OpenReadAsync()?.get()?;
            let size = stream.Size()? as u32;
            let reader = DataReader::CreateDataReader(&stream)?;
            reader.LoadAsync(size)?.get()?;
            let mut bytes = vec![0u8; size as usize];
            reader.ReadBytes(&mut bytes)?;
            Ok(bytes)
        };
        read().ok().filter(|b| b.len() > 64)
    }
}

/// Polls after a track change during which artwork is re-read every time. Players
/// publish a new title first and its artwork later, so early reads may be stale.
const ART_FAST_POLLS: u32 = 30;
const ART_SLOW_EVERY: u32 = 10;
/// Polls a new track may go without artwork before falling back to a lookup.
const ART_SETTLE_POLLS: u32 = 4;

#[derive(Default)]
struct ArtWatcher {
    polls: u32,
    fingerprint: Option<u64>,
    published: bool,
}

enum ArtUpdate {
    Keep,
    Use(Vec<u8>),
    Lookup,
}

impl ArtWatcher {
    fn track_changed(&mut self) {
        self.polls = 0;
        self.published = false;
    }
    fn due(&mut self) -> bool {
        let due = self.polls < ART_FAST_POLLS || self.polls % ART_SLOW_EVERY == 0;
        self.polls = self.polls.wrapping_add(1);
        due
    }
    fn update(&mut self, art: Option<Vec<u8>>) -> ArtUpdate {
        match art {
            Some(bytes) => {
                let mut h = DefaultHasher::new();
                bytes.hash(&mut h);
                let fingerprint = h.finish();
                if self.published && self.fingerprint == Some(fingerprint) {
                    return ArtUpdate::Keep;
                }
                self.fingerprint = Some(fingerprint);
                self.published = true;
                ArtUpdate::Use(bytes)
            }
            None if !self.published && self.polls > ART_SETTLE_POLLS => {
                self.fingerprint = None;
                self.published = true;
                ArtUpdate::Lookup
            }
            None => ArtUpdate::Keep,
        }
    }
}

static ART_SEQ: AtomicU64 = AtomicU64::new(1);

fn decode_art(bytes: &[u8]) -> Option<Arc<Art>> {
    const MAX_SIDE: u32 = 640;
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    // Browser sessions report widescreen video thumbnails, so crop to the centre square.
    let side = w.min(h);
    let square = image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
    let square = if side > MAX_SIDE {
        image::imageops::resize(&square, MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        square
    };
    let side = square.width();
    Some(Arc::new(Art { seq: ART_SEQ.fetch_add(1, Ordering::Relaxed), side, rgba: square.into_raw() }))
}

pub fn spawn(shared: SharedMusic, live: SharedLive) {
    std::thread::Builder::new().name("music".into()).spawn(move || run(shared, live)).expect("spawn music thread");
}

fn run(shared: SharedMusic, live: SharedLive) {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let demo = std::env::var("MENISCUS_DEMO").is_ok_and(|v| v == "1");
    let mut backend = Backend::default();
    let mut watcher = ArtWatcher::default();
    let mut last_track = String::new();
    let mut lyrics_for = String::new();
    // Bumped on every track change: a slower lookup for an old track then discards itself.
    let generation = Arc::new(AtomicU64::new(0));
    let demo_start = now_ms();

    loop {
        let want = live.lock().unwrap().profile.as_ref().map(needs).unwrap_or_default();
        if !want.media {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        let snap = if demo { Some(demo_snapshot(demo_start)) } else { backend.snapshot() };
        let Some(s) = snap else {
            if !last_track.is_empty() {
                generation.fetch_add(1, Ordering::SeqCst);
                last_track.clear();
                lyrics_for.clear();
                *shared.write().unwrap() = Music::default();
                let mut l = live.lock().unwrap();
                l.media_title.clear();
                l.media_artist.clear();
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };

        if s.track_id != last_track {
            last_track = s.track_id.clone();
            lyrics_for.clear();
            generation.fetch_add(1, Ordering::SeqCst);
            watcher.track_changed();
            let mut m = shared.write().unwrap();
            // The previous artwork stays up until this track's arrives.
            m.state = LyricState::Idle;
            m.lines = Arc::default();
            m.plain = Arc::default();
            let mut l = live.lock().unwrap();
            l.media_title = s.title.clone();
            l.media_artist = s.artist.clone();
        }
        {
            let mut m = shared.write().unwrap();
            m.present = true;
            m.track_id = s.track_id.clone();
            m.title = s.title.clone();
            m.artist = s.artist.clone();
            m.duration = s.duration;
            m.position = s.position;
            m.timestamp_ms = s.timestamp_ms;
            m.playing = s.playing;
        }

        if want.lyrics && lyrics_for != s.track_id {
            lyrics_for = s.track_id.clone();
            shared.write().unwrap().state = LyricState::Loading;
            if demo {
                let mut m = shared.write().unwrap();
                m.state = LyricState::Synced;
                m.lines = Arc::new(demo_lines());
            } else {
                spawn_lyrics(s.clone(), shared.clone(), generation.clone());
            }
        }

        if want.art && watcher.due() {
            let bytes = if demo { Some(demo_art()) } else { backend.thumbnail() };
            match watcher.update(bytes) {
                ArtUpdate::Keep => {}
                ArtUpdate::Use(bytes) => {
                    if let Some(art) = decode_art(&bytes) {
                        shared.write().unwrap().art = Some(art);
                    }
                }
                ArtUpdate::Lookup => spawn_art_lookup(s.clone(), shared.clone(), generation.clone()),
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ---------- network ----------

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(12)))
        .user_agent("Meniscus/1.0 (AIO screen lyrics)")
        .build()
        .into()
});

/// `Ok(None)` is a definite answer that nothing exists. Errors, which include LRCLIB
/// being overloaded or rate limiting, are worth retrying.
fn get_text(url: &str, query: &[(&str, &str)]) -> Result<Option<String>, ()> {
    let mut response = AGENT.get(url).query_pairs(query.iter().copied()).call().map_err(|_| ())?;
    let status = response.status().as_u16();
    if (400..500).contains(&status) && status != 429 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(());
    }
    response.body_mut().read_to_string().map(Some).map_err(|_| ())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Record {
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

impl Record {
    fn has_synced(&self) -> bool {
        self.synced_lyrics.as_deref().is_some_and(|s| !s.is_empty())
    }
    fn has_plain(&self) -> bool {
        self.plain_lyrics.as_deref().is_some_and(|s| !s.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Found {
    state: LyricState,
    lines: Vec<Line>,
    plain: Vec<String>,
}

const LRCLIB: &str = "https://lrclib.net/api";
/// LRCLIB answers "503 ServerOverloaded" when busy, sometimes for over a minute.
const RETRY_SECONDS: [u64; 8] = [2, 4, 8, 15, 30, 30, 30, 30];

static LYRIC_CACHE: LazyLock<Mutex<HashMap<String, Found>>> = LazyLock::new(Default::default);

fn spawn_lyrics(track: Snapshot, shared: SharedMusic, generation: Arc<AtomicU64>) {
    let mine = generation.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("lyrics".into()).spawn(move || {
        let wanted = || generation.load(Ordering::SeqCst) == mine;
        let found = fetch_lyrics(&track, &wanted);
        if wanted() {
            let mut m = shared.write().unwrap();
            m.state = found.state;
            m.lines = Arc::new(found.lines);
            m.plain = Arc::new(found.plain);
        }
    });
}

fn fetch_lyrics(track: &Snapshot, wanted: &dyn Fn() -> bool) -> Found {
    let key = format!("{}|{}", track.artist, track.title).to_lowercase();
    if let Some(hit) = LYRIC_CACHE.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let (mut records, mut failed) = lookup(track);
    for delay in RETRY_SECONDS {
        if !failed || records.iter().any(Record::has_synced) || !wanted() {
            break;
        }
        std::thread::sleep(Duration::from_secs(delay));
        if !wanted() {
            break;
        }
        (records, failed) = lookup(track);
    }
    let result = choose(&records, track.duration);
    if failed {
        crate::store::log(&format!("lyrics: lookup failed for {}", track.title));
    }
    // A failed lookup may have missed better lyrics, so it is retried next time.
    if !failed || records.iter().any(Record::has_synced) {
        LYRIC_CACHE.lock().unwrap().insert(key, result.clone());
    }
    result
}

fn lookup(track: &Snapshot) -> (Vec<Record>, bool) {
    let mut failed = false;
    let mut records = Vec::new();
    for (title, artist) in queries(&track.title, &track.artist) {
        if let Some(duration) = track.duration {
            let seconds = duration.round().to_string();
            let query = [("track_name", title.as_str()), ("artist_name", artist.as_str()), ("album_name", track.album.as_str()), ("duration", seconds.as_str())];
            match get_text(&format!("{LRCLIB}/get"), &query) {
                Ok(Some(body)) => records.extend(serde_json::from_str::<Record>(&body).ok()),
                Ok(None) => {}
                Err(()) => failed = true,
            }
        }
        if !records.iter().any(Record::has_synced) {
            let query = [("track_name", title.as_str()), ("artist_name", artist.as_str())];
            match get_text(&format!("{LRCLIB}/search"), &query) {
                Ok(Some(body)) => records.extend(serde_json::from_str::<Vec<Record>>(&body).unwrap_or_default()),
                Ok(None) => {}
                Err(()) => failed = true,
            }
        }
        if records.iter().any(Record::has_synced) {
            break;
        }
    }
    (records, failed)
}

/// The (title, artist) pairs to try, best first.
fn queries(title: &str, artist: &str) -> Vec<(String, String)> {
    let mut out = vec![(title.to_string(), artist.to_string())];
    let cleaned = (clean_title(title), primary_artist(artist));
    if !out.contains(&cleaned) {
        out.push(cleaned);
    }
    // Browser videos report "Artist - Song" as the title and the channel as the artist.
    if let Some((a, t)) = split_video_title(title) {
        if !out.contains(&(t.clone(), a.clone())) {
            out.push((t, a));
        }
    }
    out
}

fn choose(records: &[Record], duration: Option<f64>) -> Found {
    let distance = |r: &Record| match (duration, r.duration) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => 0.0,
    };
    let mut synced: Vec<&Record> = records.iter().filter(|r| r.has_synced() && distance(r) < 15.0).collect();
    synced.sort_by(|a, b| distance(a).total_cmp(&distance(b)));

    if let Some(lines) = synced.first().and_then(|r| parse_lrc(r.synced_lyrics.as_deref().unwrap_or(""))) {
        return Found { state: LyricState::Synced, lines, plain: Vec::new() };
    }
    if records.first().is_some_and(|r| r.instrumental == Some(true)) {
        return Found { state: LyricState::Instrumental, lines: Vec::new(), plain: Vec::new() };
    }
    if let Some(r) = records.iter().find(|r| r.has_plain()) {
        let plain = r.plain_lyrics.as_deref().unwrap_or("").lines().map(str::to_string).collect();
        return Found { state: LyricState::Plain, lines: Vec::new(), plain };
    }
    Found { state: LyricState::Unavailable, lines: Vec::new(), plain: Vec::new() }
}

static TAG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\[(\d+):(\d+(?:[.:]\d+)?)\]").unwrap());
static WORD_TAG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"<\d+:\d+(?:\.\d+)?>").unwrap());

fn parse_lrc(lrc: &str) -> Option<Vec<Line>> {
    let mut entries: Vec<(f64, String)> = Vec::new();
    for raw in lrc.lines() {
        let tags: Vec<_> = TAG.captures_iter(raw).collect();
        let Some(last) = tags.last() else { continue };
        let start = last.get(0).unwrap().end();
        let text = WORD_TAG.replace_all(&raw[start..], "").trim().to_string();
        for tag in &tags {
            let minutes: f64 = tag[1].parse().unwrap_or(0.0);
            let seconds: f64 = tag[2].replace(':', ".").parse().unwrap_or(0.0);
            entries.push((minutes * 60.0 + seconds, text.clone()));
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Consecutive gaps collapse so an instrumental break shows once.
    let mut lines: Vec<Line> = Vec::new();
    for (time, text) in entries {
        if text.is_empty() && lines.last().is_some_and(|l| l.text.is_empty()) {
            continue;
        }
        lines.push(Line { time, text });
    }
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    Some(lines)
}

static CLEAN_PARENS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\s*[\(\[][^\)\]]*(feat|ft\.|with|remaster|version|edit|mix|live|mono|stereo|deluxe|bonus|explicit|official|video|audio|lyrics|visualizer)[^\)\]]*[\)\]]").unwrap()
});
static CLEAN_SUFFIX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\s+-\s+.*(remaster|version|edit|mix|live|mono|stereo|official|video|audio).*$").unwrap());

fn clean_title(title: &str) -> String {
    let result = CLEAN_PARENS.replace_all(title, "");
    CLEAN_SUFFIX.replace_all(&result, "").trim().to_string()
}

fn split_video_title(title: &str) -> Option<(String, String)> {
    let (artist, song) = title.split_once(" - ").or_else(|| title.split_once(" \u{2013} "))?;
    let (artist, song) = (primary_artist(artist), clean_title(song));
    (!artist.is_empty() && !song.is_empty()).then_some((artist, song))
}

fn primary_artist(artist: &str) -> String {
    let artist = artist.trim();
    let artist = artist.strip_suffix(" - Topic").unwrap_or(artist);
    let artist = artist.strip_suffix("VEVO").unwrap_or(artist);
    let lower = artist.to_lowercase();
    let cut = [", ", " & ", " feat. ", " feat ", " ft. ", " x ", " and "]
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim().to_string()
}

fn spawn_art_lookup(track: Snapshot, shared: SharedMusic, generation: Arc<AtomicU64>) {
    let mine = generation.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("art".into()).spawn(move || {
        let art = itunes_art(&track.title, &track.artist).and_then(|b| decode_art(&b));
        if generation.load(Ordering::SeqCst) == mine {
            if let Some(art) = art {
                shared.write().unwrap().art = Some(art);
            }
        }
    });
}

fn itunes_art(title: &str, artist: &str) -> Option<Vec<u8>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        artwork_url100: Option<String>,
    }
    #[derive(Deserialize)]
    struct Reply {
        results: Vec<Item>,
    }
    let term = format!("{artist} {title}");
    let body = get_text("https://itunes.apple.com/search", &[("term", term.as_str()), ("entity", "song"), ("limit", "1")]).ok()??;
    let reply: Reply = serde_json::from_str(&body).ok()?;
    let url = reply.results.into_iter().find_map(|i| i.artwork_url100)?.replace("100x100bb", "600x600bb");
    let mut response = AGENT.get(&url).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response.body_mut().with_config().limit(16 * 1024 * 1024).read_to_vec().ok()
}

// ---------- demo mode: exercises the view with no player running ----------

const DEMO_SECONDS: f64 = 60.0;

fn demo_snapshot(start: f64) -> Snapshot {
    Snapshot {
        track_id: "demo".into(),
        title: "Glass Water (Meniscus Demo Track)".into(),
        artist: "The Placeholder Band".into(),
        album: String::new(),
        duration: Some(DEMO_SECONDS),
        position: ((now_ms() - start) / 1000.0) % DEMO_SECONDS,
        timestamp_ms: now_ms(),
        playing: true,
    }
}

fn demo_lines() -> Vec<Line> {
    let lyric = [
        "Light bends where the water meets the air",
        "A curve you only notice when you stare",
        "Numbers rising slowly on the glass",
        "",
        "Every line lights up and then will pass",
        "Keep it cool, keep it running through the night",
        "한국어 and 日本語 should render right",
        "Fans are humming underneath the song",
        "This whole demo only runs ten lines long",
        "So it loops around and starts again",
    ];
    let mut out: Vec<Line> = lyric.iter().enumerate().map(|(i, t)| Line { time: 4.0 + i as f64 * 5.0, text: (*t).to_string() }).collect();
    out.push(Line { time: 54.0, text: String::new() });
    out
}

fn demo_art() -> Vec<u8> {
    let img = image::RgbImage::from_fn(300, 300, |x, y| {
        let (u, v) = (x as f32 / 300.0, y as f32 / 300.0);
        let d = ((u - 0.5).powi(2) + (v - 0.45).powi(2)).sqrt();
        let ring = if (d - 0.28).abs() < 0.02 { 110.0 } else { 0.0 };
        image::Rgb([(240.0 * (1.0 - v) + ring).min(255.0) as u8, (70.0 + 90.0 * u + ring).min(255.0) as u8, (150.0 * v + 90.0 + ring).min(255.0) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_and_collapses_gaps() {
        let lines = parse_lrc("[00:01.00]first\n[00:02.50]\n[00:03.00]\n[00:04.25]second\n[00:09.00]").unwrap();
        let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["first", "", "second"]);
        assert!((lines[2].time - 4.25).abs() < 1e-9);
        assert_eq!(parse_lrc("[00:10.00][00:01.00]repeat").unwrap().len(), 2);
        assert!(parse_lrc("no timing here").is_none());
    }

    #[test]
    fn cleans_titles_and_artists() {
        assert_eq!(clean_title("Song (Official Video)"), "Song");
        assert_eq!(clean_title("Song - Remastered 2011"), "Song");
        assert_eq!(clean_title("Song (feat. Someone)"), "Song");
        assert_eq!(primary_artist("A & B"), "A");
        assert_eq!(primary_artist("Adele - Topic"), "Adele");
        assert_eq!(primary_artist("AdeleVEVO"), "Adele");
        assert_eq!(split_video_title("Adele - Hello (Official Music Video)"), Some(("Adele".into(), "Hello".into())));
        assert_eq!(split_video_title("Hello"), None);
    }

    #[test]
    fn queries_skip_duplicates() {
        assert_eq!(queries("Song", "Artist").len(), 1);
        assert_eq!(queries("Song (Live)", "A & B"), [("Song (Live)".to_string(), "A & B".to_string()), ("Song".to_string(), "A".to_string())]);
    }

    fn record(duration: f64, synced: &str, plain: &str) -> Record {
        Record {
            duration: Some(duration),
            instrumental: None,
            plain_lyrics: Some(plain.into()),
            synced_lyrics: Some(synced.into()),
        }
    }

    #[test]
    fn reads_lrclib_json() {
        let body = r#"{"id":1,"trackName":"T","artistName":"A","albumName":"B","duration":145.0,"instrumental":false,
            "plainLyrics":"one\ntwo","syncedLyrics":"[00:01.50] one\n[00:03.00] two"}"#;
        let found = choose(&[serde_json::from_str::<Record>(body).unwrap()], Some(145.0));
        assert_eq!(found.state, LyricState::Synced);
        assert_eq!(found.lines, [Line { time: 1.5, text: "one".into() }, Line { time: 3.0, text: "two".into() }]);
        // Not-yet-synced records carry nulls.
        let nulls = r#"[{"duration":145.0,"instrumental":false,"plainLyrics":"one","syncedLyrics":null}]"#;
        let found = choose(&serde_json::from_str::<Vec<Record>>(nulls).unwrap(), Some(145.0));
        assert_eq!(found.state, LyricState::Plain);
    }

    #[test]
    fn prefers_the_synced_lyrics_closest_in_length() {
        let found = choose(&[record(110.0, "[00:01.00]far", ""), record(101.0, "[00:01.00]near", "")], Some(100.0));
        assert_eq!(found.state, LyricState::Synced);
        assert_eq!(found.lines[0].text, "near");
    }

    #[test]
    fn falls_back_when_timing_is_wrong_or_missing() {
        let found = choose(&[record(300.0, "[00:01.00]wrong song", "words")], Some(100.0));
        assert_eq!(found.state, LyricState::Plain);
        assert_eq!(choose(&[], Some(100.0)).state, LyricState::Unavailable);
        let inst = Record { instrumental: Some(true), ..Default::default() };
        assert_eq!(choose(&[inst], Some(100.0)).state, LyricState::Instrumental);
    }

    #[test]
    fn artwork_arriving_after_the_title_replaces_the_stale_one() {
        let mut w = ArtWatcher::default();
        w.track_changed();
        assert!(w.due());
        assert!(matches!(w.update(Some(vec![1; 100])), ArtUpdate::Use(_)));
        w.track_changed();
        assert!(w.due());
        assert!(matches!(w.update(Some(vec![1; 100])), ArtUpdate::Use(_)));
        assert!(w.due());
        assert!(matches!(w.update(Some(vec![1; 100])), ArtUpdate::Keep));
        assert!(w.due());
        assert!(matches!(w.update(Some(vec![2; 100])), ArtUpdate::Use(_)));
    }

    #[test]
    fn missing_artwork_waits_before_looking_it_up() {
        let mut w = ArtWatcher::default();
        w.track_changed();
        for _ in 0..ART_SETTLE_POLLS {
            assert!(w.due());
            assert!(matches!(w.update(None), ArtUpdate::Keep));
        }
        assert!(w.due());
        assert!(matches!(w.update(None), ArtUpdate::Lookup));
        assert!(w.due());
        assert!(matches!(w.update(None), ArtUpdate::Keep));
    }

    #[test]
    fn position_only_advances_while_playing() {
        let mut m = Music { position: 10.0, timestamp_ms: now_ms() - 5000.0, playing: false, ..Default::default() };
        assert_eq!(m.time(), 10.0);
        m.playing = true;
        assert!((m.time() - 15.0).abs() < 0.5);
        m.duration = Some(12.0);
        assert_eq!(m.time(), 12.0);
    }

    /// Hits the real services: `cargo test live_ -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_lookups_resolve() {
        let track = Snapshot {
            track_id: "t".into(),
            title: "Figaro".into(),
            artist: "Madvillain".into(),
            album: String::new(),
            duration: Some(145.0),
            position: 0.0,
            timestamp_ms: 0.0,
            playing: true,
        };
        let found = fetch_lyrics(&track, &|| true);
        println!("lyrics: {:?}, {} lines", found.state, found.lines.len());
        assert_eq!(found.state, LyricState::Synced);
    }

    #[test]
    #[ignore]
    fn live_artwork_resolves() {
        let art = itunes_art("Figaro", "Madvillain").and_then(|b| decode_art(&b)).expect("artwork");
        println!("art: {}x{}", art.side, art.side);
    }
}
