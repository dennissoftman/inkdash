pub mod backdrops;
mod style;
pub mod updates;
mod widgets;

use std::sync::Arc;

use embedded_graphics::pixelcolor::BinaryColor;

use crate::battery::BatteryStatus;
use crate::datetime::DateTime;
use crate::ink_stacks::Framebuffer;
use crate::language::Language;
use crate::ota;
use crate::power::PowerSource;
use crate::shtc3::ClimateReading;
use crate::weather::Weather;

pub struct DashboardData {
    pub time: Option<DateTime>,
    pub climate: Option<ClimateReading>,
    pub battery: BatteryStatus,
    pub power_source: PowerSource,
    pub wifi_connected: bool,
    pub wifi_ssid: Option<String>,
    pub wifi_signal_dbm: Option<i8>,
    pub weather: Option<Arc<Weather>>,
    pub language: Language,
    /// A background check found a newer release. Installing it is still a
    /// deliberate act: BOOT+PWR, then BOOT to accept.
    pub update_available: bool,
}

impl DashboardData {
    /// Everything unknown until the first sensor read. Fields are updated in
    /// place from then on, so state that is not read from hardware — the
    /// language, and anything added later — survives a refresh.
    pub const fn new(language: Language) -> Self {
        Self {
            time: None,
            climate: None,
            battery: BatteryStatus::unavailable(),
            power_source: PowerSource::Battery,
            wifi_connected: false,
            wifi_ssid: None,
            wifi_signal_dbm: None,
            weather: None,
            language,
            update_available: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardScreen {
    Home,
    Today,
    Forecast,
}

impl DashboardScreen {
    pub const fn next(self) -> Self {
        match self {
            Self::Home => Self::Today,
            Self::Today => Self::Forecast,
            Self::Forecast => Self::Home,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Home => Self::Forecast,
            Self::Today => Self::Home,
            Self::Forecast => Self::Today,
        }
    }
}

pub fn render(
    framebuffer: &mut Framebuffer,
    data: &DashboardData,
    screen: DashboardScreen,
    update_screen: &updates::Screen,
) {
    framebuffer.clear(BinaryColor::Off);
    if update_screen.is_visible() {
        updates::render(framebuffer, update_screen, ota::CURRENT_VERSION);
        return;
    }
    widgets::render(framebuffer, data, screen);
}

/// The scene the clock card is currently drawing, for logging and diagnostics.
pub fn scene(data: &DashboardData) -> (Option<backdrops::Slot>, backdrops::Sky) {
    (
        data.time
            .map(|value| backdrops::Slot::from_hour(value.hour)),
        widgets::sky_for(data.weather.as_deref()),
    )
}
