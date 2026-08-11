use anyhow::{bail, Context, Result};
use esp_idf_hal::delay::FreeRtos;

use crate::i2c_bus::I2cBus;

const ADDRESS: u8 = 0x70;
const WAKEUP: [u8; 2] = [0x35, 0x17];
const SLEEP: [u8; 2] = [0xb0, 0x98];
const SOFT_RESET: [u8; 2] = [0x80, 0x5d];
const READ_ID: [u8; 2] = [0xef, 0xc8];
const MEASURE_T_RH_POLLING: [u8; 2] = [0x78, 0x66];

// Waveshare's board example compensates for heat produced inside the enclosure.
const BOARD_TEMPERATURE_OFFSET_C: f32 = -4.0;

#[derive(Clone, Copy, Debug)]
pub struct ClimateReading {
    pub temperature_c: f32,
    pub humidity_percent: f32,
}

pub struct Shtc3;

impl Shtc3 {
    pub const fn new() -> Self {
        Self
    }

    pub fn initialize(&self, i2c: &mut I2cBus<'_>) -> Result<u16> {
        self.wake(i2c)?;
        i2c.write(ADDRESS, &SOFT_RESET).context("resetting SHTC3")?;
        FreeRtos::delay_ms(20);

        let mut response = [0_u8; 3];
        i2c.write_read(ADDRESS, &READ_ID, &mut response)
            .context("reading SHTC3 ID")?;
        check_crc(&response[..2], response[2])?;
        let id = u16::from_be_bytes([response[0], response[1]]);
        self.sleep(i2c)?;
        Ok(id)
    }

    pub fn read(&self, i2c: &mut I2cBus<'_>) -> Result<ClimateReading> {
        self.wake(i2c)?;
        let result = self.read_awake(i2c);
        if let Err(error) = self.sleep(i2c) {
            log::warn!("Could not put SHTC3 to sleep: {error:#}");
        }
        result
    }

    fn read_awake(&self, i2c: &mut I2cBus<'_>) -> Result<ClimateReading> {
        i2c.write(ADDRESS, &MEASURE_T_RH_POLLING)
            .context("starting SHTC3 measurement")?;
        FreeRtos::delay_ms(20);

        let mut response = [0_u8; 6];
        i2c.read(ADDRESS, &mut response)
            .context("reading SHTC3 measurement")?;
        check_crc(&response[..2], response[2]).context("temperature CRC")?;
        check_crc(&response[3..5], response[5]).context("humidity CRC")?;

        let raw_temperature = u16::from_be_bytes([response[0], response[1]]);
        let raw_humidity = u16::from_be_bytes([response[3], response[4]]);
        Ok(ClimateReading {
            temperature_c: -45.0
                + 175.0 * raw_temperature as f32 / 65_536.0
                + BOARD_TEMPERATURE_OFFSET_C,
            humidity_percent: 100.0 * raw_humidity as f32 / 65_536.0,
        })
    }

    fn wake(&self, i2c: &mut I2cBus<'_>) -> Result<()> {
        i2c.write(ADDRESS, &WAKEUP).context("waking SHTC3")?;
        FreeRtos::delay_ms(1);
        Ok(())
    }

    fn sleep(&self, i2c: &mut I2cBus<'_>) -> Result<()> {
        i2c.write(ADDRESS, &SLEEP).context("sleeping SHTC3")
    }
}

fn check_crc(data: &[u8], expected: u8) -> Result<()> {
    let mut crc = 0xff_u8;
    for byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    if crc != expected {
        bail!("CRC mismatch: expected 0x{expected:02x}, calculated 0x{crc:02x}");
    }
    Ok(())
}
