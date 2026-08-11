use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

use anyhow::{bail, Result};
use embedded_graphics::primitives::Rectangle;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Input, Output, PinDriver};
use esp_idf_hal::spi::SpiSingleDeviceDriver;
use esp_idf_hal::task::block_on;
use esp_idf_svc::timer::EspTaskTimerService;

pub const WIDTH: usize = 200;
pub const HEIGHT: usize = 200;
pub const FRAMEBUFFER_SIZE: usize = WIDTH * HEIGHT / 8;

// Full-refresh waveform from Waveshare's ESP32-S3-ePaper-1.54 example.
const FULL_REFRESH_LUT: [u8; 159] = [
    0x80, 0x48, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x48, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80,
    0x48, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x48, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0x0a, 0, 0, 0, 0, 0, 0, 0x08, 0x01, 0, 0x08, 0x01, 0, 0x02, 0x0a, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0, 0, 0, 0x22, 0x17, 0x41, 0, 0x32, 0x20,
];

const PARTIAL_REFRESH_LUT: [u8; 159] = [
    0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x40, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0x0f, 0, 0, 0, 0, 0, 0, 0x01, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x22, 0x22, 0x22,
    0x22, 0x22, 0x22, 0, 0, 0, 0x02, 0x17, 0x41, 0xb0, 0x32, 0x28,
];

pub struct Epaper<'d> {
    spi: SpiSingleDeviceDriver<'d>,
    power: PinDriver<'d, Output>,
    busy: PinDriver<'d, Input>,
    reset: PinDriver<'d, Output>,
    dc: PinDriver<'d, Output>,
}

impl<'d> Epaper<'d> {
    pub fn new(
        spi: SpiSingleDeviceDriver<'d>,
        power: PinDriver<'d, Output>,
        busy: PinDriver<'d, Input>,
        reset: PinDriver<'d, Output>,
        dc: PinDriver<'d, Output>,
    ) -> Self {
        Self {
            spi,
            power,
            busy,
            reset,
            dc,
        }
    }

    pub fn init_full(&mut self) -> Result<()> {
        self.power.set_low()?;
        FreeRtos::delay_ms(20);

        self.hardware_reset()?;

        self.wait_until_idle(Duration::from_secs(5))?;
        self.command(0x12)?; // Software reset
        self.wait_until_idle(Duration::from_secs(5))?;

        self.command(0x01)?; // Driver output control
        self.data(&[0xc7, 0x00, 0x01])?;

        self.command(0x11)?; // X increments, Y decrements
        self.data(&[0x01])?;

        self.command(0x44)?; // RAM X start/end, in bytes
        self.data(&[0x00, ((WIDTH - 1) >> 3) as u8])?;

        self.command(0x45)?; // RAM Y start/end
        self.data(&[(HEIGHT - 1) as u8, 0x00, 0x00, 0x00])?;

        self.command(0x3c)?; // Border waveform
        self.data(&[0x01])?;
        self.command(0x18)?; // Internal temperature sensor
        self.data(&[0x80])?;

        self.command(0x22)?; // Load temperature and waveform settings
        self.data(&[0xb1])?;
        self.command(0x20)?;

        self.command(0x4e)?; // RAM X address counter
        self.data(&[0x00])?;
        self.command(0x4f)?; // RAM Y address counter
        self.data(&[(HEIGHT - 1) as u8, 0x00])?;
        self.wait_until_idle(Duration::from_secs(5))?;

        self.load_full_refresh_lut()
    }

    pub fn display_base(&mut self, framebuffer: &[u8]) -> Result<()> {
        validate_framebuffer(framebuffer)?;
        self.command(0x24)?; // Write black/white RAM
        self.data(framebuffer)?;
        self.command(0x26)?; // Seed previous-image RAM for partial updates
        self.data(framebuffer)?;
        self.command(0x22)?;
        self.data(&[0xc7])?;
        self.command(0x20)?; // Activate display update
        self.wait_until_idle(Duration::from_secs(10))
    }

    pub fn init_partial(&mut self) -> Result<()> {
        self.hardware_reset()?;
        self.wait_until_idle(Duration::from_secs(5))?;
        self.load_lut(&PARTIAL_REFRESH_LUT)?;

        self.command(0x37)?;
        self.data(&[0, 0, 0, 0, 0, 0x40, 0, 0, 0, 0])?;
        self.command(0x3c)?;
        self.data(&[0x80])?;
        self.command(0x22)?;
        self.data(&[0xc0])?;
        self.command(0x20)?;
        self.wait_until_idle(Duration::from_secs(5))
    }

    pub fn display_partial_windows(
        &mut self,
        framebuffer: &[u8],
        regions: &[Rectangle],
    ) -> Result<()> {
        validate_framebuffer(framebuffer)?;
        for &region in regions {
            let (x_start, x_end, y_start, y_end) = validate_region(region)?;
            self.command(0x44)?;
            self.data(&[x_start as u8, x_end as u8])?;
            self.command(0x45)?;
            self.data(&[
                y_start as u8,
                (y_start >> 8) as u8,
                y_end as u8,
                (y_end >> 8) as u8,
            ])?;
            self.command(0x4e)?;
            self.data(&[x_start as u8])?;
            self.command(0x4f)?;
            self.data(&[y_start as u8, (y_start >> 8) as u8])?;

            self.command(0x24)?;
            let row_bytes = WIDTH / 8;
            for y in region.top_left.y as usize
                ..(region.top_left.y as usize + region.size.height as usize)
            {
                let start = y * row_bytes + x_start;
                self.data(&framebuffer[start..=y * row_bytes + x_end])?;
            }
        }
        self.command(0x22)?;
        self.data(&[0xcf])?;
        self.command(0x20)?;
        self.wait_until_idle(Duration::from_secs(5))
    }

    fn command(&mut self, command: u8) -> Result<()> {
        self.dc.set_low()?;
        self.spi.write(&[command])?;
        Ok(())
    }

    fn hardware_reset(&mut self) -> Result<()> {
        self.reset.set_high()?;
        FreeRtos::delay_ms(50);
        self.reset.set_low()?;
        FreeRtos::delay_ms(20);
        self.reset.set_high()?;
        FreeRtos::delay_ms(50);
        Ok(())
    }

    fn data(&mut self, data: &[u8]) -> Result<()> {
        self.dc.set_high()?;
        self.spi.write(data)?;
        Ok(())
    }

    fn wait_until_idle(&mut self, timeout: Duration) -> Result<()> {
        let timer_service = EspTaskTimerService::new()?;
        let mut timer = timer_service.timer_async()?;
        let ready = block_on(async {
            let mut busy = core::pin::pin!(self.busy.wait_for_low());
            let mut deadline = core::pin::pin!(timer.after(timeout));
            poll_fn(|context| {
                if let Poll::Ready(result) = busy.as_mut().poll(context) {
                    return Poll::Ready(result.map(|()| true));
                }
                if let Poll::Ready(result) = deadline.as_mut().poll(context) {
                    return Poll::Ready(result.map(|()| false));
                }
                Poll::Pending
            })
            .await
        })?;
        if !ready {
            self.busy.disable_interrupt()?;
            bail!("timed out waiting for the e-paper BUSY signal");
        }
        Ok(())
    }

    fn load_full_refresh_lut(&mut self) -> Result<()> {
        self.load_lut(&FULL_REFRESH_LUT)
    }

    fn load_lut(&mut self, lut: &[u8; 159]) -> Result<()> {
        self.command(0x32)?;
        self.data(&lut[..153])?;
        self.wait_until_idle(Duration::from_secs(5))?;

        self.command(0x3f)?;
        self.data(&lut[153..154])?;
        self.command(0x03)?;
        self.data(&lut[154..155])?;
        self.command(0x04)?;
        self.data(&lut[155..158])?;
        self.command(0x2c)?;
        self.data(&lut[158..159])
    }
}

fn validate_framebuffer(framebuffer: &[u8]) -> Result<()> {
    if framebuffer.len() != FRAMEBUFFER_SIZE {
        bail!(
            "framebuffer is {} bytes; expected {FRAMEBUFFER_SIZE}",
            framebuffer.len()
        );
    }
    Ok(())
}

fn validate_region(region: Rectangle) -> Result<(usize, usize, usize, usize)> {
    if region.size.width == 0
        || region.size.height == 0
        || region.top_left.x < 0
        || region.top_left.y < 0
        || region.top_left.x as usize + region.size.width as usize > WIDTH
        || region.top_left.y as usize + region.size.height as usize > HEIGHT
    {
        bail!("partial update region {region:?} is outside the panel");
    }
    if region.top_left.x % 8 != 0 {
        bail!("partial update x coordinate must be byte aligned");
    }

    let x_start = region.top_left.x as usize / 8;
    let x_end = (region.top_left.x as usize + region.size.width as usize - 1) / 8;
    let top = region.top_left.y as usize;
    let bottom = top + region.size.height as usize - 1;
    // RAM Y decreases while framebuffer rows increase from visual top to bottom.
    Ok((x_start, x_end, HEIGHT - 1 - top, HEIGHT - 1 - bottom))
}
