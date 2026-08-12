use std::borrow::Borrow;

use anyhow::Result;
use esp_idf_hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_hal::adc::AdcChannel;
use esp_idf_hal::delay::Ets;

const DIVIDER_MULTIPLIER: f32 = 2.0;
const PRESENT_THRESHOLD_V: f32 = 2.7;

#[derive(Clone, Copy, Debug)]
pub struct BatteryReading {
    pub voltage_v: f32,
    pub percent: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct BatteryStatus {
    pub reading: Option<BatteryReading>,
}

impl BatteryStatus {
    pub const fn unavailable() -> Self {
        Self { reading: None }
    }
}

pub struct Battery<'d, C, M>
where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    channel: AdcChannelDriver<'d, C, M>,
}

impl<'d, C, M> Battery<'d, C, M>
where
    C: AdcChannel,
    M: Borrow<AdcDriver<'d, C::AdcUnit>>,
{
    pub fn new(channel: AdcChannelDriver<'d, C, M>) -> Self {
        Self { channel }
    }

    pub fn read(&mut self) -> Result<BatteryStatus> {
        const SAMPLE_COUNT: u32 = 16;
        // Samples are spaced with a busy-wait, not `FreeRtos::delay_ms`: the
        // 100 Hz tick rounds any sub-tick sleep up to a full 10 ms tick, which
        // turned a 30 ms sampling window into more than a tenth of a second of
        // blocked event loop on every minute boundary.
        let mut millivolts = 0_u32;
        for sample in 0..SAMPLE_COUNT {
            if sample > 0 {
                Ets::delay_us(200);
            }
            millivolts += self.channel.read()? as u32;
        }

        let voltage_v = millivolts as f32 / SAMPLE_COUNT as f32 / 1000.0 * DIVIDER_MULTIPLIER;
        if voltage_v < PRESENT_THRESHOLD_V {
            return Ok(BatteryStatus::unavailable());
        }

        Ok(BatteryStatus {
            reading: Some(BatteryReading {
                voltage_v,
                percent: voltage_to_percent(voltage_v),
            }),
        })
    }
}

// Coarse one-cell LiPo resting-voltage curve. Charge percentage is intentionally
// presented as an estimate because this board has voltage sensing, not a fuel gauge.
fn voltage_to_percent(voltage: f32) -> u8 {
    const CURVE: [(f32, u8); 11] = [
        (3.30, 0),
        (3.55, 10),
        (3.68, 20),
        (3.74, 30),
        (3.77, 40),
        (3.79, 50),
        (3.82, 60),
        (3.87, 70),
        (3.92, 80),
        (4.03, 90),
        (4.20, 100),
    ];

    if voltage <= CURVE[0].0 {
        return 0;
    }
    for pair in CURVE.windows(2) {
        let (low_v, low_percent) = pair[0];
        let (high_v, high_percent) = pair[1];
        if voltage <= high_v {
            let fraction = (voltage - low_v) / (high_v - low_v);
            return (low_percent as f32 + fraction * (high_percent - low_percent) as f32).round()
                as u8;
        }
    }
    100
}
