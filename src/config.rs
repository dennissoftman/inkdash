use std::time::Duration;

// Operational policy. Values with `option_env!` can be overridden at build time
// without editing firmware source.
pub const IP_LOCATION_ENDPOINT: &str = match option_env!("IP_LOCATION_ENDPOINT") {
    Some(value) => value,
    None => "https://ipwho.is/",
};
pub const WEATHER_ENDPOINT: &str = match option_env!("WEATHER_ENDPOINT") {
    Some(value) => value,
    None => "https://api.open-meteo.com/v1/forecast",
};
pub const NTP_SERVER: &str = match option_env!("NTP_SERVER") {
    Some(value) => value,
    None => "0.pool.ntp.org",
};
/// Settings live in the 24 KB `nvs` partition alongside ESP-IDF's own Wi-Fi
/// data, so only small values belong there: identifiers, coordinates, and short
/// strings. Bulk data such as artwork, audio, or logs needs its own storage.
pub const NVS_NAMESPACE: &str = "dashboard";

pub const WEATHER_REFRESH_INTERVAL: Duration = Duration::from_secs(value_or_default(
    option_env!("WEATHER_REFRESH_SECONDS"),
    60 * 60,
));
pub const WEATHER_RETRY_INTERVAL: Duration = Duration::from_secs(value_or_default(
    option_env!("WEATHER_RETRY_SECONDS"),
    5 * 60,
));
pub const WIFI_RECONNECT_INTERVAL: Duration = Duration::from_secs(value_or_default(
    option_env!("WIFI_RECONNECT_SECONDS"),
    5 * 60,
));
pub const FULL_REFRESH_AFTER_PARTIALS: u8 = checked_u8(value_or_default(
    option_env!("FULL_REFRESH_AFTER_PARTIALS"),
    30,
));

pub const HTTP_TIMEOUT: Duration =
    Duration::from_secs(value_or_default(option_env!("HTTP_TIMEOUT_SECONDS"), 15));
pub const HTTP_RESPONSE_LIMIT: usize = checked_usize(value_or_default(
    option_env!("HTTP_RESPONSE_LIMIT_BYTES"),
    4096,
));
pub const BACKGROUND_TASK_STACK_SIZE: usize = checked_usize(value_or_default(
    option_env!("BACKGROUND_TASK_STACK_SIZE"),
    12 * 1024,
));
pub const INPUT_TASK_STACK_SIZE: usize =
    checked_usize(value_or_default(option_env!("INPUT_TASK_STACK_SIZE"), 4096));
pub const BUTTON_DEBOUNCE_MS: u32 =
    checked_u32(value_or_default(option_env!("BUTTON_DEBOUNCE_MS"), 30));
pub const BUTTON_LONG_PRESS: Duration = Duration::from_secs(value_or_default(
    option_env!("BUTTON_LONG_PRESS_SECONDS"),
    2,
));
pub const IMMEDIATE_EVENT_DELAY: Duration = Duration::from_millis(1);
pub const CLOCK_RETRY_INTERVAL: Duration = Duration::from_secs(5);
pub const OTA_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(value_or_default(
    option_env!("OTA_CONFIRMATION_SECONDS"),
    60,
));
// An unattended device would otherwise never notice a release, since the only
// other trigger is the BOOT+PWR chord. The check is silent and never takes over
// the panel: finding an update only raises a badge in the status bar.
pub const OTA_CHECK_INTERVAL: Duration = Duration::from_secs(value_or_default(
    option_env!("OTA_CHECK_SECONDS"),
    24 * 60 * 60,
));
/// Long enough after boot for Wi-Fi, NTP, and the first weather fetch to settle.
pub const OTA_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(value_or_default(
    option_env!("OTA_CHECK_STARTUP_SECONDS"),
    60,
));
pub const OTA_RESTART_DELAY: Duration =
    Duration::from_secs(value_or_default(option_env!("OTA_RESTART_SECONDS"), 3));
// Both are per-read socket timeouts rather than limits on a whole transfer. A
// manifest is a few hundred bytes, so it can be brief; a firmware image is
// megabytes over Wi-Fi that may stall, and aborting that costs the whole
// download.
pub const OTA_MANIFEST_TIMEOUT: Duration = Duration::from_secs(value_or_default(
    option_env!("OTA_MANIFEST_TIMEOUT_SECONDS"),
    10,
));
pub const OTA_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(value_or_default(
    option_env!("OTA_DOWNLOAD_TIMEOUT_SECONDS"),
    60,
));

pub const ALERT_START_HOUR: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_ALERT_START_HOUR"),
    10,
));
pub const ALERT_END_HOUR: u8 =
    checked_u8(value_or_default(option_env!("BATTERY_ALERT_END_HOUR"), 22));
pub const WARNING_BATTERY_PERCENT: u8 =
    checked_u8(value_or_default(option_env!("BATTERY_WARNING_PERCENT"), 25));
pub const CRITICAL_BATTERY_PERCENT: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_CRITICAL_PERCENT"),
    10,
));
pub const WARNING_BATTERY_CLEAR_PERCENT: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_WARNING_CLEAR_PERCENT"),
    28,
));
pub const CRITICAL_BATTERY_CLEAR_PERCENT: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_CRITICAL_CLEAR_PERCENT"),
    12,
));
pub const WARNING_REPEAT_MINUTES: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_WARNING_REPEAT_MINUTES"),
    30,
));
pub const CRITICAL_REPEAT_MINUTES: u8 = checked_u8(value_or_default(
    option_env!("BATTERY_CRITICAL_REPEAT_MINUTES"),
    5,
));
pub const TOMORROW_FORECAST_HOURS: [usize; 4] = [8, 12, 18, 23];

const fn value_or_default(value: Option<&str>, default: u64) -> u64 {
    let Some(value) = value else {
        return default;
    };
    let bytes = value.as_bytes();
    assert!(
        !bytes.is_empty(),
        "build-time configuration cannot be empty"
    );
    let mut parsed = 0_u64;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        assert!(
            byte >= b'0' && byte <= b'9',
            "configuration must be an integer"
        );
        parsed = match parsed.checked_mul(10) {
            Some(value) => value,
            None => panic!("configuration integer overflow"),
        };
        parsed = match parsed.checked_add((byte - b'0') as u64) {
            Some(value) => value,
            None => panic!("configuration integer overflow"),
        };
        index += 1;
    }
    assert!(parsed > 0, "configuration must be greater than zero");
    parsed
}

const fn checked_u8(value: u64) -> u8 {
    assert!(value <= u8::MAX as u64, "configuration exceeds u8 range");
    value as u8
}

const fn checked_usize(value: u64) -> usize {
    assert!(
        value <= usize::MAX as u64,
        "configuration exceeds usize range"
    );
    value as usize
}

const fn checked_u32(value: u64) -> u32 {
    assert!(value <= u32::MAX as u64, "configuration exceeds u32 range");
    value as u32
}

const _: () = {
    assert!(ALERT_START_HOUR < 24 && ALERT_END_HOUR <= 24);
    assert!(CRITICAL_BATTERY_PERCENT < WARNING_BATTERY_PERCENT);
    assert!(CRITICAL_BATTERY_CLEAR_PERCENT >= CRITICAL_BATTERY_PERCENT);
    assert!(WARNING_BATTERY_CLEAR_PERCENT >= WARNING_BATTERY_PERCENT);
    assert!(WARNING_BATTERY_CLEAR_PERCENT <= 100);
};
