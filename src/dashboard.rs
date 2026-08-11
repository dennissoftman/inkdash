mod widgets;

use std::sync::Arc;

use crate::battery::BatteryStatus;
use crate::datetime::DateTime;
use crate::ink_stacks::Framebuffer;
use crate::shtc3::ClimateReading;
use crate::weather::Weather;

pub struct DashboardData {
    pub time: Option<DateTime>,
    pub climate: Option<ClimateReading>,
    pub battery: BatteryStatus,
    pub wifi_connected: bool,
    pub wifi_ssid: Option<String>,
    pub wifi_signal_dbm: Option<i8>,
    pub weather: Option<Arc<Weather>>,
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

pub fn render(framebuffer: &mut Framebuffer, data: &DashboardData, screen: DashboardScreen) {
    widgets::render(framebuffer, data, screen);
}
