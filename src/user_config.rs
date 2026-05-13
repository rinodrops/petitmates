/// User-specific settings persisted in `user.toml`.
///
/// Located in the OS-standard application support directory:
/// - macOS:   ~/Library/Application Support/PetitMates/user.toml
/// - Windows: %APPDATA%\PetitMates\user.toml
///
/// The file is auto-generated with defaults on first launch.
/// Users can edit it manually; changes take effect on next launch.

use std::path::PathBuf;

// ---- Config structs ----

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(default)]
pub struct DisplayConfig {
    /// Character sprite size in pixels (applies to all characters).
    pub sprite_size: u32,
    /// Font size in points for speech bubbles (OS default font).
    pub font_size: u32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            sprite_size: 150,
            font_size: 14,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(default)]
pub struct SpeechConfig {
    /// Set to false to silence all characters.
    pub enabled: bool,
    /// Minimum seconds between speeches across all characters (global lock).
    pub speech_lock_sec: f64,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            speech_lock_sec: 30.0,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(default)]
pub struct WeatherConfig {
    /// Set to false to disable weather fetching entirely.
    pub enabled: bool,
    /// City name resolved to lat/lon via Open-Meteo geocoding API.
    pub city: Option<String>,
    /// Latitude override (auto-populated from city if unset).
    pub latitude: Option<f64>,
    /// Longitude override (auto-populated from city if unset).
    pub longitude: Option<f64>,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            city: None,
            latitude: None,
            longitude: None,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct UserConfig {
    pub display: DisplayConfig,
    pub speech: SpeechConfig,
    pub weather: WeatherConfig,
}

// ---- Path resolution ----

/// Returns the path to the PetitMates application support directory.
/// Creates the directory if it does not exist.
pub fn app_support_dir() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    let dir = base.join("PetitMates");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Returns the path to `user.toml`.
pub fn user_config_path() -> Option<PathBuf> {
    Some(app_support_dir()?.join("user.toml"))
}

// ---- Load / save ----

const DEFAULT_TOML: &str = r#"[display]
sprite_size = 150   # character size in pixels
font_size   = 14    # speech bubble font size in points

[speech]
enabled         = true    # set to false to silence all characters
speech_lock_sec = 30.0    # minimum seconds between speeches (global)

[weather]
enabled = true
# city = "Tokyo"   # uncomment and set your city name
"#;

/// Loads `user.toml` from the application support directory.
/// If the file does not exist, creates it with default values and returns defaults.
/// Parse errors are logged to stderr and defaults are returned.
pub fn load() -> UserConfig {
    let path = match user_config_path() {
        Some(p) => p,
        None => {
            eprintln!("[user_config] could not resolve app support dir, using defaults");
            return UserConfig::default();
        }
    };

    if !path.exists() {
        if let Err(e) = std::fs::write(&path, DEFAULT_TOML) {
            eprintln!("[user_config] failed to write default user.toml: {e}");
        }
        return UserConfig::default();
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[user_config] failed to read user.toml: {e}");
            return UserConfig::default();
        }
    };

    match toml::from_str::<UserConfig>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[user_config] failed to parse user.toml: {e}");
            UserConfig::default()
        }
    }
}

/// Opens `user.toml` in the system default text editor.
/// No-op if the path cannot be resolved.
#[cfg(target_os = "macos")]
pub fn open_in_editor() {
    if let Some(path) = user_config_path() {
        // Ensure the file exists before opening.
        if !path.exists() {
            let _ = std::fs::write(&path, DEFAULT_TOML);
        }
        let path_str = path.to_string_lossy();
        let _ = std::process::Command::new("open").arg(path_str.as_ref()).spawn();
    }
}

#[cfg(target_os = "windows")]
pub fn open_in_editor() {
    if let Some(path) = user_config_path() {
        if !path.exists() {
            let _ = std::fs::write(&path, DEFAULT_TOML);
        }
        let path_str = path.to_string_lossy();
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", path_str.as_ref()])
            .spawn();
    }
}
