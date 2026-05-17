//! Weather fetching from Open-Meteo.
//!
//! Runs a background thread that:
//! 1. Geocodes `user.toml [weather] city` → latitude / longitude (once).
//! 2. Fetches current weather every hour and stores it in a shared `WeatherHandle`.
//!
//! The speech engine reads `WeatherHandle` each tick; if the cache is empty
//! (fetch never succeeded) weather triggers are suppressed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- Public types ----

/// Simplified weather category matching `speech.toml` trigger values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeatherCategory {
    Sunny,
    Cloudy,
    Rainy,
    Snowy,
}

impl WeatherCategory {
    /// Maps a WMO weather code to a category.
    pub fn from_wmo(code: u16) -> Option<Self> {
        match code {
            0..=1             => Some(Self::Sunny),
            2..=3             => Some(Self::Cloudy),
            51..=67 | 80..=82 => Some(Self::Rainy),
            71..=77           => Some(Self::Snowy),
            _                 => None,
        }
    }

    /// String key as used in `speech.toml` `weather = ["sunny", ...]`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sunny  => "sunny",
            Self::Cloudy => "cloudy",
            Self::Rainy  => "rainy",
            Self::Snowy  => "snowy",
        }
    }
}

/// Current weather snapshot stored in the shared handle.
#[derive(Clone, Debug)]
pub struct WeatherInfo {
    pub category: WeatherCategory,
    pub temp_c: f64,
}

/// Geocoding resolution status exposed via [`WeatherHandle::geo_status`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeoStatus {
    /// Geocoding request is in flight.
    Resolving,
    /// Coordinates were successfully resolved (or supplied directly).
    Ok,
    /// The city name returned no matching results.
    NotFound,
    /// A network or parse error occurred during geocoding.
    Unavailable,
}

/// Combined internal state of the weather handle.
struct WeatherState {
    weather: Option<WeatherInfo>,
    geo: GeoStatus,
}

/// Shared, cheaply-cloneable handle to the latest weather snapshot.
#[derive(Clone)]
pub struct WeatherHandle(Arc<Mutex<WeatherState>>);

impl WeatherHandle {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(WeatherState {
            weather: None,
            geo: GeoStatus::Resolving,
        })))
    }

    /// Returns the latest weather, or `None` if not yet fetched.
    pub fn get(&self) -> Option<WeatherInfo> {
        self.0.lock().ok()?.weather.clone()
    }

    /// Returns the current geocoding resolution status.
    pub fn geo_status(&self) -> GeoStatus {
        self.0.lock().ok()
            .map(|g| g.geo.clone())
            .unwrap_or(GeoStatus::Unavailable)
    }

    fn set_weather(&self, info: WeatherInfo) {
        if let Ok(mut guard) = self.0.lock() {
            guard.weather = Some(info);
        }
    }

    fn set_geo(&self, status: GeoStatus) {
        if let Ok(mut guard) = self.0.lock() {
            guard.geo = status;
        }
    }
}

// ---- Background thread ----

/// Spawn the weather background thread and return the shared handle.
///
/// If `cfg.enabled` is `false`, or if no location can be resolved, the handle
/// will have `geo_status() == GeoStatus::Unavailable` and `get()` returns `None`
/// forever (weather triggers are silently suppressed).
pub fn spawn(cfg: &crate::user_config::WeatherConfig) -> WeatherHandle {
    let handle = WeatherHandle::new();

    if !cfg.enabled {
        handle.set_geo(GeoStatus::Unavailable);
        return handle;
    }

    // If lat/lon are supplied directly, mark geo as resolved immediately so
    // the status menu can show "✓" without waiting for the background thread.
    if cfg.latitude.is_some() && cfg.longitude.is_some() {
        handle.set_geo(GeoStatus::Ok);
    }

    // Resolve coordinates and start the fetch loop inside a background thread
    // so the main thread is not blocked by the geocoding HTTP request.
    let cfg_bg = cfg.clone();
    let handle_bg = handle.clone();
    std::thread::Builder::new()
        .name("weather-fetch".into())
        .spawn(move || {
            let lat_lon: Option<(f64, f64)> = match (cfg_bg.latitude, cfg_bg.longitude) {
                (Some(lat), Some(lon)) => {
                    // Already marked Ok before the thread started.
                    Some((lat, lon))
                }
                _ => match cfg_bg.city.as_deref() {
                    Some(city) => match geocode_detailed(city) {
                        GeoResult::Ok(lat, lon) => {
                            handle_bg.set_geo(GeoStatus::Ok);
                            Some((lat, lon))
                        }
                        GeoResult::NotFound => {
                            eprintln!("[weather] city '{}' not found", city);
                            handle_bg.set_geo(GeoStatus::NotFound);
                            None
                        }
                        GeoResult::Unavailable => {
                            eprintln!("[weather] geocoding unavailable");
                            handle_bg.set_geo(GeoStatus::Unavailable);
                            None
                        }
                    },
                    None => {
                        eprintln!("[weather] no location configured; weather triggers disabled");
                        handle_bg.set_geo(GeoStatus::Unavailable);
                        None
                    }
                },
            };
            let (lat, lon) = match lat_lon {
                Some(v) => v,
                None    => return,
            };
            run_loop(handle_bg, lat, lon);
        })
        .expect("failed to spawn weather thread");

    handle
}

/// Refresh loop: fetch immediately, then every hour.
fn run_loop(handle: WeatherHandle, lat: f64, lon: f64) {
    const INTERVAL: Duration = Duration::from_secs(3600);
    loop {
        let tick_start = Instant::now();
        match fetch_weather(lat, lon) {
            Ok(info) => handle.set_weather(info),
            Err(e)   => eprintln!("[weather] fetch failed: {e}"),
        }
        let elapsed = tick_start.elapsed();
        if elapsed < INTERVAL {
            std::thread::sleep(INTERVAL - elapsed);
        }
    }
}

// ---- Geocoding ----

/// Detailed geocoding result distinguishing failure modes.
enum GeoResult {
    Ok(f64, f64),
    NotFound,
    Unavailable,
}

fn geocode_detailed(city: &str) -> GeoResult {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        urlencoded(city)
    );
    let body: serde_json::Value = match ureq::get(&url).call() {
        Ok(mut resp) => match resp.body_mut().read_json() {
            Ok(v)  => v,
            Err(_) => return GeoResult::Unavailable,
        },
        Err(_) => return GeoResult::Unavailable,
    };
    let results = match body.get("results").and_then(|r| r.as_array()) {
        Some(r) if !r.is_empty() => r,
        _ => return GeoResult::NotFound,
    };
    let result = &results[0];
    let lat = match result.get("latitude").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None    => return GeoResult::Unavailable,
    };
    let lon = match result.get("longitude").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None    => return GeoResult::Unavailable,
    };
    GeoResult::Ok(lat, lon)
}

// ---- Weather fetch ----

fn fetch_weather(lat: f64, lon: f64) -> Result<WeatherInfo, Box<dyn std::error::Error>> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast\
         ?latitude={lat:.4}&longitude={lon:.4}\
         &current=weather_code,temperature_2m\
         &forecast_days=1"
    );
    let body: serde_json::Value = ureq::get(&url)
        .call()?
        .body_mut()
        .read_json()?;
    let current = body.get("current")
        .ok_or("missing 'current'")?;
    let code = current.get("weather_code")
        .and_then(|v| v.as_u64())
        .ok_or("missing weather_code")? as u16;
    let temp_c = current.get("temperature_2m")
        .and_then(|v| v.as_f64())
        .ok_or("missing temperature_2m")?;
    let category = WeatherCategory::from_wmo(code)
        .ok_or_else(|| format!("unknown WMO code {code}"))?;
    Ok(WeatherInfo { category, temp_c })
}

// ---- Utility ----

/// Percent-encode a plain ASCII city name for URL use.
fn urlencoded(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                vec![c]
            } else if c == ' ' {
                vec!['+']
            } else {
                // Percent-encode non-ASCII / special chars (basic, covers city names).
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes().to_owned();
                bytes.iter()
                    .flat_map(|b| {
                        let hi = char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase();
                        let lo = char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase();
                        vec!['%', hi, lo]
                    })
                    .collect::<Vec<char>>()
            }
        })
        .collect()
}
