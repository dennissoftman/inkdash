use anyhow::{bail, Context, Result};

use crate::datetime::DateTime;
use crate::i2c_bus::I2cBus;

const ADDRESS: u8 = 0x51;
const OFFSET_REGISTER: u8 = 0x02;
const SECONDS_REGISTER: u8 = 0x04;
const NORMAL_MODE_PPM_PER_STEP: f32 = 4.34;

#[derive(Clone, Copy, Debug)]
pub struct RtcCalibration {
    pub offset_steps: i8,
    pub correction_ppm: f32,
    pub fast_mode: bool,
}

pub struct Rtc;

impl Rtc {
    pub const fn new() -> Self {
        Self
    }

    pub fn read(&self, i2c: &mut I2cBus<'_>) -> Result<DateTime> {
        let mut registers = [0_u8; 7];
        i2c.write_read(ADDRESS, &[SECONDS_REGISTER], &mut registers)
            .context("reading PCF85063 RTC")?;

        if registers[0] & 0x80 != 0 {
            bail!("RTC clock integrity flag is set; set the time over USB");
        }

        let value = DateTime {
            year: 2000 + bcd_to_binary(registers[6])? as u16,
            month: bcd_to_binary(registers[5] & 0x1f)?,
            day: bcd_to_binary(registers[3] & 0x3f)?,
            hour: bcd_to_binary(registers[2] & 0x3f)?,
            minute: bcd_to_binary(registers[1] & 0x7f)?,
            second: bcd_to_binary(registers[0] & 0x7f)?,
        };
        value.validate().context("RTC contains an invalid date")?;
        Ok(value)
    }

    pub fn set(&self, i2c: &mut I2cBus<'_>, value: DateTime) -> Result<()> {
        value.validate()?;
        let bytes = [
            SECONDS_REGISTER,
            binary_to_bcd(value.second),
            binary_to_bcd(value.minute),
            binary_to_bcd(value.hour),
            binary_to_bcd(value.day),
            value.weekday(),
            binary_to_bcd(value.month),
            binary_to_bcd((value.year % 100) as u8),
        ];
        i2c.write(ADDRESS, &bytes).context("writing PCF85063 RTC")
    }

    pub fn calibration(&self, i2c: &mut I2cBus<'_>) -> Result<RtcCalibration> {
        let mut value = [0_u8; 1];
        i2c.write_read(ADDRESS, &[OFFSET_REGISTER], &mut value)
            .context("reading PCF85063 offset calibration")?;
        let fast_mode = value[0] & 0x80 != 0;
        let raw_steps = value[0] & 0x7f;
        let offset_steps = if raw_steps & 0x40 != 0 {
            raw_steps as i16 - 128
        } else {
            raw_steps as i16
        } as i8;
        let ppm_per_step = if fast_mode { 4.069 } else { 4.34 };
        Ok(RtcCalibration {
            offset_steps,
            // A positive register offset subtracts pulses to slow a fast RTC.
            correction_ppm: -(offset_steps as f32) * ppm_per_step,
            fast_mode,
        })
    }

    /// Compensates a measured RTC drift. Positive drift means the RTC gains
    /// time; negative drift means it loses time.
    pub fn calibrate(
        &self,
        i2c: &mut I2cBus<'_>,
        measured_drift_ppm: f32,
    ) -> Result<RtcCalibration> {
        if !measured_drift_ppm.is_finite() {
            bail!("measured RTC drift must be finite");
        }
        let steps = (measured_drift_ppm / NORMAL_MODE_PPM_PER_STEP).round() as i16;
        if !(-64..=63).contains(&steps) {
            bail!("measured RTC drift is outside the correctable range");
        }
        let encoded_steps = (steps as i8 as u8) & 0x7f;
        // MODE=0 is the lower-power correction mode and adjusts every 2 hours.
        i2c.write(ADDRESS, &[OFFSET_REGISTER, encoded_steps])
            .context("writing PCF85063 offset calibration")?;
        self.calibration(i2c)
    }
}

fn binary_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn bcd_to_binary(value: u8) -> Result<u8> {
    let low = value & 0x0f;
    let high = value >> 4;
    if low > 9 || high > 9 {
        bail!("invalid BCD value 0x{value:02x}");
    }
    Ok(high * 10 + low)
}
