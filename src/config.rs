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

    /// Period of one head-bob cycle (stand ↔ stand-close) [min, max] s.
    pub headbob_period: [f64; 2],

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
            headbob_period: [0.8, 1.2],
            peek_prob: 0.20,
            peek_duration: 0.5,
            edge_idle_prob: 0.40,
            edge_stand_to_sit_prob: 0.50,
            edge_sit_to_lie_prob: 0.50,
            edge_lie_to_sleep_prob: 0.30,
            standup_duration: 0.8,
            turn_duration: 0.7,
            observe_duration: [3.0, 8.0],
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
}

impl Default for CornerConfig {
    fn default() -> Self {
        Self {
            side_corner_secs: 0.3,
            front_corner_secs: 0.5,
            rest_prob: 0.30,
            rest_duration: [3.0, 8.0],
            lower_rest_duration: [1.0, 3.0],
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
}

impl Default for JumpConfig {
    fn default() -> Self {
        Self {
            gravity: 0.6,
            runup_duration: 0.3,
            wall_jump_max_dist: 80.0,
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
    /// Create a loader. If `config.toml` does not exist, default values are used.
    pub fn new(char_dir: &Path) -> Self {
        let path = char_dir.join("config.toml");
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

pub fn make_shared(char_dir: &Path) -> SharedConfig {
    Arc::new(Mutex::new(ConfigLoader::new(char_dir)))
}
