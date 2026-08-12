use crate::config;
use crate::datetime::DateTime;
use crate::power::PowerSource;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Notification {
    ChargeSoon,
    ChargeCritical,
}

impl Notification {
    const fn repeat_minutes(self) -> u8 {
        match self {
            Self::ChargeSoon => config::WARNING_REPEAT_MINUTES,
            Self::ChargeCritical => config::CRITICAL_REPEAT_MINUTES,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::ChargeSoon => "charge-soon",
            Self::ChargeCritical => "charge-critical",
        }
    }
}

/// Turns minute-level battery observations into semantic notification events.
/// Audio, display, or other consumers decide how to present those events.
pub struct BatteryNotificationSchedule {
    active: Option<Notification>,
    minutes_until_repeat: u8,
}

impl BatteryNotificationSchedule {
    pub const fn new() -> Self {
        Self {
            active: None,
            minutes_until_repeat: 0,
        }
    }

    pub fn on_minute(
        &mut self,
        time: Option<DateTime>,
        battery_percent: Option<u8>,
        power_source: PowerSource,
    ) -> Option<Notification> {
        if !time.is_some_and(|time| is_alert_hour(time.hour)) {
            self.reset();
            return None;
        }

        if power_source.is_external() {
            self.reset();
            return None;
        }

        let Some(percent) = battery_percent else {
            self.reset();
            return None;
        };
        let notification = notification_for_percent(percent, self.active);
        let Some(notification) = notification else {
            self.reset();
            return None;
        };

        if self.active != Some(notification) {
            self.active = Some(notification);
            self.minutes_until_repeat = notification.repeat_minutes();
            return Some(notification);
        }

        if self.minutes_until_repeat > 1 {
            self.minutes_until_repeat -= 1;
            None
        } else {
            self.minutes_until_repeat = notification.repeat_minutes();
            Some(notification)
        }
    }

    fn reset(&mut self) {
        self.active = None;
        self.minutes_until_repeat = 0;
    }
}

const fn is_alert_hour(hour: u8) -> bool {
    hour >= config::ALERT_START_HOUR && hour < config::ALERT_END_HOUR
}

const fn notification_for_percent(
    percent: u8,
    active: Option<Notification>,
) -> Option<Notification> {
    if percent <= config::CRITICAL_BATTERY_PERCENT
        || (matches!(active, Some(Notification::ChargeCritical))
            && percent <= config::CRITICAL_BATTERY_CLEAR_PERCENT)
    {
        Some(Notification::ChargeCritical)
    } else if percent <= config::WARNING_BATTERY_PERCENT
        || (matches!(active, Some(Notification::ChargeSoon))
            && percent <= config::WARNING_BATTERY_CLEAR_PERCENT)
    {
        Some(Notification::ChargeSoon)
    } else {
        None
    }
}
