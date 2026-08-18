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

/// Uptime in seconds when the battery node last showed the board taking a
/// charge, or `NEVER`. Maintained by [`ChargeDetector`].
///
/// Evidence rather than a latched state, because the two directions are not
/// equally visible. A charge announces itself: the node jumps 57-89 mV when the
/// charger is applied and climbs while current flows. Its removal can be
/// almost silent -- measured at 7 mV on a nearly full cell, where the charge
/// current has already tapered and there is barely any to remove. Latching on
/// the loud edge and waiting for the quiet one to clear it left the bolt stuck
/// on. Letting the evidence expire instead fails toward battery, which is both
/// the safer default and the true one whenever a charge has finished.
static LAST_CHARGE_EVIDENCE_S: AtomicU32 = AtomicU32::new(NEVER);
/// Long enough to ride out the constant-voltage tail, where the climb slows to
/// a crawl, without keeping the bolt lit long after a cable is gone.
const CHARGE_EVIDENCE_WINDOW_S: u32 = 15 * 60;

/// Return the board's current power source.
///
/// Two independent signals, because neither covers everything. ESP-IDF's USB
/// SOF monitor is definitive when it fires, but it reports a USB *host*: a wall
/// adapter supplies VBUS and never sends a SOF packet, so it reads as battery.
/// The battery node covers exactly that gap. Reading either is free -- the SOF
/// state is a cached flag and the node state is sampled elsewhere -- so this
/// never waits on traffic or touches the ADC.
pub fn source() -> PowerSource {
    if unsafe { sys::usb_serial_jtag_is_connected() } || charging_recently() {
        PowerSource::ExternalPower
    } else {
        PowerSource::Battery
    }
}

/// Two medians of three. Thirty seconds at the sampling interval, which is
/// short enough that the badge follows a plug within one dashboard refresh.
const STEP_WINDOW: usize = 6;
/// Comfortably under the smallest transition measured on this board (57 mV,
/// on a nearly full cell where the step is weakest) and far above the ~3 mV
/// per-sample noise.
const STEP_MV: i32 = 30;
/// A charge climbs 3-5 mV per minute through the flat middle of the discharge
/// curve, so ten minutes moves the node well past this. Discharge is an order
/// of magnitude slower and is deliberately not inferred from trend.
const TREND_MV: i32 = 25;
const TREND_SAMPLES: u16 = 120;

/// Infers external power from the battery node, for the wall-adapter case the
/// USB SOF flag cannot see.
///
/// Applying or removing external power moves the node in a single step: the
/// charger reverses the cell's IR drop and the board's own load transfers off
/// the cell. Measured at 57-89 mV on this board between 3.7 V and 4.15 V.
///
/// The step is compared between medians rather than means so that one sagging
/// sample -- an e-paper refresh or a Wi-Fi burst pulling the rail down while
/// the ADC happens to read -- is discarded instead of averaged in.
///
/// A slow rise is accepted as a second, positive-only signal. It covers booting
/// already on a charger, where there is no edge to catch. The reverse is not
/// inferred: discharge is far too slow to separate from drift over any window
/// worth waiting for, so leaving external power is only ever concluded from a
/// step.
pub struct ChargeDetector {
    window: [u16; STEP_WINDOW],
    len: usize,
    trend_reference: u16,
    trend_samples: u16,
}

impl ChargeDetector {
    pub const fn new() -> Self {
        Self {
            window: [0; STEP_WINDOW],
            len: 0,
            trend_reference: 0,
            trend_samples: 0,
        }
    }

    /// Feed one battery-node reading in millivolts.
    pub fn sample(&mut self, millivolts: f32) {
        let millivolts = millivolts.round().clamp(0.0, f32::from(u16::MAX)) as u16;

        // Deliberately not stamped from the USB flag. `source` already treats an
        // attached host as external on its own, and stamping here would keep
        // the evidence alive for the whole expiry window after the cable came
        // out, leaving the bolt lit on a board running from its battery. A
        // charge taken from a host still registers through the node itself, so
        // a swap from host to adapter carries over on real evidence.

        if self.len < STEP_WINDOW {
            self.window[self.len] = millivolts;
            self.len += 1;
        } else {
            self.window.rotate_left(1);
            self.window[STEP_WINDOW - 1] = millivolts;
        }

        if self.len == STEP_WINDOW {
            let older = median_of_three(&self.window[..STEP_WINDOW / 2]);
            let newer = median_of_three(&self.window[STEP_WINDOW / 2..]);
            let step = i32::from(newer) - i32::from(older);
            if step >= STEP_MV {
                note_charging("step up");
            } else if step <= -STEP_MV {
                note_charge_gone("step down");
            }
        }

        if self.trend_samples == 0 {
            self.trend_reference = millivolts;
        }
        self.trend_samples += 1;
        if self.trend_samples > TREND_SAMPLES {
            if i32::from(millivolts) - i32::from(self.trend_reference) >= TREND_MV {
                note_charging("sustained rise");
            }
            self.trend_reference = millivolts;
            self.trend_samples = 1;
        }
    }
}

/// The middle of three, which discards a lone outlier instead of averaging it
/// in: `min(max(a,b), max(a,c), max(b,c))`.
fn median_of_three(values: &[u16]) -> u16 {
    let (a, b, c) = (values[0], values[1], values[2]);
    a.max(b).min(a.max(c)).min(b.max(c))
}

/// True while the node has shown a charge recently enough to still believe it.
fn charging_recently() -> bool {
    let last = LAST_CHARGE_EVIDENCE_S.load(Ordering::Relaxed);
    if last == NEVER {
        return false;
    }
    uptime_seconds().saturating_sub(last) < CHARGE_EVIDENCE_WINDOW_S
}

fn note_charging(evidence: &str) {
    if !charging_recently() {
        log::info!("Battery node shows a charge ({evidence})");
    }
    LAST_CHARGE_EVIDENCE_S.store(uptime_seconds(), Ordering::Relaxed);
}

fn note_charge_gone(evidence: &str) {
    if LAST_CHARGE_EVIDENCE_S.swap(NEVER, Ordering::Relaxed) != NEVER {
        log::info!("Battery node shows no charge ({evidence})");
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
