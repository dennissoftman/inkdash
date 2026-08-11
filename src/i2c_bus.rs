use std::marker::PhantomData;
use std::ptr;

use anyhow::{bail, Result};
use esp_idf_hal::delay::Ets;
use esp_idf_hal::gpio::{Gpio47, Gpio48, PinDriver, Pull};
use esp_idf_hal::i2c::I2C0;
use esp_idf_svc::sys::{self, EspError};

const TRANSFER_TIMEOUT_MS: i32 = 1_000;
const BUS_FREQUENCY_HZ: u32 = 300_000;

/// Rust owner for ESP-IDF 5's current I2C master-bus API.
///
/// esp-idf-hal 0.46 still exposes ESP-IDF's legacy driver, while Waveshare's
/// ESP-IDF 5.5 examples use this newer API for the onboard RTC and SHTC3.
pub struct I2cBus<'d> {
    bus: sys::i2c_master_bus_handle_t,
    codec: sys::i2c_master_dev_handle_t,
    rtc: sys::i2c_master_dev_handle_t,
    shtc3: sys::i2c_master_dev_handle_t,
    _peripherals: PhantomData<(&'d mut I2C0<'d>, &'d mut Gpio47<'d>, &'d mut Gpio48<'d>)>,
}

impl<'d> I2cBus<'d> {
    pub fn new(_i2c: I2C0<'d>, sda: Gpio47<'d>, scl: Gpio48<'d>) -> Result<Self> {
        recover_bus(sda, scl)?;

        let mut bus_config = sys::i2c_master_bus_config_t {
            i2c_port: sys::i2c_port_t_I2C_NUM_0 as _,
            sda_io_num: 47,
            scl_io_num: 48,
            __bindgen_anon_1: sys::i2c_master_bus_config_t__bindgen_ty_1 {
                clk_source: sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT,
            },
            glitch_ignore_cnt: 7,
            ..Default::default()
        };
        bus_config.flags.set_enable_internal_pullup(1);

        let mut driver = Self {
            bus: ptr::null_mut(),
            codec: ptr::null_mut(),
            rtc: ptr::null_mut(),
            shtc3: ptr::null_mut(),
            _peripherals: PhantomData,
        };
        EspError::convert(unsafe { sys::i2c_new_master_bus(&bus_config, &mut driver.bus) })?;
        driver.codec = driver.add_device(0x18)?;
        driver.rtc = driver.add_device(0x51)?;
        driver.shtc3 = driver.add_device(0x70)?;
        Ok(driver)
    }

    pub fn probe(&self, address: u8) -> Result<()> {
        EspError::convert(unsafe {
            sys::i2c_master_probe(self.bus, address as u16, TRANSFER_TIMEOUT_MS)
        })?;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<()> {
        EspError::convert(unsafe { sys::i2c_master_bus_reset(self.bus) })?;
        Ok(())
    }

    pub fn line_levels(&self) -> (i32, i32) {
        unsafe { (sys::gpio_get_level(47), sys::gpio_get_level(48)) }
    }

    pub fn write(&mut self, address: u8, bytes: &[u8]) -> Result<()> {
        let device = self.device(address)?;
        EspError::convert(unsafe {
            sys::i2c_master_transmit(device, bytes.as_ptr(), bytes.len(), TRANSFER_TIMEOUT_MS)
        })?;
        Ok(())
    }

    pub fn read(&mut self, address: u8, buffer: &mut [u8]) -> Result<()> {
        let device = self.device(address)?;
        EspError::convert(unsafe {
            sys::i2c_master_receive(
                device,
                buffer.as_mut_ptr(),
                buffer.len(),
                TRANSFER_TIMEOUT_MS,
            )
        })?;
        Ok(())
    }

    pub fn write_read(&mut self, address: u8, bytes: &[u8], buffer: &mut [u8]) -> Result<()> {
        let device = self.device(address)?;
        EspError::convert(unsafe {
            sys::i2c_master_transmit_receive(
                device,
                bytes.as_ptr(),
                bytes.len(),
                buffer.as_mut_ptr(),
                buffer.len(),
                TRANSFER_TIMEOUT_MS,
            )
        })?;
        Ok(())
    }

    fn add_device(&self, address: u16) -> Result<sys::i2c_master_dev_handle_t> {
        let config = sys::i2c_device_config_t {
            dev_addr_length: sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7,
            device_address: address,
            scl_speed_hz: BUS_FREQUENCY_HZ,
            ..Default::default()
        };
        let mut device = ptr::null_mut();
        EspError::convert(unsafe {
            sys::i2c_master_bus_add_device(self.bus, &config, &mut device)
        })?;
        Ok(device)
    }

    fn device(&self, address: u8) -> Result<sys::i2c_master_dev_handle_t> {
        match address {
            0x18 => Ok(self.codec),
            0x51 => Ok(self.rtc),
            0x70 => Ok(self.shtc3),
            _ => bail!("I2C address 0x{address:02x} is not registered"),
        }
    }
}

fn recover_bus(sda: Gpio47<'_>, scl: Gpio48<'_>) -> Result<()> {
    let mut sda = PinDriver::input_output_od(sda, Pull::Up)?;
    let mut scl = PinDriver::input_output_od(scl, Pull::Up)?;
    sda.set_high()?;
    scl.set_high()?;
    Ets::delay_us(10);

    let initial_sda = sda.is_high();
    let initial_scl = scl.is_high();
    log::info!(
        "I2C levels before manual recovery: SDA={}, SCL={}",
        u8::from(initial_sda),
        u8::from(initial_scl)
    );

    if !initial_sda && initial_scl {
        // A slave may have been reset between ACK and STOP. Clock enough bits
        // for it to finish its byte, then generate an explicit STOP condition.
        for _ in 0..18 {
            scl.set_low()?;
            Ets::delay_us(10);
            scl.set_high()?;
            Ets::delay_us(10);
            if sda.is_high() {
                break;
            }
        }
        sda.set_low()?;
        Ets::delay_us(10);
        scl.set_high()?;
        Ets::delay_us(10);
        sda.set_high()?;
        Ets::delay_us(10);
    }

    log::info!(
        "I2C levels after manual recovery: SDA={}, SCL={}",
        u8::from(sda.is_high()),
        u8::from(scl.is_high())
    );
    Ok(())
}

impl Drop for I2cBus<'_> {
    fn drop(&mut self) {
        unsafe {
            if !self.codec.is_null() {
                let _ = sys::i2c_master_bus_rm_device(self.codec);
            }
            if !self.rtc.is_null() {
                let _ = sys::i2c_master_bus_rm_device(self.rtc);
            }
            if !self.shtc3.is_null() {
                let _ = sys::i2c_master_bus_rm_device(self.shtc3);
            }
            if !self.bus.is_null() {
                let _ = sys::i2c_del_master_bus(self.bus);
            }
        }
    }
}
