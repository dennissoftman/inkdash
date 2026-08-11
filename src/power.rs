use anyhow::{Context, Result};
use esp_idf_svc::sys::{self, EspError};

const MAX_CPU_FREQUENCY_MHZ: i32 = 160;
const IDLE_CPU_FREQUENCY_MHZ: i32 = 40;

/// Enables dynamic frequency scaling while keeping native USB continuously
/// available. ESP-IDF peripheral drivers take power-management locks whenever
/// SPI, I2S, Wi-Fi, or other timing-sensitive work needs a faster clock.
pub fn configure_dynamic_frequency_scaling() -> Result<()> {
    let config = sys::esp_pm_config_t {
        max_freq_mhz: MAX_CPU_FREQUENCY_MHZ,
        min_freq_mhz: IDLE_CPU_FREQUENCY_MHZ,
        // Native USB Serial/JTAG is an always-available control interface, so
        // automatic light sleep would be a feature regression here.
        light_sleep_enable: false,
    };
    EspError::convert(unsafe {
        sys::esp_pm_configure((&raw const config).cast::<core::ffi::c_void>())
    })
    .context("configuring ESP32-S3 dynamic frequency scaling")?;
    log::info!(
        "Power management enabled: CPU {IDLE_CPU_FREQUENCY_MHZ}-{MAX_CPU_FREQUENCY_MHZ} MHz, light sleep off for USB"
    );
    Ok(())
}
