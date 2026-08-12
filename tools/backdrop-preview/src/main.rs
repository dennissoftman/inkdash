//! Renders the firmware's real clock-card artwork to a PNG contact sheet so it
//! can be reviewed without flashing the device.
//!
//! The framebuffer and backdrop modules are included straight from the firmware
//! source; only the clock/date overlay is re-created here, mirroring
//! `ClockWidget`. Pass a path to a packed 1-bit bitmap to preview a custom
//! backdrop as the panel would draw it.

// The included firmware modules carry more API than one preview needs.
#![allow(dead_code)]

#[path = "../../../src/ink_stacks.rs"]
mod ink_stacks;

#[path = "../../../src/dashboard/backdrops.rs"]
mod backdrops;

mod png;

use std::convert::Infallible;

use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::mono_font::iso_8859_5::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use backdrops::{Sky, Slot};
use ink_stacks::Framebuffer;

const CELL: Size = Size::new(200, 80);
const SCALE: u32 = 3;
const DATE: &str = "Wed 12 Aug";

fn main() {
    // One row per weather condition, one column per hour slot.
    let mut cells: Vec<(String, Framebuffer)> = Vec::new();
    for sky in Sky::ALL {
        for (slot, clock) in [
            (Slot::Morning, "07:24"),
            (Slot::Day, "13:05"),
            (Slot::Evening, "19:41"),
            (Slot::Night, "23:58"),
        ] {
            let mut frame = Framebuffer::new(CELL);
            frame.clear(BinaryColor::Off);
            let bounds = frame.bounds();
            let color = backdrops::draw(&mut frame, bounds, slot, sky);
            clock_text(&mut frame, bounds, clock, DATE, color);
            cells.push((format!("{} {}", slot.label(), sky.label()), frame));
        }
    }
    let weather_cells = cells.len();

    // A custom upload, with and without a plate. A path argument renders a real
    // file produced by the host tool, which checks the format contract.
    let pattern = match std::env::args().nth(1) {
        Some(path) => {
            let bytes = std::fs::read(&path).expect("reading bitmap");
            assert_eq!(
                bytes.len(),
                backdrops::CUSTOM_BYTES,
                "{path} is not {} bytes",
                backdrops::CUSTOM_BYTES
            );
            bytes
        }
        None => {
            let mut pattern = vec![0_u8; backdrops::CUSTOM_BYTES];
            for y in 0..backdrops::CUSTOM_HEIGHT as usize {
                for x in 0..backdrops::CUSTOM_WIDTH as usize {
                    let ink = ((x + y) / 6) % 2 == 0 || (x * x + y * y) % 97 < 8;
                    if ink {
                        pattern[y * backdrops::CUSTOM_ROW_BYTES + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            pattern
        }
    };
    for (label, style) in [
        (
            "CUSTOM plate",
            backdrops::Style {
                light_text: false,
                plate: true,
            },
        ),
        (
            "CUSTOM bare",
            backdrops::Style {
                light_text: false,
                plate: false,
            },
        ),
    ] {
        let mut frame = Framebuffer::new(CELL);
        frame.clear(BinaryColor::Off);
        let bounds = frame.bounds();
        let color = backdrops::draw_custom(&mut frame, bounds, &pattern, style);
        clock_text(&mut frame, bounds, "16:20", DATE, color);
        cells.push((label.to_owned(), frame));
    }

    // Beside the tool, so the output does not depend on the working directory.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/backdrops.png");
    write_sheet(path, &cells, 4);
    println!(
        "wrote {path} ({weather_cells} weather cells, {} custom)",
        cells.len() - weather_cells
    );
}

fn clock_text(
    target: &mut Framebuffer,
    bounds: Rectangle,
    clock: &str,
    date: &str,
    color: BinaryColor,
) {
    const SCALE: i32 = 2;
    let width = clock.chars().count() as i32 * FONT_10X20.character_size.width as i32 * SCALE;
    let origin = Point::new(
        bounds.top_left.x + bounds.size.width as i32 / 2 - width / 2,
        bounds.top_left.y + 11,
    );
    let mut scaled = Scaled {
        target,
        origin,
        scale: SCALE,
    };
    Text::with_baseline(
        clock,
        Point::zero(),
        MonoTextStyle::new(&FONT_10X20, color),
        Baseline::Top,
    )
    .draw(&mut scaled)
    .ok();

    Text::with_text_style(
        date,
        Point::new(
            bounds.top_left.x + bounds.size.width as i32 / 2,
            bounds.top_left.y + 52,
        ),
        MonoTextStyle::new(&FONT_9X15_BOLD, color),
        TextStyleBuilder::new()
            .alignment(Alignment::Center)
            .baseline(Baseline::Top)
            .build(),
    )
    .draw(target)
    .ok();
}

struct Scaled<'a> {
    target: &'a mut Framebuffer,
    origin: Point,
    scale: i32,
}

impl OriginDimensions for Scaled<'_> {
    fn size(&self) -> Size {
        let size = self.target.size();
        Size::new(
            size.width / self.scale as u32,
            size.height / self.scale as u32,
        )
    }
}

impl DrawTarget for Scaled<'_> {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            Rectangle::new(
                self.origin + Point::new(point.x * self.scale, point.y * self.scale),
                Size::new(self.scale as u32, self.scale as u32),
            )
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(self.target)?;
        }
        Ok(())
    }
}

/// Lays the cells out in a grid with a caption strip under each one.
fn write_sheet(path: &str, cells: &[(String, Framebuffer)], columns: usize) {
    const CAPTION: u32 = 14;
    const GAP: u32 = 6;

    let rows = cells.len().div_ceil(columns);
    let cell_height = CELL.height + CAPTION;
    let sheet_width = GAP + (CELL.width + GAP) * columns as u32;
    let sheet_height = GAP + (cell_height + GAP) * rows as u32;

    let mut sheet = Framebuffer::new(Size::new(sheet_width, sheet_height));
    sheet.clear(BinaryColor::Off);

    for (index, (label, frame)) in cells.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let origin = Point::new(
            (GAP + (CELL.width + GAP) * column as u32) as i32,
            (GAP + (cell_height + GAP) * row as u32) as i32,
        );
        for y in 0..CELL.height as i32 {
            for x in 0..CELL.width as i32 {
                let color = read_pixel(frame, Point::new(x, y));
                Pixel(origin + Point::new(x, y), color).draw(&mut sheet).ok();
            }
        }
        Text::with_baseline(
            label,
            origin + Point::new(0, CELL.height as i32 + 2),
            MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
            Baseline::Top,
        )
        .draw(&mut sheet)
        .ok();
    }

    let mut pixels = Vec::with_capacity((sheet_width * sheet_height * SCALE * SCALE) as usize);
    for y in 0..sheet_height {
        let mut row = Vec::with_capacity((sheet_width * SCALE) as usize);
        for x in 0..sheet_width {
            let value = match read_pixel(&sheet, Point::new(x as i32, y as i32)) {
                BinaryColor::On => 0,
                BinaryColor::Off => 255,
            };
            row.extend(std::iter::repeat(value).take(SCALE as usize));
        }
        for _ in 0..SCALE {
            pixels.extend_from_slice(&row);
        }
    }
    std::fs::write(
        path,
        png::encode(sheet_width * SCALE, sheet_height * SCALE, &pixels),
    )
    .expect("writing preview PNG");
}

fn read_pixel(frame: &Framebuffer, point: Point) -> BinaryColor {
    let row_bytes = (frame.size().width as usize).div_ceil(8);
    let byte = frame.bytes()[point.y as usize * row_bytes + point.x as usize / 8];
    if byte & (0x80 >> (point.x as usize % 8)) == 0 {
        BinaryColor::On
    } else {
        BinaryColor::Off
    }
}
