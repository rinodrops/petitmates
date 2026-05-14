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
