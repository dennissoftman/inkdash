use std::convert::Infallible;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use crate::battery::BatteryReading;
use crate::datetime::DateTime;
use crate::epaper::{FRAMEBUFFER_SIZE, HEIGHT, WIDTH};
use crate::shtc3::ClimateReading;

pub struct DashboardData {
    pub time: Option<DateTime>,
    pub climate: Option<ClimateReading>,
    pub battery: Option<BatteryReading>,
    pub wifi_connected: bool,
    pub wifi_ssid: Option<String>,
}

pub struct Framebuffer {
    bytes: Box<[u8]>,
}

impl Framebuffer {
    pub fn new() -> Self {
        Self {
            bytes: vec![0xff; FRAMEBUFFER_SIZE].into_boxed_slice(),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn clear_white(&mut self) {
        self.bytes.fill(0xff);
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if (0..WIDTH as i32).contains(&point.x) && (0..HEIGHT as i32).contains(&point.y) {
                let index = point.y as usize * (WIDTH / 8) + (point.x as usize >> 3);
                let mask = 1 << (7 - (point.x as usize & 7));
                match color {
                    BinaryColor::On => self.bytes[index] &= !mask,
                    BinaryColor::Off => self.bytes[index] |= mask,
                }
            }
        }
        Ok(())
    }
}

pub fn render(framebuffer: &mut Framebuffer, data: &DashboardData) {
    framebuffer.clear_white();
    let thin = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let filled = PrimitiveStyle::with_fill(BinaryColor::On);

    // Status bar.
    draw_wifi_badge(
        framebuffer,
        data.wifi_connected,
        data.wifi_ssid.as_deref(),
        filled,
    );
    if let Some(battery) = data.battery {
        draw_battery(framebuffer, battery, thin, filled);
    }
    Line::new(Point::new(4, 25), Point::new(195, 25))
        .into_styled(thin)
        .draw(framebuffer)
        .ok();

    let small = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let value_style = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);
    let centered_top = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();

    let time = data
        .time
        .map(|value| format!("{:02}:{:02}", value.hour, value.minute))
        .unwrap_or_else(|| "--:--".to_owned());
    draw_double_size_time(framebuffer, &time, Point::new(100, 43));

    Line::new(Point::new(4, 105), Point::new(195, 105))
        .into_styled(thin)
        .draw(framebuffer)
        .ok();
    Line::new(Point::new(100, 115), Point::new(100, 190))
        .into_styled(thin)
        .draw(framebuffer)
        .ok();

    Text::with_text_style("TEMP", Point::new(50, 120), small, centered_top)
        .draw(framebuffer)
        .ok();
    Text::with_text_style("HUM", Point::new(150, 120), small, centered_top)
        .draw(framebuffer)
        .ok();

    let temperature = data
        .climate
        .map(|reading| format!("{:.1} C", reading.temperature_c))
        .unwrap_or_else(|| "--.- C".to_owned());
    let humidity = data
        .climate
        .map(|reading| format!("{:.0}%", reading.humidity_percent))
        .unwrap_or_else(|| "--%".to_owned());
    Text::with_text_style(&temperature, Point::new(50, 149), value_style, centered_top)
        .draw(framebuffer)
        .ok();
    Text::with_text_style(&humidity, Point::new(150, 149), value_style, centered_top)
        .draw(framebuffer)
        .ok();
}

fn draw_double_size_time(target: &mut Framebuffer, value: &str, center_top: Point) {
    const SCALE: i32 = 2;
    let width = value.chars().count() as i32 * FONT_10X20.character_size.width as i32 * SCALE;
    let origin = Point::new(center_top.x - width / 2, center_top.y);
    let mut scaled = ScaledDrawTarget {
        target,
        origin,
        scale: SCALE,
    };
    let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
    Text::with_baseline(value, Point::zero(), style, Baseline::Top)
        .draw(&mut scaled)
        .ok();
}

struct ScaledDrawTarget<'a> {
    target: &'a mut Framebuffer,
    origin: Point,
    scale: i32,
}

impl OriginDimensions for ScaledDrawTarget<'_> {
    fn size(&self) -> Size {
        Size::new(
            WIDTH as u32 / self.scale as u32,
            HEIGHT as u32 / self.scale as u32,
        )
    }
}

impl DrawTarget for ScaledDrawTarget<'_> {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let top_left = self.origin + Point::new(point.x * self.scale, point.y * self.scale);
            Rectangle::new(top_left, Size::new(self.scale as u32, self.scale as u32))
                .into_styled(PrimitiveStyle::with_fill(color))
                .draw(self.target)?;
        }
        Ok(())
    }
}

fn draw_wifi_badge(
    target: &mut Framebuffer,
    connected: bool,
    ssid: Option<&str>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    let heights = if connected { [3, 6, 9] } else { [3, 3, 3] };
    for (index, height) in heights.into_iter().enumerate() {
        Rectangle::new(
            Point::new(5 + index as i32 * 4, 17 - height),
            Size::new(3, height as u32),
        )
        .into_styled(filled)
        .draw(target)
        .ok();
    }

    // Reserve the right side for the battery indicator. Non-ASCII glyphs are
    // represented by '?' because this compact built-in font is ASCII-only.
    let label = compact_ssid(ssid);
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    Text::with_baseline(&label, Point::new(20, 7), style, Baseline::Top)
        .draw(target)
        .ok();
}

fn compact_ssid(ssid: Option<&str>) -> String {
    const MAX_CHARS: usize = 20;

    let Some(ssid) = ssid else {
        return "No WiFi".to_owned();
    };
    let mut label = String::with_capacity(MAX_CHARS);
    let mut characters = ssid.chars();
    for character in characters.by_ref().take(MAX_CHARS) {
        label.push(if character.is_ascii() { character } else { '?' });
    }
    if characters.next().is_some() {
        label.truncate(MAX_CHARS - 3);
        label.push_str("...");
    }
    label
}

fn draw_battery(
    target: &mut Framebuffer,
    battery: BatteryReading,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(Point::new(150, 7), Size::new(17, 10))
        .into_styled(thin)
        .draw(target)
        .ok();
    Rectangle::new(Point::new(167, 10), Size::new(2, 4))
        .into_styled(filled)
        .draw(target)
        .ok();

    let fill_width = ((battery.percent as u32 * 13) / 100).max(1);
    Rectangle::new(Point::new(152, 9), Size::new(fill_width, 6))
        .into_styled(filled)
        .draw(target)
        .ok();

    let label = format!("{}%", battery.percent);
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let right_top = TextStyleBuilder::new()
        .alignment(Alignment::Right)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(&label, Point::new(197, 7), style, right_top)
        .draw(target)
        .ok();
}
