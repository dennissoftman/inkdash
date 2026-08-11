use anyhow::{Context, Result};
use esp_idf_svc::sys::{self, EspError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerSource {
    Battery,
    ExternalPower,
}

impl PowerSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Battery => "BAT",
            Self::ExternalPower => "PWR",
        }
    }

    pub const fn is_external(self) -> bool {
        matches!(self, Self::ExternalPower)
    }
}

/// Return ESP-IDF's cached USB host state as the board's current power source.
///
/// The board does not expose USB VBUS or the charger's status output to a GPIO.
/// `usb_serial_jtag_is_connected()` only reads a flag maintained by ESP-IDF's
/// USB SOF monitor, so this function does not wait for traffic or touch the ADC.
pub fn source() -> PowerSource {
    if unsafe { sys::usb_serial_jtag_is_connected() } {
        PowerSource::ExternalPower
    } else {
        PowerSource::Battery
    }
}

const EXTERNAL_POWER_MAX_CPU_FREQUENCY_MHZ: i32 = 160;
// 80 MHz is ESP-IDF's lowest supported production CPU ceiling on ESP32-S3.
// DFS can still select the 40 MHz crystal whenever no PM lock is held.
const BATTERY_MAX_CPU_FREQUENCY_MHZ: i32 = 80;
const IDLE_CPU_FREQUENCY_MHZ: i32 = 40;

pub struct PowerPolicy {
    source: PowerSource,
}

impl PowerPolicy {
    pub fn initialize() -> Result<Self> {
        let source = source();
        configure_dynamic_frequency_scaling(source)?;
        Ok(Self { source })
    }

    /// Reconcile the CPU policy with ESP-IDF's cached USB connection state.
    ///
    /// Call this while handling an existing application event. It performs no
    /// polling and reconfigures DFS only when the source actually changed.
    pub fn refresh(&mut self) -> Result<Option<PowerSource>> {
        let observed = source();
        if observed == self.source {
            return Ok(None);
        }

        configure_dynamic_frequency_scaling(observed)?;
        let previous = self.source;
        self.source = observed;
        log::info!(
            "Power source changed: {} -> {}",
            previous.label(),
            observed.label()
        );
        Ok(Some(observed))
    }
}

fn configure_dynamic_frequency_scaling(source: PowerSource) -> Result<()> {
    let max_freq_mhz = match source {
        PowerSource::Battery => BATTERY_MAX_CPU_FREQUENCY_MHZ,
        PowerSource::ExternalPower => EXTERNAL_POWER_MAX_CPU_FREQUENCY_MHZ,
    };
    let config = sys::esp_pm_config_t {
        max_freq_mhz,
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
        "Power policy: source={}, CPU {IDLE_CPU_FREQUENCY_MHZ}-{max_freq_mhz} MHz, light sleep off for USB",
        source.label()
    );
    Ok(())
}
