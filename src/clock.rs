//! A displayed clock that keeps running when the RTC stops answering.
//!
//! This is a wall clock: a minute that is slightly wrong is far more useful than
//! `--:--`. Every successful read is kept along with the uptime it was taken at,
//! so a failed read still yields a time by advancing the last one. Only a clock
//! that has never been read successfully leaves the panel blank.
//!
//! The console reads the hardware directly instead, because a diagnostic that
//! answers with an estimate is worse than one that reports the failure.

use std::time::Instant;

use anyhow::Result;

use crate::datetime::DateTime;
use crate::i2c_bus::I2cBus;
use crate::rtc::{Rtc, RtcCalibration};

pub struct Clock {
    rtc: Rtc,
    last: Option<(DateTime, Instant)>,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            rtc: Rtc::new(),
            last: None,
        }
    }

    /// The time to display: the RTC when it answers, otherwise the last reading
    /// advanced by the uptime since it was taken.
    pub fn now(&mut self, i2c: &mut I2cBus<'_>) -> Option<DateTime> {
        match self.rtc.read(i2c) {
            Ok(time) => {
                self.last = Some((time, Instant::now()));
                Some(time)
            }
            Err(error) => match self.estimate() {
                Some(time) => {
                    log::warn!(
                        "RTC read failed; showing {time} carried from the last read: {error:#}"
                    );
                    Some(time)
                }
                None => {
                    log::warn!("RTC read failed with nothing to carry forward: {error:#}");
                    None
                }
            },
        }
    }

    /// A direct hardware read, for the console.
    pub fn read_rtc(&self, i2c: &mut I2cBus<'_>) -> Result<DateTime> {
        self.rtc.read(i2c)
    }

    pub fn set(&mut self, i2c: &mut I2cBus<'_>, value: DateTime) -> Result<()> {
        self.rtc.set(i2c, value)?;
        self.last = Some((value, Instant::now()));
        Ok(())
    }

    pub fn calibration(&self, i2c: &mut I2cBus<'_>) -> Result<RtcCalibration> {
        self.rtc.calibration(i2c)
    }

    pub fn calibrate(
        &self,
        i2c: &mut I2cBus<'_>,
        measured_drift_ppm: f32,
    ) -> Result<RtcCalibration> {
        self.rtc.calibrate(i2c, measured_drift_ppm)
    }

    /// The last reading plus the elapsed uptime. `None` before the first read,
    /// and past the end of the supported year range.
    fn estimate(&self) -> Option<DateTime> {
        let (time, taken) = self.last?;
        let elapsed = i64::try_from(taken.elapsed().as_secs()).ok()?;
        let stamp = u64::try_from(time.as_local_seconds().checked_add(elapsed)?).ok()?;
        DateTime::from_unix_seconds(stamp, 0).ok()
    }
}
