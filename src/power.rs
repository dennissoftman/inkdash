use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use esp_idf_svc::sys::{self, EspError};

/// Uptime in seconds at the most recent automatic light sleep, or `NEVER`.
/// Seconds rather than the timer's native microseconds because the Xtensa
/// target has no 64-bit atomics, and this only feeds a once-a-minute badge.
static LAST_LIGHT_SLEEP_S: AtomicU32 = AtomicU32::new(NEVER);
const NEVER: u32 = u32::MAX;
/// Twice the dashboard's one-minute cadence, so a refresh that happens to land
/// in a busy stretch does not blink the badge off.
const ECO_EVIDENCE_WINDOW_S: u32 = 120;

fn uptime_seconds() -> u32 {
    (unsafe { sys::esp_timer_get_time() } / 1_000_000).max(0) as u32
}

unsafe extern "C" fn on_light_sleep_exit(
    _sleep_time_us: i64,
    _arg: *mut core::ffi::c_void,
) -> sys::esp_err_t {
    // Runs in the idle task, so this stays to a single relaxed store.
    LAST_LIGHT_SLEEP_S.store(uptime_seconds(), Ordering::Relaxed);
    sys::ESP_OK
}

/// Record when the CPU actually enters automatic light sleep.
///
/// Registration is independent of whether light sleep is currently permitted:
/// `esp_pm_light_sleep_register_cbs` is compiled unconditionally, and the
/// callback simply never fires while the policy keeps the CPU awake. That makes
/// the eco badge evidence of real sleep rather than of configuration.
pub fn track_light_sleep() -> Result<()> {
    let mut callbacks = sys::esp_pm_sleep_cbs_register_config_t {
        enter_cb: None,
        exit_cb: Some(on_light_sleep_exit),
        enter_cb_user_arg: core::ptr::null_mut(),
        exit_cb_user_arg: core::ptr::null_mut(),
        enter_cb_prior: 0,
        exit_cb_prior: 0,
    };
    EspError::convert(unsafe { sys::esp_pm_light_sleep_register_cbs(&mut callbacks) })
        .context("registering light sleep callbacks")?;
    Ok(())
}

/// True while the CPU has recently entered automatic light sleep.
///
/// Deliberately not what the eco badge reports: light sleep is disabled, so
/// this is always false right now. It is kept wired up because it is the
/// evidence that will show sleep working once the hang off USB is fixed, and
/// it is logged with every refresh so that is visible without a rebuild.
/// Reading it is idempotent.
pub fn slept_recently() -> bool {
    let last = LAST_LIGHT_SLEEP_S.load(Ordering::Relaxed);
    if last == NEVER {
        return false;
    }
    uptime_seconds().saturating_sub(last) < ECO_EVIDENCE_WINDOW_S
}

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
        // Automatic light sleep hangs this board once it actually engages: on
        // battery the dashboard stopped updating and the board reset on
        // reconnect. It is only ever exercised off USB, because the USJ
        // connection lock holds the CPU awake whenever a host is attached,
        // which is why every tethered test passed. Left off until the hang is
        // understood; the tickless idle and callback plumbing stay, since they
        // are inert without this and the eco badge depends on them.
        light_sleep_enable: false,
    };
    EspError::convert(unsafe {
        sys::esp_pm_configure((&raw const config).cast::<core::ffi::c_void>())
    })
    .context("configuring ESP32-S3 dynamic frequency scaling")?;
    log::info!(
        "Power policy: source={}, CPU {IDLE_CPU_FREQUENCY_MHZ}-{max_freq_mhz} MHz, automatic light sleep off",
        source.label()
    );
    Ok(())
}
