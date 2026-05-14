//! Speech data model, TOML parser, and 4-layer merge loader.
//!
//! Layer order (later layers append entries and override `speech_display` fields):
//! 1. Builtin common  (`assets/common/speech.toml`,          embedded via `include_bytes!`)
//! 2. Builtin char    (`assets/{char}/speech.toml`,          embedded via `include_bytes!`)
//! 3. User char       (`{AppSupport}/{char}/speech.toml`,    optional file)
//! 4. User common     (`{AppSupport}/common/speech.toml`,    optional file)

// ---- Embedded builtin speech data ----

const BUILTIN_COMMON: &[u8] = include_bytes!("../assets/common/speech.toml");
const BUILTIN_BD:     &[u8] = include_bytes!("../assets/bearded_dragon/speech.toml");
const BUILTIN_PT:     &[u8] = include_bytes!("../assets/pond_turtle/speech.toml");

// ---- Data model ----

/// Which condition fires this speech entry.
#[derive(serde::Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Random,
    Time,
    Weather,
    State,
}

/// A single speech entry from `speech.toml`.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct SpeechEntry {
    pub text_ja: Option<String>,
    pub text_en: Option<String>,
    pub trigger: TriggerKind,

    // --- random ---
    #[serde(default = "default_weight")]
    pub weight: f64,

    // --- time (all fields are AND conditions) ---
    pub hours:    Option<Vec<u8>>,
    pub weekdays: Option<Vec<String>>,
    pub months:   Option<Vec<u8>>,
    pub seasons:  Option<Vec<String>>,

    // --- weather ---
    pub weather:  Option<Vec<String>>,
    pub temp_max: Option<f64>,
    pub temp_min: Option<f64>,

    // --- state ---
    pub state: Option<String>,

    // --- per-entry display override ---
    pub duration_sec: Option<f64>,

    // --- future: oneshot animation ---
    pub oneshot:      Option<String>,
    pub oneshot_sync: Option<String>,
}

fn default_weight() -> f64 { 1.0 }

/// Effective bubble display parameters after merging all layers.
#[derive(Clone, Debug)]
pub struct SpeechDisplay {
    pub duration_sec: f64,
    pub fade_out_sec: f64,
}

impl Default for SpeechDisplay {
    fn default() -> Self {
        Self { duration_sec: 6.0, fade_out_sec: 1.0 }
    }
}

/// Merged speech data for one character, ready for the trigger engine.
pub struct SpeechData {
    pub display: SpeechDisplay,
    pub entries: Vec<SpeechEntry>,
}

// ---- Internal TOML file structure ----

#[derive(serde::Deserialize, Default, Clone)]
struct SpeechDisplayPartial {
    duration_sec: Option<f64>,
    fade_out_sec: Option<f64>,
}

/// Raw top-level structure of a single `speech.toml` file.
#[derive(serde::Deserialize, Default)]
struct SpeechFile {
    speech_display: Option<SpeechDisplayPartial>,
    /// Deserialized from `[[speech]]` array-of-tables.
    #[serde(default, rename = "speech")]
    entries: Vec<SpeechEntry>,
}

// ---- Parsing helpers ----

fn parse_bytes(bytes: &[u8]) -> Option<SpeechFile> {
    let text = std::str::from_utf8(bytes).ok()?;
    toml::from_str(text).ok()
}

fn parse_path(path: &std::path::Path) -> Option<SpeechFile> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn apply_partial(base: &mut SpeechDisplay, partial: &SpeechDisplayPartial) {
    if let Some(v) = partial.duration_sec { base.duration_sec = v; }
    if let Some(v) = partial.fade_out_sec { base.fade_out_sec = v; }
}

fn merge_layer(display: &mut SpeechDisplay, entries: &mut Vec<SpeechEntry>, file: SpeechFile) {
    if let Some(d) = &file.speech_display {
        apply_partial(display, d);
    }
    entries.extend(file.entries);
}

// ---- Public API ----

/// Load and merge all 4 layers for `char_name` (e.g. `"bearded_dragon"`).
///
/// Always returns a valid `SpeechData`; missing or malformed files are silently skipped.
pub fn load(char_name: &str) -> SpeechData {
    let builtin_char: &[u8] = match char_name {
        "bearded_dragon" => BUILTIN_BD,
        "pond_turtle"    => BUILTIN_PT,
        _                => b"",
    };

    let mut display = SpeechDisplay::default();
    let mut entries: Vec<SpeechEntry> = Vec::new();

    // Layers 1 & 2: embedded builtin data.
    for bytes in [BUILTIN_COMMON, builtin_char] {
        if let Some(file) = parse_bytes(bytes) {
            merge_layer(&mut display, &mut entries, file);
        }
    }

    // Layers 3 & 4: optional user files from AppSupport.
    if let Some(app_dir) = crate::user_config::app_support_dir() {
        let user_char   = app_dir.join(char_name).join("speech.toml");
        let user_common = app_dir.join("common").join("speech.toml");
        for path in [user_char.as_path(), user_common.as_path()] {
            if let Some(file) = parse_path(path) {
                merge_layer(&mut display, &mut entries, file);
            }
        }
    }

    SpeechData { display, entries }
}

// ---- Speech engine ----

use std::time::Instant;
use rand::{Rng, SeedableRng, rngs::SmallRng};
use crate::behavior::State;

/// A resolved speech line ready for display.
#[derive(Clone, Debug)]
pub struct SpeechLine {
    pub text_ja: Option<String>,
    pub text_en: Option<String>,
    pub duration_sec: f64,
    pub fade_out_sec: f64,
}

/// Per-character speech trigger engine.
///
/// Call [`SpeechEngine::tick`] every game tick. When it returns `Some`, display
/// the resulting [`SpeechLine`] and set the global speech lock in `AppState`.
pub struct SpeechEngine {
    data: SpeechData,
    rng: SmallRng,
    last_tick: Instant,
    /// Counts up each tick; fires a random/time trigger when >= `next_random_interval`.
    random_timer: f64,
    next_random_interval: f64,
    /// True once the "startup" state entry has been fired.
    startup_fired: bool,
    /// Whether the previous tick was in `LandingStandUp`.
    prev_in_landing: bool,
}

impl SpeechEngine {
    pub fn new(data: SpeechData) -> Self {
        let mut rng = SmallRng::from_os_rng();
        // Stagger first random speech so multiple characters don't fire together.
        let initial_offset = 15.0 + rng.random::<f64>() * 30.0;
        let next_interval = 45.0 + rng.random::<f64>() * 45.0;
        SpeechEngine {
            data,
            rng,
            last_tick: Instant::now(),
            random_timer: -initial_offset,
            next_random_interval: next_interval,
            startup_fired: false,
            prev_in_landing: false,
        }
    }

    /// Called every tick. Returns a speech line if one should be displayed.
    ///
    /// `lock_remaining`: global speech lock countdown (seconds). When > 0 the
    /// random/time triggers are suppressed; state triggers still fire.
    pub fn tick(&mut self, state: &State, lock_remaining: f64) -> Option<SpeechLine> {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.5);
        self.last_tick = now;

        let in_landing = matches!(state, State::LandingStandUp { .. });
        let entered_landing = !self.prev_in_landing && in_landing;
        self.prev_in_landing = in_landing;

        // State triggers bypass the global lock — they are event-driven.
        if entered_landing {
            let trigger_name = if !self.startup_fired {
                self.startup_fired = true;
                "startup"
            } else {
                "landing"
            };
            if let Some(line) = self.pick_state(trigger_name) {
                return Some(line);
            }
        }

        // Random/time triggers: advance timer and check.
        self.random_timer += dt;
        if self.random_timer < self.next_random_interval || lock_remaining > 0.0 {
            return None;
        }
        self.random_timer = 0.0;
        self.next_random_interval = 45.0 + self.rng.random::<f64>() * 45.0;
        self.pick_random_or_time()
    }

    fn pick_state(&mut self, trigger_name: &str) -> Option<SpeechLine> {
        let candidates: Vec<&SpeechEntry> = self.data.entries.iter()
            .filter(|e| {
                e.trigger == TriggerKind::State
                    && e.state.as_deref() == Some(trigger_name)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let idx = self.rng.random_range(0..candidates.len());
        Some(self.make_line(candidates[idx]))
    }

    fn pick_random_or_time(&mut self) -> Option<SpeechLine> {
        let (hour, weekday, month, season) = current_time_info();
        let candidates: Vec<(&SpeechEntry, f64)> = self.data.entries.iter()
            .filter_map(|e| match e.trigger {
                TriggerKind::Random => Some((e, e.weight)),
                TriggerKind::Time => {
                    if time_matches(e, hour, &weekday, month, &season) {
                        Some((e, e.weight))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let total: f64 = candidates.iter().map(|(_, w)| w).sum();
        let pick = self.rng.random::<f64>() * total;
        let mut acc = 0.0;
        for (entry, weight) in &candidates {
            acc += weight;
            if pick < acc {
                return Some(self.make_line(entry));
            }
        }
        // Fallback: last entry (handles floating-point rounding at the boundary).
        Some(self.make_line(candidates.last().unwrap().0))
    }

    fn make_line(&self, entry: &SpeechEntry) -> SpeechLine {
        SpeechLine {
            text_ja: entry.text_ja.clone(),
            text_en: entry.text_en.clone(),
            duration_sec: entry.duration_sec.unwrap_or(self.data.display.duration_sec),
            fade_out_sec: self.data.display.fade_out_sec,
        }
    }
}

fn current_time_info() -> (u8, String, u8, String) {
    use chrono::{Datelike, Timelike, Local, Weekday};
    let now = Local::now();
    let hour = now.hour() as u8;
    let weekday = match now.weekday() {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
    .to_owned();
    let month = now.month() as u8;
    let season = match month {
        3..=5  => "spring",
        6..=8  => "summer",
        9..=11 => "autumn",
        _      => "winter",
    }
    .to_owned();
    (hour, weekday, month, season)
}

fn time_matches(entry: &SpeechEntry, hour: u8, weekday: &str, month: u8, season: &str) -> bool {
    if let Some(hours) = &entry.hours {
        if !hours.contains(&hour) {
            return false;
        }
    }
    if let Some(weekdays) = &entry.weekdays {
        if !weekdays.iter().any(|d| d == weekday) {
            return false;
        }
    }
    if let Some(months) = &entry.months {
        if !months.contains(&month) {
            return false;
        }
    }
    if let Some(seasons) = &entry.seasons {
        if !seasons.iter().any(|s| s == season) {
            return false;
        }
    }
    true
}
