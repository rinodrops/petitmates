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

/// Shared, cheaply-cloneable handle to the latest weather snapshot.
#[derive(Clone)]
pub struct WeatherHandle(Arc<Mutex<Option<WeatherInfo>>>);

impl WeatherHandle {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// Returns the latest weather, or `None` if not yet fetched.
    pub fn get(&self) -> Option<WeatherInfo> {
        self.0.lock().ok()?.clone()
    }

    fn set(&self, info: WeatherInfo) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some(info);
        }
    }
}

// ---- Background thread ----

/// Spawn the weather background thread and return the shared handle.
///
/// If `cfg.enabled` is `false`, or if no location can be resolved, the handle
/// will remain `None` forever (weather triggers are silently suppressed).
pub fn spawn(cfg: &crate::user_config::WeatherConfig) -> WeatherHandle {
    let handle = WeatherHandle::new();

    if !cfg.enabled {
        return handle;
    }

    // Resolve coordinates and start the fetch loop inside a background thread
    // so the main thread is not blocked by the geocoding HTTP request.
    let cfg_bg = cfg.clone();
    let handle_bg = handle.clone();
    std::thread::Builder::new()
        .name("weather-fetch".into())
        .spawn(move || {
            let lat_lon: Option<(f64, f64)> = match (cfg_bg.latitude, cfg_bg.longitude) {
                (Some(lat), Some(lon)) => Some((lat, lon)),
                _ => cfg_bg.city.as_deref().and_then(geocode),
            };
            let (lat, lon) = match lat_lon {
                Some(v) => v,
                None => {
                    eprintln!("[weather] no location configured; weather triggers disabled");
                    return;
                }
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
            Ok(info) => handle.set(info),
            Err(e)   => eprintln!("[weather] fetch failed: {e}"),
        }
        let elapsed = tick_start.elapsed();
        if elapsed < INTERVAL {
            std::thread::sleep(INTERVAL - elapsed);
        }
    }
}

// ---- Geocoding ----

fn geocode(city: &str) -> Option<(f64, f64)> {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        urlencoded(city)
    );
    let body: serde_json::Value = ureq::get(&url)
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    let result = body.get("results")?.get(0)?;
    let lat = result.get("latitude")?.as_f64()?;
    let lon = result.get("longitude")?.as_f64()?;
    Some((lat, lon))
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
