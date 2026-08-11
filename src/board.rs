use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Output, OutputPin, PinDriver};
use esp_idf_svc::sys::{self, EspError};

/// Keeps the board's VBAT/peripheral power rail enabled.
///
/// Waveshare's reference firmware drives GPIO17 high before accessing the RTC
/// and SHTC3. The pin driver is retained so the rail remains enabled.
pub struct BoardPower<'d> {
    _vbat_enable: PinDriver<'d, Output>,
    _audio_enable: PinDriver<'d, Output>,
}

impl<'d> BoardPower<'d> {
    pub fn enable(vbat_pin: impl OutputPin + 'd, audio_pin: impl OutputPin + 'd) -> Result<Self> {
        // The factory sleep demo can retain GPIO17 across deep sleep. Release
        // those holds before taking ownership and driving the rail high.
        unsafe {
            sys::gpio_deep_sleep_hold_dis();
            EspError::convert(sys::gpio_hold_dis(17))?;
        }
        let mut vbat_enable = PinDriver::output(vbat_pin)?;
        vbat_enable.set_high()?;
        // The onboard ES8311 codec shares GPIO47/48. When its active-low
        // power rail is off it clamps SDA low, blocking the RTC and SHTC3.
        let mut audio_enable = PinDriver::output(audio_pin)?;
        audio_enable.set_low()?;
        FreeRtos::delay_ms(100);
        Ok(Self {
            _vbat_enable: vbat_enable,
            _audio_enable: audio_enable,
        })
    }
}
