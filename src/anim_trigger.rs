//! Animation trigger system — `behavior.toml` data model, 4-layer merge loader,
//! and runtime `BehaviorEngine`.
//!
//! Layer order (later layers append entries; same as `speech.toml`):
//! 1. Builtin common  (`assets/common/behavior.toml`,          embedded via `include_bytes!`)
//! 2. Builtin char    (`assets/{char}/behavior.toml`,          embedded via `include_bytes!`)
//! 3. User char       (`{AppSupport}/{char}/behavior.toml`,    optional file)
//! 4. User common     (`{AppSupport}/common/behavior.toml`,    optional file)

// ---- Embedded builtin data ----

const BUILTIN_COMMON: &[u8] = include_bytes!("../assets/common/behavior.toml");
const BUILTIN_BD:     &[u8] = include_bytes!("../assets/bearded_dragon/behavior.toml");
const BUILTIN_PT:     &[u8] = include_bytes!("../assets/pond_turtle/behavior.toml");

// ---- Data model ----

fn default_weight()       -> f64 { 1.0   }
fn default_interval_min() -> f64 { 60.0  }
fn default_interval_max() -> f64 { 300.0 }

/// A single animation trigger entry from `behavior.toml`.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct BehaviorEntry {
    pub animation: String,
    pub trigger:   crate::speech::TriggerKind,

    // --- random ---
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_interval_min")]
    pub interval_min: f64,
    #[serde(default = "default_interval_max")]
    pub interval_max: f64,

    /// If set, only fire when the current state name matches one of these strings.
    pub states: Option<Vec<String>>,

    // --- time (AND conditions) ---
    pub hours:    Option<Vec<u8>>,
    pub weekdays: Option<Vec<String>>,
    pub months:   Option<Vec<u8>>,
    pub seasons:  Option<Vec<String>>,

    // --- weather ---
    pub weather:  Option<Vec<String>>,
    pub temp_max: Option<f64>,
    pub temp_min: Option<f64>,

    // --- state trigger ---
    #[allow(dead_code)]
    pub state: Option<String>,
}

/// Which user interaction fires a `[[reaction]]` entry.
#[derive(serde::Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReactionTrigger {
    Grabbed,
    Dropped,
    Hovered,
}

/// A single reaction entry from `behavior.toml`.
#[derive(serde::Deserialize, Clone, Debug)]
pub struct ReactionEntry {
    pub trigger:   ReactionTrigger,
    pub animation: String,
}

/// Merged behavior data for one character, ready for the trigger engine.
pub struct BehaviorData {
    pub entries:   Vec<BehaviorEntry>,
    pub reactions: Vec<ReactionEntry>,
}

// ---- Internal TOML file structure ----

#[derive(serde::Deserialize, Default)]
struct BehaviorFile {
    #[serde(default, rename = "behavior")]
    entries: Vec<BehaviorEntry>,
    #[serde(default, rename = "reaction")]
    reactions: Vec<ReactionEntry>,
}

// ---- Parsing helpers ----

fn parse_bytes(bytes: &[u8]) -> Option<BehaviorFile> {
    let text = std::str::from_utf8(bytes).ok()?;
    toml::from_str(text).ok()
}

fn parse_path(path: &std::path::Path) -> Option<BehaviorFile> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

// ---- Public API ----

/// Load and merge all 4 layers for `char_name` (e.g. `"bearded_dragon"`).
///
/// Always returns a valid `BehaviorData`; missing or malformed files are silently skipped.
pub fn load(char_name: &str) -> BehaviorData {
    let builtin_char: &[u8] = match char_name {
        "bearded_dragon" => BUILTIN_BD,
        "pond_turtle"    => BUILTIN_PT,
        _                => b"",
    };

    let mut entries:   Vec<BehaviorEntry>  = Vec::new();
    let mut reactions: Vec<ReactionEntry> = Vec::new();

    for bytes in [BUILTIN_COMMON, builtin_char] {
        if let Some(file) = parse_bytes(bytes) {
            entries.extend(file.entries);
            reactions.extend(file.reactions);
        }
    }

    if let Some(app_dir) = crate::user_config::app_support_dir() {
        let user_char   = app_dir.join(char_name).join("behavior.toml");
        let user_common = app_dir.join("common").join("behavior.toml");
        for path in [user_char.as_path(), user_common.as_path()] {
            if let Some(file) = parse_path(path) {
                entries.extend(file.entries);
                reactions.extend(file.reactions);
            }
        }
    }

    BehaviorData { entries, reactions }
}

// ---- Trigger evaluation helpers ----

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

fn time_matches(e: &BehaviorEntry, hour: u8, weekday: &str, month: u8, season: &str) -> bool {
    if let Some(hs) = &e.hours    { if !hs.contains(&hour)                      { return false; } }
    if let Some(ds) = &e.weekdays { if !ds.iter().any(|d| d == weekday)         { return false; } }
    if let Some(ms) = &e.months   { if !ms.contains(&month)                     { return false; } }
    if let Some(ss) = &e.seasons  { if !ss.iter().any(|s| s == season)          { return false; } }
    true
}

fn weather_matches(e: &BehaviorEntry, info: &crate::weather::WeatherInfo) -> bool {
    if let Some(cats) = &e.weather {
        let cat_str = info.category.as_str();
        if !cats.iter().any(|c| c == cat_str) { return false; }
    }
    if let Some(max) = e.temp_max { if info.temp_c > max { return false; } }
    if let Some(min) = e.temp_min { if info.temp_c < min { return false; } }
    true
}

fn state_name(state: &crate::behavior::State) -> &'static str {
    use crate::behavior::State;
    match state {
        State::Falling { .. }              => "Falling",
        State::Airborne { .. }             => "Airborne",
        State::LandingStandUp { .. }       => "LandingStandUp",
        State::Observing { .. }            => "Observing",
        State::Walking { .. }              => "Walking",
        State::Running { .. }              => "Running",
        State::TurningAround { .. }        => "TurningAround",
        State::StandIdle { .. }            => "StandIdle",
        State::SitIdle { .. }              => "SitIdle",
        State::LieIdle { .. }              => "LieIdle",
        State::Sleeping { .. }             => "Sleeping",
        State::SurfaceInteract { .. }      => "SurfaceInteract",
        State::JumpRunup { .. }            => "JumpRunup",
        State::ClimbingUp { .. }           => "ClimbingUp",
        State::ClimbingDown { .. }         => "ClimbingDown",
        State::WallPause { .. }            => "WallPause",
        State::WallEntry { .. }            => "WallEntry",
        State::CornerTransitionSide { .. } => "CornerTransitionSide",
        State::CornerTransitionFront { .. }=> "CornerTransitionFront",
        State::CornerRest { .. }           => "CornerRest",
        State::OneShot { .. }              => "OneShot",
        State::Grabbed                     => "Grabbed",
    }
}

// ---- Engine ----

use std::time::Instant;
use rand::{Rng, SeedableRng, rngs::SmallRng};
use crate::speech::TriggerKind;

/// Per-character animation trigger engine.
///
/// Call [`BehaviorEngine::tick`] every game tick. When it returns `Some`, play the
/// named animation as `State::OneShot`.
///
/// Entries whose `animation` does not exist in the manifest are removed at
/// construction time (silently ignored, per spec).
pub struct BehaviorEngine {
    data:     BehaviorData,
    rng:      SmallRng,
    last_tick: Instant,
    /// Per-entry elapsed time (seconds) since last firing.
    elapsed:  Vec<f64>,
    /// Post-fire cooldown: prevents two animations from firing in rapid succession.
    cooldown: f64,
}

impl BehaviorEngine {
    /// Create a new engine for `data`, filtered to animations present in `animations`.
    pub fn new(
        data: BehaviorData,
        animations: &std::collections::HashMap<String, crate::manifest::AnimationDef>,
    ) -> Self {
        let entries: Vec<BehaviorEntry> = data.entries.into_iter()
            .filter(|e| animations.contains_key(&e.animation))
            .collect();
        let reactions: Vec<ReactionEntry> = data.reactions.into_iter()
            .filter(|r| animations.contains_key(&r.animation))
            .collect();
        let filtered = BehaviorData { entries, reactions };

        let mut rng = SmallRng::from_os_rng();
        // Randomize initial elapsed so multiple characters don't sync up.
        let elapsed: Vec<f64> = filtered.entries.iter()
            .map(|e| rng.random::<f64>() * e.interval_max)
            .collect();
        // Small initial cooldown to avoid firing right at startup.
        let cooldown = 10.0 + rng.random::<f64>() * 10.0;

        BehaviorEngine {
            data: filtered,
            rng,
            last_tick: Instant::now(),
            elapsed,
            cooldown,
        }
    }

    /// Called every tick. Returns an animation name to play as `State::OneShot`, or `None`.
    ///
    /// Suppressed when:
    /// - `has_bubble` — a speech bubble is visible
    /// - state is `Falling`, `Airborne`, `Grabbed`, or an unfinished `OneShot`
    pub fn tick(
        &mut self,
        state:   &crate::behavior::State,
        has_bubble: bool,
        weather: Option<&crate::weather::WeatherInfo>,
    ) -> Option<String> {
        let now = Instant::now();
        let dt  = now.duration_since(self.last_tick).as_secs_f64().min(0.5);
        self.last_tick = now;

        for e in &mut self.elapsed { *e += dt; }
        self.cooldown = (self.cooldown - dt).max(0.0);

        // Suppression checks.
        if has_bubble || self.cooldown > 0.0 { return None; }
        if matches!(
            state,
            crate::behavior::State::Falling  { .. }
            | crate::behavior::State::Airborne { .. }
            | crate::behavior::State::Grabbed
            | crate::behavior::State::OneShot { done: false, .. }
        ) {
            return None;
        }

        let sname = state_name(state);
        let (hour, weekday, month, season) = current_time_info();

        // Collect candidates with weights.
        let candidates: Vec<(usize, f64)> = self.data.entries.iter().enumerate()
            .filter_map(|(i, e)| {
                if self.elapsed[i] < e.interval_min { return None; }
                // State filter (optional allow-list).
                if let Some(states) = &e.states {
                    if !states.iter().any(|s| s == sname) { return None; }
                }
                match e.trigger {
                    TriggerKind::Random => Some((i, e.weight)),
                    TriggerKind::Time => {
                        if time_matches(e, hour, &weekday, month, &season) {
                            Some((i, e.weight))
                        } else {
                            None
                        }
                    }
                    TriggerKind::Weather => {
                        weather.and_then(|w| {
                            if weather_matches(e, w) { Some((i, e.weight)) } else { None }
                        })
                    }
                    // State-trigger behaviours are not evaluated here.
                    TriggerKind::State => None,
                }
            })
            .collect();

        if candidates.is_empty() { return None; }

        // Weighted random selection.
        let total: f64 = candidates.iter().map(|(_, w)| *w).sum();
        let pick  = self.rng.random::<f64>() * total;
        let mut acc = 0.0;
        let mut chosen_idx = candidates.last().map(|&(i, _)| i);
        for &(i, w) in &candidates {
            acc += w;
            if pick < acc {
                chosen_idx = Some(i);
                break;
            }
        }

        if let Some(i) = chosen_idx {
            let anim = self.data.entries[i].animation.clone();
            self.elapsed[i] = 0.0;
            self.cooldown   = 5.0 + self.rng.random::<f64>() * 10.0;
            Some(anim)
        } else {
            None
        }
    }

    /// Returns an animation to play in response to a user interaction event.
    pub fn on_interaction(&self, trigger: ReactionTrigger) -> Option<String> {
        self.data.reactions.iter()
            .find(|r| r.trigger == trigger)
            .map(|r| r.animation.clone())
    }
}
