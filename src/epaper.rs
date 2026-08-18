use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

use anyhow::{bail, Result};
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Input, Output, PinDriver};
use esp_idf_hal::spi::SpiSingleDeviceDriver;
use esp_idf_hal::task::block_on;
use esp_idf_svc::timer::{EspAsyncTimer, EspTaskTimerService};

pub const WIDTH: usize = 200;
pub const HEIGHT: usize = 200;
pub const FRAMEBUFFER_SIZE: usize = WIDTH * HEIGHT / 8;

// Waveform tables, loaded over the panel's own factory waveforms.
//
// TEMPERATURE: these are fixed, and loading them costs the panel's built-in
// temperature compensation. `init_full` enables the internal temperature sensor
// (0x18), but that only feeds the OTP waveform selection a host-loaded LUT
// replaces, so the frame counts below are whatever they say regardless of how
// cold the panel is.
//
// That matters because a waveform is a duration, not a value: pigment moves
// through an oil whose viscosity rises as it cools, so the same frame count
// delivers less movement when cold. Under-driven pixels stop between the rails
// and drift back toward their previous state over the following seconds, which
// reads as the display silently reverting to an older frame -- the clock
// walking backwards while the firmware's own log stays monotonic.
//
// The counts here were settled at roughly 25-30 C, which is where this board
// has been run. Somewhere cold they may under-drive again, with that same
// decay signature. The board already measures ambient temperature with the
// SHTC3, so the fix if it shows up is to scale the drive rather than to retune
// by hand: raise group 0's repeat count (byte 66, `RP`) as temperature falls,
// for example one extra pass below ~20 C and another below ~10 C. Frames are
// the cheap knob -- each pass is about 300 ms and costs no contrast at the
// warm end, since a pixel already at the rail stays there.
//
// Full-refresh waveform from Waveshare's ESP32-S3-ePaper-1.54 example.
const FULL_REFRESH_LUT: [u8; 159] = [
    0x80, 0x48, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x48, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80,
    0x48, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x48, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0x0a, 0, 0, 0, 0, 0, 0, 0x08, 0x01, 0, 0x08, 0x01, 0, 0x02, 0x0a, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0, 0, 0, 0x22, 0x17, 0x41, 0, 0x32, 0x20,
];

// Partial-refresh waveform. Group 0 does the transition: LUT1 (0x80) pulls
// white->black at VSL, LUT2 (0x40) pulls black->white at VSH1. Unchanged pixels
// get a one-frame touch-up in group 1.
//
// Group 0 repeats three times (RP=2) rather than running once. At a single pass
// the partial waveform spent 17 frames against the full refresh's 74 on this
// panel, which parked pixels between the rails instead of at them: an update
// looked correct and then drifted back toward the image before it over the
// following seconds, so the clock appeared to walk backwards while the firmware
// log stayed monotonic. Three passes spend 47 frames, close enough to the full
// refresh to settle the ink without the full refresh's flash.
const PARTIAL_REFRESH_LUT: [u8; 159] = [
    0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x40, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0x0f, 0, 0, 0, 0, 0, 0x02, 0x01, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x22, 0x22, 0x22,
    0x22, 0x22, 0x22, 0, 0, 0, 0x02, 0x17, 0x41, 0xb0, 0x32, 0x28,
];

pub struct Epaper<'d> {
    spi: SpiSingleDeviceDriver<'d>,
    power: PinDriver<'d, Output>,
    busy: PinDriver<'d, Input>,
    reset: PinDriver<'d, Output>,
    dc: PinDriver<'d, Output>,
    /// A refresh waits on BUSY a dozen times. Keeping one timer for the
    /// driver's lifetime avoids creating and destroying an ESP timer per wait.
    deadline: EspAsyncTimer,
}

impl<'d> Epaper<'d> {
    pub fn new(
        spi: SpiSingleDeviceDriver<'d>,
        power: PinDriver<'d, Output>,
        busy: PinDriver<'d, Input>,
        reset: PinDriver<'d, Output>,
        dc: PinDriver<'d, Output>,
    ) -> Result<Self> {
        let deadline = EspTaskTimerService::new()?.timer_async()?;
        Ok(Self {
            spi,
            power,
            busy,
            reset,
            dc,
            deadline,
        })
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
        // The update below refreshes the area described by the RAM window, not
        // the whole panel, so it can only ever drive one region. Writing every
        // region and then updating once left all but the last sitting in RAM,
        // unrendered -- a glyph whose diff split into several rectangles came
        // out with pieces missing. Coalescing to the bounding box keeps the one
        // update while covering every change in it.
        let Some(window) = bounding_box(regions) else {
            return Ok(());
        };
        self.write_region(0x24, framebuffer, window)?;
        self.command(0x22)?;
        self.data(&[0xcf])?;
        self.command(0x20)?;
        self.wait_until_idle(Duration::from_secs(5))?;

        // Catch the previous-image RAM up to what was just displayed. The
        // controller drives each partial transition from it and never updates
        // it itself, so left alone it keeps describing the last full refresh
        // and every later partial is computed against a state the glass no
        // longer holds. That matters more now the window is a bounding box: one
        // changed pixel can drag the whole clock into it, and everything inside
        // would be re-driven against the wrong reference.
        //
        // After the update, never before: seeding it with the same bytes first
        // would leave nothing to transition and the panel would not change.
        self.write_region(0x26, framebuffer, window)
    }

    /// Point the RAM window at `region` and stream its rows into `ram_command`,
    /// which is either the black/white RAM (0x24) or the previous image (0x26).
    fn write_region(
        &mut self,
        ram_command: u8,
        framebuffer: &[u8],
        region: Rectangle,
    ) -> Result<()> {
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

        self.command(ram_command)?;
        let row_bytes = WIDTH / 8;
        for y in
            region.top_left.y as usize..(region.top_left.y as usize + region.size.height as usize)
        {
            let start = y * row_bytes + x_start;
            self.data(&framebuffer[start..=y * row_bytes + x_end])?;
        }
        Ok(())
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
        let Self {
            busy: busy_pin,
            deadline: timer,
            ..
        } = self;
        let ready = block_on(async {
            let mut busy = core::pin::pin!(busy_pin.wait_for_low());
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

/// The smallest rectangle covering every changed region. Left and right edges
/// are already byte aligned by the differ, and the panel width is a whole
/// number of bytes, so the union stays aligned too.
fn bounding_box(regions: &[Rectangle]) -> Option<Rectangle> {
    let left = regions.iter().map(|r| r.top_left.x).min()?;
    let top = regions.iter().map(|r| r.top_left.y).min()?;
    let right = regions
        .iter()
        .map(|r| r.top_left.x + r.size.width as i32)
        .max()?;
    let bottom = regions
        .iter()
        .map(|r| r.top_left.y + r.size.height as i32)
        .max()?;
    Some(Rectangle::new(
        Point::new(left, top),
        Size::new((right - left) as u32, (bottom - top) as u32),
    ))
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
