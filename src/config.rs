/// Runtime-tunable behavior parameters.
/// Loaded from `config.toml` in the character directory.
/// The file is re-read on every change (hot-reload) so values take effect
/// without rebuilding or restarting.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// ---- Parameter structs ----

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct FloorConfig {
    /// Walking speed on floor/window-top surface (px/s, display coords).
    pub walk_speed: f64,
    /// Duration of one walk animation frame (seconds).
    pub walk_frame_secs: f64,

    /// How long the character stands idle before considering a transition [min, max] s.
    pub stand_duration: [f64; 2],
    /// How long the character sits idle [min, max] s.
    pub sit_duration: [f64; 2],
    /// How long the character lies idle [min, max] s.
    pub lie_duration: [f64; 2],
    /// How long the character sleeps [min, max] s.
    pub sleep_duration: [f64; 2],

    /// Interval between head-bobs (mouth closed → open) [min, max] s.
    pub headbob_period: [f64; 2],
    /// How long the mouth stays open during a single head-bob [min, max] s.
    pub headbob_open_duration: [f64; 2],

    /// Probability of peek-down when arriving at an edge (0..1).
    pub peek_prob: f64,
    /// How long peek-down is held (seconds).
    pub peek_duration: f64,

    /// Probability of idling (stand/sit/lie/sleep) at a window-top edge instead of
    /// immediately rounding the corner (0..1).
    pub edge_idle_prob: f64,

    /// When standing at a window-top edge: probability of sitting down (vs rounding corner).
    pub edge_stand_to_sit_prob: f64,
    /// When sitting at a window-top edge: probability of lying down (vs standing back up).
    pub edge_sit_to_lie_prob: f64,
    /// When lying at a window-top edge: probability of falling asleep (vs sitting back up).
    pub edge_lie_to_sleep_prob: f64,

    /// Duration of stand-up animation after landing (seconds).
    pub standup_duration: f64,
    /// Duration of turn-around animation (seconds).
    pub turn_duration: f64,

    /// Observation phase duration after landing [min, max] s.
    pub observe_duration: [f64; 2],

    /// How long the character looks sideways before turning head forward
    /// during SitIdle / LieIdle / Sleeping [min, max] s.
    pub head_side_duration: [f64; 2],
    /// How long the character looks forward before turning head back to side
    /// during SitIdle / LieIdle / Sleeping [min, max] s.
    pub head_front_duration: [f64; 2],

    // ---- State-transition probabilities ----

    /// After PeekDown: probability of walking (vs turning around) (0..1).
    pub peek_walk_prob: f64,
    /// After TurningAround: probability of walking (vs entering StandIdle) (0..1).
    pub turn_walk_prob: f64,

    /// After StandIdle (non-edge): cumulative thresholds.
    /// r < sit  → SitIdle; r < walk → walk; r < turn → TurningAround; else PeekDown.
    pub stand_idle_sit_prob: f64,
    pub stand_idle_walk_prob: f64,
    pub stand_idle_turn_prob: f64,

    /// After SitIdle (non-edge): cumulative thresholds.
    /// r < lie → LieIdle; r < stand → StandIdle; else walk.
    pub sit_idle_lie_prob: f64,
    pub sit_idle_stand_prob: f64,

    /// After LieIdle (non-edge): cumulative thresholds.
    /// r < sleep → Sleeping; r < sit → SitIdle; else walk.
    pub lie_idle_sleep_prob: f64,
    pub lie_idle_sit_prob: f64,

    /// Initial idle state when arriving at a window-top edge while Walking.
    /// Cumulative thresholds (only used when edge_idle_prob fires):
    /// r < stand → StandIdle; r < sit → SitIdle; r < lie → LieIdle; else Sleeping.
    pub edge_arrive_stand_prob: f64,
    pub edge_arrive_sit_prob: f64,
    pub edge_arrive_lie_prob: f64,

    /// Probability of falling (surprised) instead of rounding the corner,
    /// applied only among the (1 - edge_idle_prob) fraction that don't idle.
    /// Shows `f-shocked` on the way down.
    pub edge_fall_prob: f64,
    /// How long the `f-shocked` sprite is shown after an edge-fall (seconds).
    pub shocked_duration: f64,
}

impl Default for FloorConfig {
    fn default() -> Self {
        Self {
            walk_speed: 40.0,
            walk_frame_secs: 0.14,
            stand_duration: [3.0, 8.0],
            sit_duration: [5.0, 15.0],
            lie_duration: [20.0, 60.0],
            sleep_duration: [60.0, 180.0],
            headbob_period: [30.0, 90.0],
            headbob_open_duration: [0.3, 0.5],
            peek_prob: 0.20,
            peek_duration: 0.5,
            edge_idle_prob: 0.40,
            edge_stand_to_sit_prob: 0.50,
            edge_sit_to_lie_prob: 0.50,
            edge_lie_to_sleep_prob: 0.30,
            standup_duration: 0.8,
            turn_duration: 0.7,
            observe_duration: [3.0, 8.0],
            head_side_duration:  [10.0, 25.0],
            head_front_duration: [ 2.0,  6.0],

            peek_walk_prob: 0.5,
            turn_walk_prob: 0.7,

            stand_idle_sit_prob:  0.40,
            stand_idle_walk_prob: 0.60,
            stand_idle_turn_prob: 0.80,

            sit_idle_lie_prob:   0.30,
            sit_idle_stand_prob: 0.65,

            lie_idle_sleep_prob: 0.15,
            lie_idle_sit_prob:   0.60,

            edge_arrive_stand_prob: 0.40,
            edge_arrive_sit_prob:   0.70,
            edge_arrive_lie_prob:   0.90,

            edge_fall_prob:    0.10,
            shocked_duration:  0.6,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct WallConfig {
    /// Climbing speed (px/s, display coords).
    pub climb_speed: f64,
    /// Duration of one climb animation frame (seconds).
    pub climb_frame_secs: f64,
    /// Probability of pausing per 3 frames (0..1).
    pub pause_prob: f64,
    /// Duration of wall pause [min, max] s.
    pub pause_duration: [f64; 2],
    /// Hold time of `s-hang-wall-0` when first attaching to wall (seconds).
    pub entry_hold: f64,
}

impl Default for WallConfig {
    fn default() -> Self {
        Self {
            climb_speed: 60.0,
            climb_frame_secs: 0.22,
            pause_prob: 0.15,
            pause_duration: [2.0, 5.0],
            entry_hold: 0.5,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct CornerConfig {
    /// Display duration of `s-hang-corner` transition sprite (seconds).
    pub side_corner_secs: f64,
    /// Display duration of `f-hang-corner` transition sprite (seconds).
    pub front_corner_secs: f64,
    /// Probability of resting at upper corner (0..1).
    pub rest_prob: f64,
    /// Upper corner rest duration [min, max] s.
    pub rest_duration: [f64; 2],
    /// Lower corner rest duration [min, max] s.
    pub lower_rest_duration: [f64; 2],
    /// Probability of lying (vs sitting) when entering CornerRest (0..1).
    pub rest_lying_prob: f64,
    /// After CornerRest: probability of descending the wall (vs walking inward) (0..1).
    pub rest_descend_prob: f64,
    /// Probability of jumping to a nearby window at the end of CornerRest (0..1).
    pub corner_jump_prob: f64,
    /// Horizontal + vertical detection radius for corner-to-window jump (px, display coords).
    pub corner_jump_dist: f64,
    /// Forced outing interval [min, max] s. After this time without a window-to-window
    /// jump, the next eligible idle state will trigger a jump if a target is in range.
    /// Set to [0, 0] to disable forced outings.
    pub outing_interval: [f64; 2],
}

impl Default for CornerConfig {
    fn default() -> Self {
        Self {
            side_corner_secs: 0.3,
            front_corner_secs: 0.5,
            rest_prob: 0.30,
            rest_duration: [3.0, 8.0],
            lower_rest_duration: [1.0, 3.0],
            rest_lying_prob: 0.5,
            rest_descend_prob: 0.5,            corner_jump_prob:  0.20,
            corner_jump_dist:  300.0,
            outing_interval:   [300.0, 900.0],
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct JumpConfig {
    /// Gravity factor; effective vertical acceleration = gravity × 60 px/s².
    pub gravity: f64,
    /// Jump-to-wall run-up duration (seconds).
    pub runup_duration: f64,
    /// Max distance from Dock for wall-jump to be allowed (px, display coords).
    pub wall_jump_max_dist: f64,
    /// Windows whose bottom is more than this many px above the Dock/taskbar
    /// are ignored for wall-jump and window-attraction purposes.
    pub wall_jump_floor_margin: f64,
    /// Horizontal detection radius for spontaneous window-climbing attraction
    /// (px, from character centre to window edge). Checked in both directions.
    pub climb_attract_dist: f64,
    /// Probability of being spontaneously attracted to a nearby window when an
    /// idle state (Observing / StandIdle / SitIdle / LieIdle) expires on the
    /// Desktop surface (0..1).
    pub climb_attract_prob: f64,
}

impl Default for JumpConfig {
    fn default() -> Self {
        Self {
            gravity: 0.6,
            runup_duration: 0.3,
            wall_jump_max_dist: 80.0,
            wall_jump_floor_margin: 150.0,
            climb_attract_dist: 600.0,
            climb_attract_prob: 0.35,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(default)]
pub struct DisplayConfig {
    /// Display width of the character in logical pixels.
    pub display_width: f64,
    /// Alpha when mouse is over the character.
    pub hover_alpha: f64,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            display_width: 150.0,
            hover_alpha: 0.25,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub floor: FloorConfig,
    pub wall: WallConfig,
    pub corner: CornerConfig,
    pub jump: JumpConfig,
    pub display: DisplayConfig,
}

// ---- Hot-reload wrapper ----

pub struct ConfigLoader {
    path: PathBuf,
    last_modified: Option<SystemTime>,
    pub current: Config,
}

impl ConfigLoader {
    /// Create a loader that watches `char_dir/config.toml`.
    /// If the file does not exist, defaults are used.
    #[allow(dead_code)]
    pub fn new(char_dir: &Path) -> Self {
        Self::new_with_path(char_dir.join("config.toml"))
    }

    /// Create a loader that watches a specific path.
    /// If the path does not exist (or cannot be read), defaults are used.
    pub fn new_with_path(path: PathBuf) -> Self {
        let mut loader = Self {
            path,
            last_modified: None,
            current: Config::default(),
        };
        loader.reload_if_changed();
        loader
    }

    /// Call this each tick. Returns `true` if the config was reloaded.
    pub fn reload_if_changed(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();

        if mtime == self.last_modified {
            return false;
        }
        self.last_modified = mtime;

        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if let Ok(cfg) = toml::from_str::<Config>(&text) {
                self.current = cfg;
                return true;
            }
        }
        false
    }
}

/// Thread-safe shared handle to a `ConfigLoader`.
pub type SharedConfig = Arc<Mutex<ConfigLoader>>;

/// macOS / Windows-with-file: watch `char_dir/config.toml`, hot-reload on change.
#[allow(dead_code)]
pub fn make_shared(char_dir: &Path) -> SharedConfig {
    Arc::new(Mutex::new(ConfigLoader::new(char_dir)))
}

/// Windows standalone (embedded assets): watch for a `{char_name}_config.toml`
/// placed next to `petitmates.exe`.  Falls back to built-in defaults if the
/// file is absent, allowing the exe to run with no external files at all.
///
/// Example: `make_shared_win_for("bearded_dragon")` watches
/// `bearded_dragon_config.toml` in the same directory as the executable.
#[cfg(target_os = "windows")]
pub fn make_shared_win_for(char_name: &str) -> SharedConfig {
    let filename = format!("{char_name}_config.toml");
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(&filename)))
        .unwrap_or_default();
    Arc::new(Mutex::new(ConfigLoader::new_with_path(path)))
}

/// Convenience alias: watch for a single `config.toml` next to the exe.
/// Kept for compatibility; prefer `make_shared_win_for` for per-character use.
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn make_shared_win() -> SharedConfig {
    make_shared_win_for("config")
}
