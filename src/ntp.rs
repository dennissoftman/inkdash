use anyhow::Result;
use esp_idf_svc::sntp::{EspSntp, SntpConf};

use crate::config;
use crate::events::{AppEvent, EventSender};

/// Starts ESP-IDF's SNTP client. The polling interval is configured in
/// `sdkconfig.defaults`; the returned service must live for the application lifetime.
pub fn start(events: EventSender) -> Result<EspSntp<'static>> {
    let mut configuration = SntpConf::default();
    configuration.servers[0] = config::NTP_SERVER;
    let service = EspSntp::new_with_callback(&configuration, move |_| {
        let _ = events.send(AppEvent::NtpSynchronized);
    })?;
    Ok(service)
}
