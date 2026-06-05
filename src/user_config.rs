/// User-specific settings persisted in `user.toml`.
///
/// Located in the OS-standard application support directory:
/// - macOS:   ~/Library/Application Support/PetitMates/user.toml
/// - Windows: %APPDATA%\PetitMates\user.toml
///
/// The file is auto-generated with defaults on first launch.
/// Users can edit it manually; changes take effect on next launch.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

// ---- Config structs ----

/// ConfUI writes slider/drag values as TOML floats (e.g. `300.0`); accept integers too.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum TomlU32 {
    Int(i64),
    Float(f64),
}

fn deserialize_u32_lossy<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match TomlU32::deserialize(deserializer)? {
        TomlU32::Int(v) if v >= 0 => Ok(v as u32),
        TomlU32::Float(v) if v >= 0.0 => Ok(v.round() as u32),
        _ => Err(serde::de::Error::custom("expected non-negative number")),
    }
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(default)]
pub struct DisplayConfig {
    /// Character sprite size in pixels (applies to all characters).
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub sprite_size: u32,
    /// Font size in points for speech bubbles (OS default font).
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub font_size: u32,
    /// Speech bubble display language: `"os"`, `"en"`, or `"ja"`.
    /// When absent or `"os"`, the OS preferred language is used (falls back to `"en"`).
    pub language: Option<String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            sprite_size: 150,
            font_size: 14,
            language: None,
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
    /// Optional manual latitude override (normally omitted; use `city` instead).
    /// The app does not write geocoded coordinates back to `user.toml`.
    pub latitude: Option<f64>,
    /// Optional manual longitude override (normally omitted; use `city` instead).
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

// ---- Language resolution ----

/// Resolves `display.language` for runtime UI and speech.
///
/// `None`, empty, or `"os"` follows the OS preferred language (`"ja"` or `"en"`).
pub fn resolve_display_language(lang: &Option<String>) -> String {
    match lang.as_deref() {
        None | Some("") | Some("os") => detect_system_language(),
        Some(code) => code.to_string(),
    }
}

#[cfg(target_os = "macos")]
fn detect_system_language() -> String {
    use objc2_foundation::NSLocale;
    let langs = NSLocale::preferredLanguages();
    for i in 0..langs.len() {
        let tag: String = langs.objectAtIndex(i).to_string();
        if tag.starts_with("ja") {
            return "ja".to_owned();
        }
        if tag.starts_with("en") {
            return "en".to_owned();
        }
    }
    "en".to_owned()
}

#[cfg(target_os = "windows")]
fn detect_system_language() -> String {
    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::Globalization::GetUserPreferredUILanguages;

    const MUI_LANGUAGE_NAME: u32 = 0x08;
    unsafe {
        let mut num_langs: u32 = 0;
        let mut buf_size: u32 = 0;
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_langs,
            std::ptr::null_mut(),
            &mut buf_size,
        );
        if buf_size == 0 {
            return "en".to_owned();
        }
        let mut buf: Vec<u16> = vec![0; buf_size as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut num_langs,
            buf.as_mut_ptr(),
            &mut buf_size,
        ) == FALSE
        {
            return "en".to_owned();
        }
        for segment in buf.split(|&c| c == 0) {
            if segment.is_empty() {
                continue;
            }
            let tag = String::from_utf16_lossy(segment);
            if tag.starts_with("ja") {
                return "ja".to_owned();
            }
            if tag.starts_with("en") {
                return "en".to_owned();
            }
        }
        "en".to_owned()
    }
}

// ---- Load / save ----

const DEFAULT_TOML: &str = r#"[display]
sprite_size = 150   # character size in pixels
font_size   = 14    # speech bubble font size in points
# language  = "os"   # "os" (default), "en", or "ja"

[speech]
enabled         = true    # set to false to silence all characters
speech_lock_sec = 30.0    # minimum seconds between speeches (global)

[weather]
enabled = true
# city = "Tokyo"   # uncomment and set your city name
"#;

/// Loads `user.toml` from the application support directory.
/// If the file does not exist, creates it with default values and returns defaults.
/// On read/parse failure, logs to stderr, shows a one-shot native warning dialog,
/// and returns defaults.
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
            warn_config_load_failed(&path, &format!("{e}"));
            return UserConfig::default();
        }
    };

    match toml::from_str::<UserConfig>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[user_config] failed to parse user.toml: {e}");
            warn_config_load_failed(&path, &format!("{e}"));
            UserConfig::default()
        }
    }
}

/// Native warning when `user.toml` could not be loaded (startup only in practice).
fn warn_config_load_failed(path: &Path, detail: &str) {
    let path_str = path.display();
    let ja = detect_system_language() == "ja";
    let (title, message) = if ja {
        (
            "設定を読み込めませんでした",
            format!(
                "user.toml を読み込めなかったため、デフォルト設定で起動します。\n\n\
                 ファイル: {path_str}\n\
                 {detail}\n\n\
                 Option を押しながらメニューの「設定ファイルを開く」で修正するか、\
                 ConfUI で内容を直してください。"
            ),
        )
    } else {
        (
            "Could Not Load Settings",
            format!(
                "Could not load user.toml — starting with default settings.\n\n\
                 File: {path_str}\n\
                 {detail}\n\n\
                 Hold Option and choose Open Settings File from the menu to edit, \
                 or fix the file in ConfUI."
            ),
        )
    };
    show_native_error(title, &message);
}

/// Path to the ConfUI settings binary bundled next to the main executable.
pub fn confui_exe_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    #[cfg(target_os = "windows")]
    let name = "ConfUI.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "confui";
    let path = dir.join(name);
    path.exists().then_some(path)
}

/// Launches the ConfUI settings window for `user.toml`.
pub fn launch_settings_ui() {
    let Some(confui) = confui_exe_path() else {
        show_settings_error(
            "Settings Unavailable",
            "The settings UI (ConfUI) was not found next to the application.\n\
             Reinstall Petit Mates or rebuild with ConfUI bundled.",
        );
        return;
    };

    let path = match user_config_path() {
        Some(p) => p,
        None => {
            show_settings_error(
                "Settings Unavailable",
                "Could not resolve the Application Support directory for user.toml.",
            );
            return;
        }
    };

    if !path.exists() {
        if let Err(e) = std::fs::write(&path, DEFAULT_TOML) {
            show_settings_error(
                "Settings Unavailable",
                &format!("Could not create user.toml:\n{e}"),
            );
            return;
        }
    }

    if let Err(e) = Command::new(&confui).arg(&path).spawn() {
        show_settings_error(
            "Settings Unavailable",
            &format!("Failed to launch ConfUI:\n{e}"),
        );
    }
}

/// Opens `user.toml` in the system default text editor.
pub fn open_in_editor() {
    let Some(path) = user_config_path() else {
        return;
    };
    if !path.exists() {
        let _ = std::fs::write(&path, DEFAULT_TOML);
    }
    open_path_in_editor(&path);
}

fn open_path_in_editor(path: &Path) {
    let path_str = path.to_string_lossy();
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path_str.as_ref()).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("cmd")
            .args(["/C", "start", "", path_str.as_ref()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

fn show_native_error(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    show_native_error_macos(title, message);
    #[cfg(target_os = "windows")]
    show_native_error_windows(title, message);
}

fn show_settings_error(title: &str, message: &str) {
    show_native_error(title, message);
}

#[cfg(target_os = "macos")]
fn show_native_error_macos(title: &str, message: &str) {
    let script = format!(
        r#"display alert "{}" message "{}" as critical buttons {{"OK"}} default button "OK""#,
        escape_applescript_string(title),
        escape_applescript_string(message),
    );
    let _ = Command::new("osascript").arg("-e").arg(script).spawn();
}

#[cfg(target_os = "windows")]
fn show_native_error_windows(title: &str, message: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    fn to_wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
    }

    let text = to_wide(&message.replace('\n', "\r\n"));
    let caption = to_wide(title);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(target_os = "macos")]
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
