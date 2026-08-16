/// User-specific settings persisted in `user.toml`.
///
/// Located in the OS-standard application support directory:
/// - macOS:   ~/Library/Application Support/PetitMates/user.toml
/// - Windows: %APPDATA%\PetitMates\user.toml
///
/// The file is auto-generated with defaults on first launch.
/// Users can edit it manually, or via the settings window (**Settings…** in the menu).
/// When the settings window closes, Petit Mates restarts so display / speech / weather / characters apply cleanly.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Deserialize;

/// Environment variable set on a Windows replacement instance while the prior
/// process is shutting down (single-instance mutex handoff).
#[cfg(target_os = "windows")]
pub const RESTART_ENV: &str = "PETITMATES_RESTARTING";

static SETTINGS_WATCHER_ACTIVE: Mutex<bool> = Mutex::new(false);
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

/// `true` when this process was spawned to replace a settings-session exit (Windows).
#[cfg(target_os = "windows")]
pub fn is_restarting_instance() -> bool {
    std::env::var_os(RESTART_ENV).is_some()
}

/// Main loop should relaunch and exit (settings window closed). Consumed once per request.
pub fn take_restart_request() -> bool {
    RESTART_REQUESTED.swap(false, Ordering::SeqCst)
}

// ---- Config structs ----

/// Settings may write slider/drag values as TOML floats (e.g. `300.0`); accept integers too.
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
    /// Animation fps when running on AC power. Valid values: 10, 15, 30, 60.
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub tick_rate_ac: u32,
    /// Animation fps when running on battery power. Valid values: 10, 15, 30, 60.
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub tick_rate_battery: u32,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            sprite_size: 150,
            font_size: 14,
            language: None,
            tick_rate_ac: 30,
            tick_rate_battery: 15,
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

/// Per-species startup spawn counts (0–5 each). Applied on launch only; menu add/remove is runtime-only.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(default)]
pub struct CharactersConfig {
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub bearded_dragon: u32,
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub pond_turtle: u32,
    #[serde(deserialize_with = "deserialize_u32_lossy")]
    pub leopard_gecko: u32,
}

impl Default for CharactersConfig {
    fn default() -> Self {
        Self {
            bearded_dragon: 1,
            pond_turtle: 1,
            leopard_gecko: 1,
        }
    }
}

const MAX_STARTUP_COUNT: u32 = 5;

impl CharactersConfig {
    /// Clamp a raw count to the valid startup range (manual `user.toml` edits may exceed slider limits).
    fn clamp_count(n: u32) -> u32 {
        n.min(MAX_STARTUP_COUNT)
    }

    /// Resolved per-species counts after clamping and the all-zero fallback (1 bearded dragon).
    pub fn resolved_counts(&self) -> (u32, u32, u32) {
        let bd = Self::clamp_count(self.bearded_dragon);
        let pt = Self::clamp_count(self.pond_turtle);
        let lg = Self::clamp_count(self.leopard_gecko);
        if bd == 0 && pt == 0 && lg == 0 {
            (1, 0, 0)
        } else {
            (bd, pt, lg)
        }
    }

    /// Built-in species IDs to spawn at startup, in fixed order (BD → PT → LG).
    pub fn startup_species_ids(&self) -> Vec<&'static str> {
        let (bd, pt, lg) = self.resolved_counts();
        let mut ids = Vec::with_capacity((bd + pt + lg) as usize);
        for _ in 0..bd {
            ids.push("bearded_dragon");
        }
        for _ in 0..pt {
            ids.push("pond_turtle");
        }
        for _ in 0..lg {
            ids.push("leopard_gecko");
        }
        ids
    }
}

/// Deprecated global mode (`mode = "vivarium"` in old `user.toml`).
/// Kept so existing files still parse. Water-in-foreign-windows is no longer used.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Free,
    Vivarium,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct UserConfig {
    pub display: DisplayConfig,
    pub speech: SpeechConfig,
    pub weather: WeatherConfig,
    pub characters: CharactersConfig,
    pub vivarium: crate::vivarium::VivariumConfig,
    /// Ignored at runtime except as a one-shot enable hint when `[vivarium]` is absent.
    #[serde(default)]
    pub mode: Mode,
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

/// Engine-written Vivarium geometry. Preserves comments and other keys.
pub fn write_vivarium_geometry(origin_x: f64, origin_y: f64, logical_w: u32, logical_h: u32) {
    patch_vivarium(|table| {
        table["origin_x"] = toml_edit::value(origin_x);
        table["origin_y"] = toml_edit::value(origin_y);
        table["logical_w"] = toml_edit::value(logical_w as i64);
        table["logical_h"] = toml_edit::value(logical_h as i64);
    });
}

pub fn write_vivarium_z_order(z: crate::vivarium::ZOrder) {
    patch_vivarium(|table| {
        table["z_order"] = toml_edit::value(z.as_str());
    });
}

fn patch_vivarium(edit: impl FnOnce(&mut toml_edit::Table)) {
    let Some(path) = user_config_path() else { return };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => DEFAULT_TOML.to_string(),
    };
    let mut doc: toml_edit::DocumentMut = match src.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[user_config] patch: parse failed: {e}");
            return;
        }
    };
    if !doc.as_table().contains_key("vivarium") {
        doc["vivarium"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let Some(table) = doc["vivarium"].as_table_mut() else {
        eprintln!("[user_config] patch: [vivarium] is not a table");
        return;
    };
    edit(table);
    let tmp = path.with_extension("toml.tmp");
    if let Err(e) = std::fs::write(&tmp, doc.to_string()) {
        eprintln!("[user_config] patch: write failed: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        eprintln!("[user_config] patch: rename failed: {e}");
    }
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

const DEFAULT_TOML: &str = r##"[display]
sprite_size       = 150   # character size in pixels
font_size         = 14    # speech bubble font size in points
tick_rate_ac      = 30    # animation fps on AC power: 10, 15, 30, or 60
tick_rate_battery = 15    # animation fps on battery: 10, 15, 30, or 60
# language  = "os"   # "os" (default), "en", or "ja"

[speech]
enabled         = true    # set to false to silence all characters
speech_lock_sec = 30.0    # minimum seconds between speeches (global)

[weather]
enabled = true
# city = "Tokyo"   # uncomment and set your city name

[characters]
bearded_dragon = 1   # startup count per species (0–5); all zero → 1 bearded dragon
pond_turtle    = 1
leopard_gecko  = 1

[vivarium]
enabled         = true    # show the glass-cage home window
z_order         = "normal"  # "desktop" (behind windows), "normal", or "front"
display_scale   = 0.5
sprite_size     = 150     # size at display_scale 1.0; same formula as display.sprite_size
logical_w       = 960     # snapped to 16 px
logical_h       = 640
# origin_x / origin_y are written by dragging the cage (native screen points)

[vivarium.back]
kind    = "gradient"   # "solid" or "gradient"
color   = "#000000"    # default black: opacity darkens, it does not wash white
color2  = "#000000"
opacity = 0.0          # 0 = glass-only ND; raise to dim the back further
"##;

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
        Ok(mut cfg) => {
            apply_legacy_mode(&mut cfg, &text);
            cfg.vivarium.clamp();
            cfg
        }
        Err(e) => {
            eprintln!("[user_config] failed to parse user.toml: {e}");
            warn_config_load_failed(&path, &format!("{e}"));
            UserConfig::default()
        }
    }
}

fn apply_legacy_mode(cfg: &mut UserConfig, text: &str) {
    if !text.contains("[vivarium]") && cfg.mode == Mode::Vivarium {
        cfg.vivarium.enabled = true;
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
                 設定ウィンドウで内容を直してください。"
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
                 or open Settings to fix the file."
            ),
        )
    };
    show_native_error(title, &message);
}

/// Path to the Settings binary bundled next to the main executable.
pub fn settings_exe_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    #[cfg(target_os = "windows")]
    let name = "Settings.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "settings";
    let path = dir.join(name);
    path.exists().then_some(path)
}

/// Launches the settings window for `user.toml`.
pub fn launch_settings_ui() {
    let Some(settings) = settings_exe_path() else {
        show_settings_error(
            "Settings Unavailable",
            "Settings was not found next to the application.\n\
             Reinstall Petit Mates.",
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

    let start_watcher = {
        let mut active = SETTINGS_WATCHER_ACTIVE.lock().unwrap();
        if *active {
            false
        } else {
            *active = true;
            true
        }
    };

    let mut child = match Command::new(&settings).arg(&path).spawn() {
        Ok(c) => c,
        Err(e) => {
            if start_watcher {
                *SETTINGS_WATCHER_ACTIVE.lock().unwrap() = false;
            }
            show_settings_error(
                "Settings Unavailable",
                &format!("Failed to launch Settings:\n{e}"),
            );
            return;
        }
    };

    if start_watcher {
        std::thread::spawn(move || {
            let _ = child.wait();
            *SETTINGS_WATCHER_ACTIVE.lock().unwrap() = false;
            restart_after_settings_closed();
        });
    }
}

/// Relaunch the main app after the settings window exits (reloads `user.toml` at startup).
fn restart_after_settings_closed() {
    #[cfg(target_os = "macos")]
    {
        // Main thread spawns the replacement and removes the status item before exit.
        // Do not use exec() here — same PID breaks Control Center menu bar registration.
        request_main_loop_exit();
        return;
    }

    #[cfg(target_os = "windows")]
    {
        let Ok(exe) = std::env::current_exe() else {
            eprintln!("[user_config] restart: could not resolve current_exe");
            request_main_loop_exit();
            return;
        };
        let args: Vec<String> = std::env::args().skip(1).collect();
        match Command::new(&exe).env(RESTART_ENV, "1").args(&args).spawn() {
            Ok(_) => request_main_loop_exit(),
            Err(e) => {
                eprintln!("[user_config] restart spawn failed: {e}");
                request_main_loop_exit();
            }
        }
    }
}

fn request_main_loop_exit() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_default_is_one_each() {
        let cfg = CharactersConfig::default();
        assert_eq!(cfg.resolved_counts(), (1, 1, 1));
        assert_eq!(
            cfg.startup_species_ids(),
            vec!["bearded_dragon", "pond_turtle", "leopard_gecko"]
        );
    }

    #[test]
    fn characters_missing_section_uses_defaults() {
        let cfg: UserConfig = toml::from_str(
            r#"[display]
sprite_size = 150
font_size = 14
"#,
        )
        .unwrap();
        assert_eq!(cfg.characters.resolved_counts(), (1, 1, 1));
    }

    #[test]
    fn characters_custom_counts() {
        let cfg: CharactersConfig = toml::from_str(
            r#"bearded_dragon = 2
pond_turtle = 0
leopard_gecko = 1"#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_counts(), (2, 0, 1));
        assert_eq!(
            cfg.startup_species_ids(),
            vec!["bearded_dragon", "bearded_dragon", "leopard_gecko"]
        );
    }

    #[test]
    fn characters_all_zero_fallback() {
        let cfg: CharactersConfig = toml::from_str(
            r#"bearded_dragon = 0
pond_turtle = 0
leopard_gecko = 0"#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_counts(), (1, 0, 0));
        assert_eq!(cfg.startup_species_ids(), vec!["bearded_dragon"]);
    }

    #[test]
    fn characters_clamp_manual_overflow() {
        let cfg: CharactersConfig = toml::from_str(
            r#"bearded_dragon = 99
pond_turtle = 0
leopard_gecko = 0"#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_counts(), (5, 0, 0));
        assert_eq!(cfg.startup_species_ids().len(), 5);
    }

    #[test]
    fn characters_accepts_float_values() {
        let cfg: CharactersConfig = toml::from_str(
            r#"bearded_dragon = 2.0
pond_turtle = 0.0
leopard_gecko = 1.0"#,
        )
        .unwrap();
        assert_eq!(cfg.resolved_counts(), (2, 0, 1));
    }
}
