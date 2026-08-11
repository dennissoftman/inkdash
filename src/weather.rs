use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use anyhow::{bail, Context, Result};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use serde::Deserialize;

use crate::events::{AppEvent, EventSender};
use crate::location::{get_json, IpLocationProvider, Location, LocationStore};

const NVS_NAMESPACE: &str = "dashboard";
pub const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20 * 60);
pub const FAILURE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weather {
    pub temperature_c: f32,
    pub relative_humidity_percent: u8,
    pub weather_code: u16,
    pub today: DailyForecast,
    pub tomorrow: DailyForecast,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DailyForecast {
    pub weather_code: u16,
    pub minimum_temperature_c: f32,
    pub maximum_temperature_c: f32,
    pub precipitation_probability_percent: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WeatherKind {
    Sunny,
    Cloudy,
    Fog,
    Rain,
    Snow,
}

impl Weather {
    pub fn kind(self) -> WeatherKind {
        WeatherKind::from_code(self.weather_code)
    }

    pub fn condition(self) -> &'static str {
        condition(self.weather_code)
    }
}

impl DailyForecast {
    pub fn kind(self) -> WeatherKind {
        WeatherKind::from_code(self.weather_code)
    }

    pub fn condition(self) -> &'static str {
        condition(self.weather_code)
    }
}

impl WeatherKind {
    fn from_code(code: u16) -> Self {
        match code {
            0 => WeatherKind::Sunny,
            1..=3 => WeatherKind::Cloudy,
            45 | 48 => WeatherKind::Fog,
            51..=67 | 80..=82 | 95..=99 => WeatherKind::Rain,
            71..=77 | 85 | 86 => WeatherKind::Snow,
            _ => WeatherKind::Cloudy,
        }
    }
}

fn condition(code: u16) -> &'static str {
    match code {
        0 => "CLEAR",
        1..=3 => "CLOUDY",
        45 | 48 => "FOG",
        51..=57 => "DRIZZLE",
        61..=67 => "RAIN",
        71..=77 => "SNOW",
        80..=82 => "SHOWERS",
        85 | 86 => "SNOW SHWR",
        95..=99 => "STORM",
        _ => "UNKNOWN",
    }
}

#[derive(Deserialize)]
struct OpenMeteoResponse {
    current: CurrentWeather,
    daily: DailyWeather,
}

#[derive(Deserialize)]
struct CurrentWeather {
    temperature_2m: f64,
    relative_humidity_2m: u8,
    weather_code: u16,
}

#[derive(Deserialize)]
struct DailyWeather {
    weather_code: [u16; 2],
    temperature_2m_min: [f64; 2],
    temperature_2m_max: [f64; 2],
    precipitation_probability_max: [Option<u8>; 2],
}

pub fn fetch_weather(location: &Location) -> Result<Weather> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.6}&longitude={:.6}\
         &current=temperature_2m,relative_humidity_2m,weather_code\
         &daily=weather_code,temperature_2m_min,temperature_2m_max,precipitation_probability_max\
         &forecast_days=2&timezone=auto",
        location.latitude, location.longitude
    );
    let response: OpenMeteoResponse = get_json(&url).context("fetching Open-Meteo forecast")?;
    let weather = Weather {
        temperature_c: checked_temperature(response.current.temperature_2m, "current")?,
        relative_humidity_percent: response.current.relative_humidity_2m,
        weather_code: response.current.weather_code,
        today: daily_forecast(&response.daily, 0, "today")?,
        tomorrow: daily_forecast(&response.daily, 1, "tomorrow")?,
    };
    if weather.relative_humidity_percent > 100
        || [weather.today, weather.tomorrow].iter().any(|day| {
            day.precipitation_probability_percent
                .is_some_and(|value| value > 100)
                || day.minimum_temperature_c > day.maximum_temperature_c
        })
    {
        bail!("Open-Meteo returned invalid weather values");
    }
    Ok(weather)
}

fn daily_forecast(daily: &DailyWeather, index: usize, label: &str) -> Result<DailyForecast> {
    Ok(DailyForecast {
        weather_code: daily.weather_code[index],
        minimum_temperature_c: checked_temperature(
            daily.temperature_2m_min[index],
            &format!("{label} minimum"),
        )?,
        maximum_temperature_c: checked_temperature(
            daily.temperature_2m_max[index],
            &format!("{label} maximum"),
        )?,
        precipitation_probability_percent: daily.precipitation_probability_max[index],
    })
}

fn checked_temperature(value: f64, label: &str) -> Result<f32> {
    if !value.is_finite() || !(-150.0..=100.0).contains(&value) {
        bail!("Open-Meteo returned invalid {label} temperature");
    }
    Ok(value as f32)
}

pub type WorkerUpdate = Result<Weather, String>;

pub struct WeatherService {
    requests: SyncSender<()>,
    latest: Option<Weather>,
    in_flight: bool,
}

impl WeatherService {
    pub fn start(nvs_partition: EspDefaultNvsPartition, events: EventSender) -> Result<Self> {
        let (request_sender, request_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("weather".into())
            .stack_size(12 * 1024)
            .spawn(move || {
                let storage = match EspNvs::new(nvs_partition, NVS_NAMESPACE, true) {
                    Ok(storage) => storage,
                    Err(error) => {
                        let _ = events.send(AppEvent::WeatherCompleted(Err(format!(
                            "opening location NVS: {error}"
                        ))));
                        return;
                    }
                };
                let location_store = LocationStore::new(storage);
                let mut provider = IpLocationProvider;
                while request_receiver.recv().is_ok() {
                    let update = location_store
                        .get_location(&mut provider)
                        .and_then(|location| fetch_weather(&location))
                        .map_err(|error| format!("{error:#}"));
                    if events.send(AppEvent::WeatherCompleted(update)).is_err() {
                        break;
                    }
                }
            })
            .context("starting weather worker")?;

        Ok(Self {
            requests: request_sender,
            latest: None,
            in_flight: false,
        })
    }

    pub fn request(&mut self) -> bool {
        if self.in_flight {
            return true;
        }
        match self.requests.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {
                self.in_flight = true;
                true
            }
            Err(TrySendError::Disconnected(())) => false,
        }
    }

    pub fn complete(&mut self, update: WorkerUpdate) -> bool {
        self.in_flight = false;
        match update {
            Ok(weather) => {
                let changed = self.latest != Some(weather);
                self.latest = Some(weather);
                log::info!(
                    "Weather updated: {:.1} C, code {}, low {:.1} C, high {:.1} C",
                    weather.temperature_c,
                    weather.weather_code,
                    weather.today.minimum_temperature_c,
                    weather.today.maximum_temperature_c
                );
                changed
            }
            Err(error) => {
                log::warn!("Weather update failed; retaining last data: {error}");
                false
            }
        }
    }

    pub fn latest(&self) -> Option<Weather> {
        self.latest
    }
}
