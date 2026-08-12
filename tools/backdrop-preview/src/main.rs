//! Renders the firmware's real artwork to PNG contact sheets so it can be
//! reviewed without flashing the device: `backdrops.png` for the clock card,
//! and `update-screens.png` for every state of the firmware update screens,
//! which are otherwise visible only during a live update.
//!
//! The framebuffer, backdrop, style, and update-screen modules are included
//! straight from the firmware source; only the clock/date overlay is re-created
//! here, mirroring `ClockWidget`. The modules are declared with the names their
//! firmware paths expect — `updates` reaches its siblings through `super`, which
//! is `dashboard` there and the crate root here. Pass a path to a packed 1-bit
//! bitmap to preview a custom backdrop as the panel would draw it.

// The included firmware modules carry more API than one preview needs.
#![allow(dead_code)]

#[path = "../../../src/ink_stacks.rs"]
mod ink_stacks;

#[path = "../../../src/dashboard/backdrops.rs"]
mod backdrops;

#[path = "../../../src/dashboard/style.rs"]
mod style;

#[path = "../../../src/dashboard/updates.rs"]
mod updates;

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

/// The whole panel, which the update screens draw into.
const PANEL: Size = Size::new(200, 200);
/// Twice the panel is already 400 pixels a cell; three times needs a scroll.
const PANEL_SCALE: u32 = 2;

fn main() {
    backdrop_sheet();
    update_screen_sheet();
}

/// Every state of the update screens, with the download sampled at the ends and
/// the middle so the bar, the percentage, and the byte counts can be checked
/// against each other.
fn update_screen_sheet() {
    // A plausible release: the size is the one in the manifest example.
    const VERSION: &str = "0.1.1";
    const TOTAL: usize = 1_415_648;

    let mut screens = vec![
        ("Checking".to_owned(), updates::Screen::Checking),
        (
            "Available".to_owned(),
            updates::Screen::Available {
                version: VERSION.to_owned(),
                size: TOTAL,
            },
        ),
    ];
    // The worker reports every ten percent; the ends and one step between them
    // are what the bar, the percentage, and the byte counts have to agree on.
    screens.extend([0, 40, 100].map(|percent: usize| {
        (
            format!("Downloading {percent}%"),
            updates::Screen::Downloading {
                version: VERSION.to_owned(),
                // Rounded up, so the screen's own integer division lands back on
                // the percentage this cell is labelled with.
                downloaded: (TOTAL * percent).div_ceil(100),
                total: TOTAL,
            },
        )
    }));
    screens.extend([
        (
            "Finalizing".to_owned(),
            updates::Screen::Finalizing {
                version: VERSION.to_owned(),
            },
        ),
        (
            "Restarting".to_owned(),
            updates::Screen::Restarting {
                version: VERSION.to_owned(),
            },
        ),
        ("Up to date".to_owned(), updates::Screen::UpToDate),
        (
            "Failed".to_owned(),
            updates::Screen::Failed {
                // A real message from the firmware, long enough to wrap.
                message: "firmware SHA-256 does not match the manifest digest".to_owned(),
            },
        ),
    ]);

    let cells: Vec<(String, Framebuffer)> = screens
        .into_iter()
        .map(|(label, screen)| {
            let mut frame = Framebuffer::new(PANEL);
            frame.clear(BinaryColor::Off);
            updates::render(&mut frame, &screen, VERSION);
            (label, frame)
        })
        .collect();

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/update-screens.png");
    write_sheet(path, &cells, 3, PANEL, PANEL_SCALE);
    println!("wrote {path} ({} update screens)", cells.len());
}

fn backdrop_sheet() {
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
    write_sheet(path, &cells, 4, CELL, SCALE);
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
fn write_sheet(
    path: &str,
    cells: &[(String, Framebuffer)],
    columns: usize,
    cell: Size,
    scale: u32,
) {
    const CAPTION: u32 = 14;
    const GAP: u32 = 6;

    let rows = cells.len().div_ceil(columns);
    let cell_height = cell.height + CAPTION;
    let sheet_width = GAP + (cell.width + GAP) * columns as u32;
    let sheet_height = GAP + (cell_height + GAP) * rows as u32;

    let mut sheet = Framebuffer::new(Size::new(sheet_width, sheet_height));
    sheet.clear(BinaryColor::Off);

    for (index, (label, frame)) in cells.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let origin = Point::new(
            (GAP + (cell.width + GAP) * column as u32) as i32,
            (GAP + (cell_height + GAP) * row as u32) as i32,
        );
        for y in 0..cell.height as i32 {
            for x in 0..cell.width as i32 {
                let color = read_pixel(frame, Point::new(x, y));
                Pixel(origin + Point::new(x, y), color)
                    .draw(&mut sheet)
                    .ok();
            }
        }
        Text::with_baseline(
            label,
            origin + Point::new(0, cell.height as i32 + 2),
            MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
            Baseline::Top,
        )
        .draw(&mut sheet)
        .ok();
    }

    let mut pixels = Vec::with_capacity((sheet_width * sheet_height * scale * scale) as usize);
    for y in 0..sheet_height {
        let mut row = Vec::with_capacity((sheet_width * scale) as usize);
        for x in 0..sheet_width {
            let value = match read_pixel(&sheet, Point::new(x as i32, y as i32)) {
                BinaryColor::On => 0,
                BinaryColor::Off => 255,
            };
            row.extend(std::iter::repeat_n(value, scale as usize));
        }
        for _ in 0..scale {
            pixels.extend_from_slice(&row);
        }
    }
    std::fs::write(
        path,
        png::encode(sheet_width * scale, sheet_height * scale, &pixels),
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
