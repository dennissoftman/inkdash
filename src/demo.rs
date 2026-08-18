//! A rehearsal mode for the panel: draw any screen on demand, from the USB
//! console, without the conditions that normally produce it.
//!
//! Most of the interface can be reached by waiting for the weather to change or
//! by pressing a button. The update screens cannot: they exist for a few seconds
//! during an install, and some of them only when one fails, which makes the one
//! thing they most need — how the panel *refreshes* between them — almost
//! impossible to study. The host preview in `tools/backdrop-preview` renders the
//! frames, but a frame is not a refresh.
//!
//! Nothing here talks to the OTA worker, the network, or the flash. Demo mode
//! only decides what the renderer is handed, and it lives in RAM, so a reset
//! ends it.

use std::sync::Arc;
use std::time::Duration;

use crate::battery::{BatteryReading, BatteryStatus};
use crate::dashboard::updates;
use crate::dashboard::DashboardData;
use crate::power::PowerSource;
use crate::shtc3::ClimateReading;
use crate::weather::{DailyForecast, ForecastPeriod, Weather, WeatherKind, WeatherLocation};

/// The version the scripted install pretends to fetch. Deliberately above
/// anything released, so a panel photographed mid-demo cannot be mistaken for a
/// real update.
const REPLAY_VERSION: &str = "9.9.9";
const REPLAY_TOTAL_BYTES: usize = 1_501_952;
const REPLAY_STEP: Duration = Duration::from_millis(1_500);
const REPLAY_STEP_LIMITS: std::ops::RangeInclusive<u64> = 100..=30_000;

#[derive(Clone, Debug)]
pub enum Action {
    Screen(updates::Screen),
    Install { total: usize, step: Duration },
    Data(DataOverride),
    Refresh(RefreshMode),
    Status,
    Off,
}

#[derive(Clone, Debug)]
pub enum DataOverride {
    /// `None` models a battery that cannot be read, which the widget draws as an
    /// empty cell rather than as zero.
    Battery(Option<u8>),
    Power(PowerSource),
    /// Zero bars models a disconnected radio; the label follows.
    Wifi {
        bars: u8,
        ssid: Option<String>,
    },
    Weather(Option<(WeatherKind, f32)>),
    Indoor(Option<ClimateReading>),
    Badge(bool),
    Clear,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RefreshMode {
    #[default]
    Auto,
    Full,
    Partial,
}

impl RefreshMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Full => "full",
            Self::Partial => "partial",
        }
    }
}

/// What the caller has to do after an action, so the event loop keeps ownership
/// of its timers and its render flags.
#[derive(Default)]
pub struct Outcome {
    pub reply: String,
    pub render: bool,
    pub stop_stepping: bool,
    pub step_after: Option<Duration>,
}

#[derive(Default)]
pub struct Demo {
    overrides: Overrides,
    refresh: RefreshMode,
    replay: Option<Replay>,
}

impl Demo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Demo mode can pin the refresh policy, which is how a candidate fix for
    /// the update-screen ghosting can be tried on hardware without reflashing.
    /// The first draw after boot stays full whatever is asked for: a partial
    /// update has no base image to differ against.
    pub const fn needs_full_refresh(&self, policy: bool, panel_initialized: bool) -> bool {
        match self.refresh {
            RefreshMode::Auto => policy,
            RefreshMode::Full => true,
            RefreshMode::Partial => !panel_initialized,
        }
    }

    /// The data to draw: the live reading with anything demo mode was told to
    /// pretend layered over it. `None` when nothing is overridden, so the normal
    /// path does not pay for the copy.
    pub fn overlay(&self, data: &DashboardData) -> Option<DashboardData> {
        if self.overrides.is_empty() {
            return None;
        }
        let mut copy = data.clone();
        if let Some(battery) = self.overrides.battery {
            copy.battery = battery;
        }
        if let Some(power_source) = self.overrides.power_source {
            copy.power_source = power_source;
        }
        if let Some(wifi) = self.overrides.wifi.as_ref() {
            copy.wifi_connected = wifi.bars > 0;
            copy.wifi_signal_dbm = wifi.signal_dbm();
            copy.wifi_ssid = wifi.ssid.clone();
        }
        if let Some(weather) = self.overrides.weather.as_ref() {
            copy.weather = weather.clone();
        }
        if let Some(climate) = self.overrides.climate {
            copy.climate = climate;
        }
        if let Some(badge) = self.overrides.badge {
            copy.update_available = badge;
        }
        Some(copy)
    }

    /// Runs an action. `screen` is the event loop's own update screen, so every
    /// button rule that already applies to a real update applies here too: PWR
    /// dismisses, and no check can start underneath a demo.
    pub fn handle(
        &mut self,
        action: Action,
        screen: &mut updates::Screen,
        ota_active: bool,
    ) -> Outcome {
        if ota_active {
            return Outcome {
                reply: "ERR a real update is in progress".to_owned(),
                ..Outcome::default()
            };
        }
        match action {
            Action::Screen(demo_screen) => {
                self.replay = None;
                let reply = format!("OK DEMO screen {}", describe(&demo_screen));
                *screen = demo_screen;
                Outcome {
                    reply,
                    render: true,
                    stop_stepping: true,
                    ..Outcome::default()
                }
            }
            Action::Install { total, step } => {
                let replay = Replay {
                    total,
                    step,
                    index: 0,
                };
                *screen = replay.screen(0).expect("a replay always has a first step");
                self.replay = Some(replay);
                Outcome {
                    reply: format!(
                        "OK DEMO install v{REPLAY_VERSION} {total} bytes, {} ms per step",
                        step.as_millis()
                    ),
                    render: true,
                    step_after: Some(step),
                    ..Outcome::default()
                }
            }
            Action::Data(over) => {
                let reply = self.overrides.set(over);
                Outcome {
                    reply,
                    render: true,
                    ..Outcome::default()
                }
            }
            Action::Refresh(mode) => {
                self.refresh = mode;
                Outcome {
                    reply: format!("OK DEMO refresh {}", mode.label()),
                    render: true,
                    ..Outcome::default()
                }
            }
            Action::Status => Outcome {
                reply: self.status(screen),
                ..Outcome::default()
            },
            Action::Off => {
                *self = Self::default();
                *screen = updates::Screen::Hidden;
                Outcome {
                    reply: "OK DEMO off".to_owned(),
                    render: true,
                    stop_stepping: true,
                    ..Outcome::default()
                }
            }
        }
    }

    /// One tick of the scripted install.
    pub fn step(&mut self, screen: &mut updates::Screen) -> Outcome {
        let Some(replay) = self.replay.as_mut() else {
            return Outcome::default();
        };
        replay.index += 1;
        let Some(next) = replay.screen(replay.index) else {
            self.replay = None;
            return Outcome {
                reply: "OK DEMO install finished".to_owned(),
                ..Outcome::default()
            };
        };
        let step = replay.step;
        *screen = next;
        Outcome {
            // Whether this step refreshes the whole panel is the event loop's
            // decision, exactly as it is for a real update. The rehearsal must
            // not have a refresh policy of its own.
            render: true,
            step_after: Some(step),
            ..Outcome::default()
        }
    }

    fn status(&self, screen: &updates::Screen) -> String {
        let replay = match self.replay.as_ref() {
            Some(replay) => format!("step {} of {}", replay.index, Replay::STEPS),
            None => "idle".to_owned(),
        };
        format!(
            "OK DEMO screen={} refresh={} install={} overrides={}",
            describe(screen),
            self.refresh.label(),
            replay,
            self.overrides.summary()
        )
    }
}

/// `None` leaves a field to the hardware. The nested options are the fields that
/// are themselves optional, where demo mode has to be able to model the reading
/// being absent as well as present.
#[derive(Default)]
struct Overrides {
    battery: Option<BatteryStatus>,
    power_source: Option<PowerSource>,
    wifi: Option<Wifi>,
    weather: Option<Option<Arc<Weather>>>,
    climate: Option<Option<ClimateReading>>,
    badge: Option<bool>,
}

impl Overrides {
    fn is_empty(&self) -> bool {
        self.battery.is_none()
            && self.power_source.is_none()
            && self.wifi.is_none()
            && self.weather.is_none()
            && self.climate.is_none()
            && self.badge.is_none()
    }

    fn set(&mut self, over: DataOverride) -> String {
        match over {
            DataOverride::Battery(percent) => {
                self.battery = Some(BatteryStatus {
                    reading: percent.map(|percent| BatteryReading {
                        // The widget bands the percentage, and shows the voltage
                        // nowhere, so a representative one is enough.
                        voltage_v: 3.3 + f32::from(percent) * 0.009,
                        percent,
                    }),
                });
                match percent {
                    Some(percent) => format!("OK DEMO battery {percent}%"),
                    None => "OK DEMO battery unavailable".to_owned(),
                }
            }
            DataOverride::Power(source) => {
                self.power_source = Some(source);
                format!("OK DEMO power {}", source.label())
            }
            DataOverride::Wifi { bars, ssid } => {
                let wifi = Wifi { bars, ssid };
                let reply = format!(
                    "OK DEMO wifi {} bar(s) {}",
                    wifi.bars,
                    wifi.ssid.as_deref().unwrap_or("(no name)")
                );
                self.wifi = Some(wifi);
                reply
            }
            DataOverride::Weather(None) => {
                self.weather = Some(None);
                "OK DEMO weather unavailable".to_owned()
            }
            DataOverride::Weather(Some((kind, temperature_c))) => {
                self.weather = Some(Some(Arc::new(sample_weather(kind, temperature_c))));
                format!("OK DEMO weather {kind:?} {temperature_c:.1} C")
            }
            DataOverride::Indoor(reading) => {
                self.climate = Some(reading);
                match reading {
                    Some(reading) => format!(
                        "OK DEMO indoor {:.1} C {:.0}%",
                        reading.temperature_c, reading.humidity_percent
                    ),
                    None => "OK DEMO indoor unavailable".to_owned(),
                }
            }
            DataOverride::Badge(shown) => {
                self.badge = Some(shown);
                format!("OK DEMO badge {}", if shown { "on" } else { "off" })
            }
            DataOverride::Clear => {
                *self = Self::default();
                "OK DEMO data cleared".to_owned()
            }
        }
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.battery.is_some() {
            parts.push("battery");
        }
        if self.power_source.is_some() {
            parts.push("power");
        }
        if self.wifi.is_some() {
            parts.push("wifi");
        }
        if self.weather.is_some() {
            parts.push("weather");
        }
        if self.climate.is_some() {
            parts.push("indoor");
        }
        if self.badge.is_some() {
            parts.push("badge");
        }
        if parts.is_empty() {
            return "none".to_owned();
        }
        parts.join(",")
    }
}

struct Wifi {
    bars: u8,
    ssid: Option<String>,
}

impl Wifi {
    /// Chosen to land in the middle of each band `wifi_signal_bars` divides the
    /// signal into, so the badge draws the number of bars that was asked for.
    fn signal_dbm(&self) -> Option<i8> {
        match self.bars {
            0 => None,
            1 => Some(-85),
            2 => Some(-68),
            _ => Some(-45),
        }
    }
}

struct Replay {
    total: usize,
    step: Duration,
    index: usize,
}

impl Replay {
    /// Check, offer, eleven progress reports, verification, and the restart
    /// notice.
    const STEPS: usize = 15;

    /// The real sequence, in the order and at the granularity a live update
    /// produces it: the manifest lookup, one report per ten percent, and the
    /// two screens either side of them.
    fn screen(&self, index: usize) -> Option<updates::Screen> {
        let version = REPLAY_VERSION.to_owned();
        match index {
            0 => Some(updates::Screen::Checking),
            1 => Some(updates::Screen::Available {
                version,
                size: self.total,
            }),
            2..=12 => {
                let percent = (index - 2) * 10;
                Some(updates::Screen::Downloading {
                    version,
                    downloaded: self.total * percent / 100,
                    total: self.total,
                })
            }
            13 => Some(updates::Screen::Finalizing { version }),
            14 => Some(updates::Screen::Restarting { version }),
            _ => None,
        }
    }
}

pub fn replay(total: Option<usize>, step_ms: Option<u64>) -> Result<Action, String> {
    let total = total.unwrap_or(REPLAY_TOTAL_BYTES);
    if total == 0 {
        return Err("install size must be more than zero bytes".to_owned());
    }
    let step_ms = step_ms.unwrap_or_else(|| REPLAY_STEP.as_millis() as u64);
    if !REPLAY_STEP_LIMITS.contains(&step_ms) {
        return Err(format!(
            "step must be between {} and {} ms",
            REPLAY_STEP_LIMITS.start(),
            REPLAY_STEP_LIMITS.end()
        ));
    }
    Ok(Action::Install {
        total,
        step: Duration::from_millis(step_ms),
    })
}

fn describe(screen: &updates::Screen) -> &'static str {
    match screen {
        updates::Screen::Hidden => "dashboard",
        updates::Screen::Checking => "checking",
        updates::Screen::Available { .. } => "available",
        updates::Screen::Downloading { .. } => "downloading",
        updates::Screen::Finalizing { .. } => "finalizing",
        updates::Screen::Restarting { .. } => "restarting",
        updates::Screen::UpToDate => "up-to-date",
        updates::Screen::Failed { .. } => "failed",
    }
}

/// A whole plausible reading built from the two things worth choosing. The
/// dashboard reads far more than it displays at once, and every page has to draw
/// something, so the rest is filled in rather than left blank.
fn sample_weather(kind: WeatherKind, temperature_c: f32) -> Weather {
    let weather_code = match kind {
        WeatherKind::Sunny => 0,
        WeatherKind::Cloudy => 3,
        WeatherKind::Fog => 45,
        WeatherKind::Rain => 61,
        WeatherKind::Snow => 71,
        WeatherKind::Storm => 95,
    };
    Weather {
        temperature_c,
        relative_humidity_percent: 55,
        weather_code,
        location: WeatherLocation::new("Demo", "XX"),
        today: DailyForecast {
            weather_code,
            mean_temperature_c: temperature_c,
            minimum_temperature_c: temperature_c - 4.0,
            maximum_temperature_c: temperature_c + 5.0,
            precipitation_probability_percent: Some(40),
            maximum_wind_speed_kmh: 12.0,
        },
        // Varied across the day, so the forecast page is not four identical rows.
        tomorrow_periods: std::array::from_fn(|index| ForecastPeriod {
            weather_code,
            temperature_c: temperature_c - 3.0 + index as f32 * 1.5,
            precipitation_probability_percent: Some(20 * index as u8),
        }),
    }
}
