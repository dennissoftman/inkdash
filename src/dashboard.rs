use std::convert::Infallible;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle, Triangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use crate::battery::{BatteryReading, BatteryStatus};
use crate::datetime::DateTime;
use crate::epaper::{FRAMEBUFFER_SIZE, HEIGHT, WIDTH};
use crate::shtc3::ClimateReading;
use crate::weather::{Weather, WeatherKind};
use crate::wifi::wifi_signal_bars;

pub struct DashboardData {
    pub time: Option<DateTime>,
    pub climate: Option<ClimateReading>,
    pub battery: BatteryStatus,
    pub wifi_connected: bool,
    pub wifi_ssid: Option<String>,
    pub wifi_signal_dbm: Option<i8>,
    pub weather: Option<Weather>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardScreen {
    Home,
    Today,
    Forecast,
}

impl DashboardScreen {
    pub const fn next(self) -> Self {
        match self {
            Self::Home => Self::Today,
            Self::Today => Self::Forecast,
            Self::Forecast => Self::Home,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Home => Self::Forecast,
            Self::Today => Self::Home,
            Self::Forecast => Self::Today,
        }
    }
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

pub fn render(framebuffer: &mut Framebuffer, data: &DashboardData, screen: DashboardScreen) {
    framebuffer.clear_white();
    let thin = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let filled = PrimitiveStyle::with_fill(BinaryColor::On);

    // Status bar.
    draw_wifi_badge(
        framebuffer,
        data.wifi_connected,
        data.wifi_signal_dbm,
        data.wifi_ssid.as_deref(),
        filled,
    );
    draw_battery_status(framebuffer, data.battery, thin, filled);
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

    match screen {
        DashboardScreen::Today => {
            draw_today_screen(
                framebuffer,
                data,
                small,
                value_style,
                centered_top,
                thin,
                filled,
            );
            return;
        }
        DashboardScreen::Forecast => {
            draw_forecast_screen(
                framebuffer,
                data,
                small,
                value_style,
                centered_top,
                thin,
                filled,
            );
            return;
        }
        DashboardScreen::Home => {}
    }

    let time = data
        .time
        .map(|value| format!("{:02}:{:02}", value.hour, value.minute))
        .unwrap_or_else(|| "--:--".to_owned());
    let time_color = draw_time_background(framebuffer, data.time, thin, filled);
    draw_double_size_time(framebuffer, &time, Point::new(100, 37), time_color);
    let date = data
        .time
        .map(|value| value.short_date())
        .unwrap_or_else(|| "--- -- ---".to_owned());
    let date_style = MonoTextStyle::new(&FONT_9X15_BOLD, time_color);
    Text::with_text_style(&date, Point::new(100, 78), date_style, centered_top)
        .draw(framebuffer)
        .ok();

    Line::new(Point::new(4, 105), Point::new(195, 105))
        .into_styled(thin)
        .draw(framebuffer)
        .ok();
    Line::new(Point::new(100, 115), Point::new(100, 190))
        .into_styled(thin)
        .draw(framebuffer)
        .ok();

    Text::with_text_style("INDOOR", Point::new(50, 116), small, centered_top)
        .draw(framebuffer)
        .ok();
    Text::with_text_style("WEATHER", Point::new(150, 116), small, centered_top)
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
    draw_thermometer_icon(framebuffer, Point::new(18, 143), thin, filled);
    draw_drop_icon(framebuffer, Point::new(18, 166), filled);
    let left_top = TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(&temperature, Point::new(31, 135), value_style, left_top)
        .draw(framebuffer)
        .ok();
    Text::with_text_style(&humidity, Point::new(31, 160), value_style, left_top)
        .draw(framebuffer)
        .ok();

    draw_weather_widget(framebuffer, data.weather, small, value_style, centered_top);
    draw_page_indicator(framebuffer, 0, thin, filled);
}

fn draw_double_size_time(
    target: &mut Framebuffer,
    value: &str,
    center_top: Point,
    color: BinaryColor,
) {
    const SCALE: i32 = 2;
    let width = value.chars().count() as i32 * FONT_10X20.character_size.width as i32 * SCALE;
    let origin = Point::new(center_top.x - width / 2, center_top.y);
    let mut scaled = ScaledDrawTarget {
        target,
        origin,
        scale: SCALE,
    };
    let style = MonoTextStyle::new(&FONT_10X20, color);
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
    signal_dbm: Option<i8>,
    ssid: Option<&str>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    let active_bars = wifi_signal_bars(connected, signal_dbm) as usize;
    for (index, height) in [3_i32, 6, 9].into_iter().enumerate() {
        let height = if index < active_bars { height } else { 1 };
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

fn draw_weather_widget(
    target: &mut Framebuffer,
    weather: Option<Weather>,
    small: MonoTextStyle<'static, BinaryColor>,
    value_style: MonoTextStyle<'static, BinaryColor>,
    centered_top: embedded_graphics::text::TextStyle,
) {
    let thin = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    let filled = PrimitiveStyle::with_fill(BinaryColor::On);
    draw_weather_icon(
        target,
        weather.map(Weather::kind),
        Point::new(150, 136),
        thin,
        filled,
    );

    let temperature = weather
        .map(|value| format!("{:.1} C", value.temperature_c))
        .unwrap_or_else(|| "--.- C".to_owned());
    Text::with_text_style(
        &temperature,
        Point::new(150, 151),
        value_style,
        centered_top,
    )
    .draw(target)
    .ok();

    let range = weather
        .map(|value| {
            format!(
                "L{:.0} H{:.0} C",
                value.today.minimum_temperature_c, value.today.maximum_temperature_c
            )
        })
        .unwrap_or_else(|| "L-- H-- C".to_owned());
    Text::with_text_style(&range, Point::new(150, 167), small, centered_top)
        .draw(target)
        .ok();

    let details = weather
        .map(
            |value| match value.today.precipitation_probability_percent {
                Some(rain) => format!("RH{}% R{}%", value.relative_humidity_percent, rain),
                None => format!("RH{}% R--%", value.relative_humidity_percent),
            },
        )
        .unwrap_or_else(|| "RH--% R--%".to_owned());
    Text::with_text_style(&details, Point::new(150, 178), small, centered_top)
        .draw(target)
        .ok();
}

fn draw_today_screen(
    target: &mut Framebuffer,
    data: &DashboardData,
    small: MonoTextStyle<'static, BinaryColor>,
    value_style: MonoTextStyle<'static, BinaryColor>,
    centered_top: embedded_graphics::text::TextStyle,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    let title = data
        .time
        .map(|value| format!("TODAY  {}", value.short_date()))
        .unwrap_or_else(|| "TODAY".to_owned());
    Text::with_text_style(&title, Point::new(100, 31), small, centered_top)
        .draw(target)
        .ok();
    Line::new(Point::new(8, 44), Point::new(192, 44))
        .into_styled(thin)
        .draw(target)
        .ok();

    let kind = data.weather.map(Weather::kind);
    draw_weather_icon(target, kind, Point::new(47, 68), thin, filled);
    let condition = data.weather.map(Weather::condition).unwrap_or("NO DATA");
    Text::with_text_style(condition, Point::new(47, 85), small, centered_top)
        .draw(target)
        .ok();

    let temperature = data
        .weather
        .map(|value| format!("{:.1} C", value.temperature_c))
        .unwrap_or_else(|| "--.- C".to_owned());
    Text::with_text_style(&temperature, Point::new(142, 57), value_style, centered_top)
        .draw(target)
        .ok();
    draw_drop_icon(target, Point::new(120, 88), filled);
    let humidity = data
        .weather
        .map(|value| format!("{}%", value.relative_humidity_percent))
        .unwrap_or_else(|| "--%".to_owned());
    let left_top = TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(&humidity, Point::new(132, 81), value_style, left_top)
        .draw(target)
        .ok();

    Line::new(Point::new(8, 106), Point::new(192, 106))
        .into_styled(thin)
        .draw(target)
        .ok();
    for x in [67, 133] {
        Line::new(Point::new(x, 112), Point::new(x, 172))
            .into_styled(thin)
            .draw(target)
            .ok();
    }
    let day = data.weather.map(|value| value.today);
    draw_metric(
        target,
        "LOW",
        day.map(|value| format!("{:.0} C", value.minimum_temperature_c)),
        Point::new(34, 118),
        small,
        value_style,
        centered_top,
    );
    draw_metric(
        target,
        "HIGH",
        day.map(|value| format!("{:.0} C", value.maximum_temperature_c)),
        Point::new(100, 118),
        small,
        value_style,
        centered_top,
    );
    draw_metric(
        target,
        "RAIN",
        day.and_then(|value| value.precipitation_probability_percent)
            .map(|value| format!("{value}%")),
        Point::new(166, 118),
        small,
        value_style,
        centered_top,
    );
    draw_page_indicator(target, 1, thin, filled);
}

fn draw_forecast_screen(
    target: &mut Framebuffer,
    data: &DashboardData,
    small: MonoTextStyle<'static, BinaryColor>,
    value_style: MonoTextStyle<'static, BinaryColor>,
    centered_top: embedded_graphics::text::TextStyle,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Text::with_text_style("FORECAST", Point::new(100, 31), small, centered_top)
        .draw(target)
        .ok();
    Line::new(Point::new(8, 44), Point::new(192, 44))
        .into_styled(thin)
        .draw(target)
        .ok();
    Line::new(Point::new(100, 50), Point::new(100, 178))
        .into_styled(thin)
        .draw(target)
        .ok();

    let days = [
        ("TODAY", data.weather.map(|value| value.today)),
        ("TOMORROW", data.weather.map(|value| value.tomorrow)),
    ];
    for (index, (label, day)) in days.into_iter().enumerate() {
        let center_x = 50 + index as i32 * 100;
        Text::with_text_style(label, Point::new(center_x, 51), small, centered_top)
            .draw(target)
            .ok();
        draw_weather_icon(
            target,
            day.map(|value| value.kind()),
            Point::new(center_x, 80),
            thin,
            filled,
        );
        let condition = day.map(|value| value.condition()).unwrap_or("NO DATA");
        Text::with_text_style(condition, Point::new(center_x, 97), small, centered_top)
            .draw(target)
            .ok();

        let high = day
            .map(|value| format!("{:.0} C", value.maximum_temperature_c))
            .unwrap_or_else(|| "-- C".to_owned());
        let low = day
            .map(|value| format!("{:.0} C", value.minimum_temperature_c))
            .unwrap_or_else(|| "-- C".to_owned());
        let rain = day
            .and_then(|value| value.precipitation_probability_percent)
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "--%".to_owned());
        Text::with_text_style("HIGH", Point::new(center_x, 113), small, centered_top)
            .draw(target)
            .ok();
        Text::with_text_style(&high, Point::new(center_x, 125), value_style, centered_top)
            .draw(target)
            .ok();
        Text::with_text_style("LOW", Point::new(center_x, 143), small, centered_top)
            .draw(target)
            .ok();
        Text::with_text_style(&low, Point::new(center_x, 155), value_style, centered_top)
            .draw(target)
            .ok();
        Text::with_text_style(
            &format!("RAIN {rain}"),
            Point::new(center_x, 174),
            small,
            centered_top,
        )
        .draw(target)
        .ok();
    }
    draw_page_indicator(target, 2, thin, filled);
}

#[allow(clippy::too_many_arguments)]
fn draw_metric(
    target: &mut Framebuffer,
    label: &str,
    value: Option<String>,
    position: Point,
    small: MonoTextStyle<'static, BinaryColor>,
    value_style: MonoTextStyle<'static, BinaryColor>,
    centered_top: embedded_graphics::text::TextStyle,
) {
    Text::with_text_style(label, position, small, centered_top)
        .draw(target)
        .ok();
    Text::with_text_style(
        value.as_deref().unwrap_or("--"),
        position + Point::new(0, 18),
        value_style,
        centered_top,
    )
    .draw(target)
    .ok();
}

fn draw_page_indicator(
    target: &mut Framebuffer,
    active: usize,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    for index in 0..3 {
        Circle::new(Point::new(91 + index as i32 * 8, 194), 4)
            .into_styled(if index == active { filled } else { thin })
            .draw(target)
            .ok();
    }
}

#[derive(Clone, Copy)]
enum TimeOfDay {
    Morning,
    Day,
    Evening,
    Night,
}

impl TimeOfDay {
    fn from_hour(hour: u8) -> Self {
        match hour {
            5..=10 => Self::Morning,
            11..=16 => Self::Day,
            17..=20 => Self::Evening,
            _ => Self::Night,
        }
    }
}

fn draw_time_background(
    target: &mut Framebuffer,
    time: Option<DateTime>,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) -> BinaryColor {
    let card = Rectangle::new(Point::new(4, 29), Size::new(192, 72));
    let Some(period) = time.map(|value| TimeOfDay::from_hour(value.hour)) else {
        card.into_styled(thin).draw(target).ok();
        return BinaryColor::On;
    };

    match period {
        TimeOfDay::Morning => {
            card.into_styled(thin).draw(target).ok();
            draw_sunrise(target, false, thin);
            BinaryColor::On
        }
        TimeOfDay::Day => {
            card.into_styled(thin).draw(target).ok();
            draw_day_sky(target, thin);
            BinaryColor::On
        }
        TimeOfDay::Evening => {
            card.into_styled(thin).draw(target).ok();
            draw_sunrise(target, true, thin);
            BinaryColor::On
        }
        TimeOfDay::Night => {
            card.into_styled(filled).draw(target).ok();
            draw_night_sky(target);
            BinaryColor::Off
        }
    }
}

fn draw_sunrise(target: &mut Framebuffer, evening: bool, thin: PrimitiveStyle<BinaryColor>) {
    let center_x = if evening { 177 } else { 22 };
    Circle::new(Point::new(center_x - 10, 83), 21)
        .into_styled(thin)
        .draw(target)
        .ok();
    for y in [96, 99] {
        Line::new(Point::new(8, y), Point::new(192, y))
            .into_styled(thin)
            .draw(target)
            .ok();
    }
    for (dx, dy) in [(-15, -8), (-11, -15), (0, -19), (11, -15), (15, -8)] {
        Line::new(
            Point::new(center_x + dx, 93 + dy),
            Point::new(center_x + dx / 2, 93 + dy / 2),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }
}

fn draw_day_sky(target: &mut Framebuffer, thin: PrimitiveStyle<BinaryColor>) {
    Circle::new(Point::new(173, 42), 13)
        .into_styled(thin)
        .draw(target)
        .ok();
    for (start, end) in [
        ((179, 37), (179, 40)),
        ((179, 56), (179, 59)),
        ((168, 48), (171, 48)),
        ((187, 48), (190, 48)),
        ((171, 40), (173, 42)),
        ((185, 54), (187, 56)),
        ((171, 56), (173, 54)),
        ((185, 42), (187, 40)),
    ] {
        Line::new(Point::new(start.0, start.1), Point::new(end.0, end.1))
            .into_styled(thin)
            .draw(target)
            .ok();
    }
    draw_outline_cloud(target, Point::new(20, 79), thin);
}

fn draw_night_sky(target: &mut Framebuffer) {
    let white_fill = PrimitiveStyle::with_fill(BinaryColor::Off);
    let black_fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let white_line = PrimitiveStyle::with_stroke(BinaryColor::Off, 1);
    Circle::new(Point::new(15, 43), 20)
        .into_styled(white_fill)
        .draw(target)
        .ok();
    Circle::new(Point::new(22, 39), 20)
        .into_styled(black_fill)
        .draw(target)
        .ok();
    for point in [
        Point::new(178, 43),
        Point::new(164, 65),
        Point::new(185, 78),
    ] {
        Line::new(point + Point::new(-2, 0), point + Point::new(2, 0))
            .into_styled(white_line)
            .draw(target)
            .ok();
        Line::new(point + Point::new(0, -2), point + Point::new(0, 2))
            .into_styled(white_line)
            .draw(target)
            .ok();
    }
}

fn draw_thermometer_icon(
    target: &mut Framebuffer,
    center: Point,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(center + Point::new(-2, -9), Size::new(5, 12))
        .into_styled(thin)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-4, 0), 9)
        .into_styled(thin)
        .draw(target)
        .ok();
    Rectangle::new(center + Point::new(0, -5), Size::new(1, 8))
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-2, 2), 5)
        .into_styled(filled)
        .draw(target)
        .ok();
}

fn draw_drop_icon(target: &mut Framebuffer, center: Point, filled: PrimitiveStyle<BinaryColor>) {
    Triangle::new(
        center + Point::new(0, -9),
        center + Point::new(-5, 1),
        center + Point::new(5, 1),
    )
    .into_styled(filled)
    .draw(target)
    .ok();
    Circle::new(center + Point::new(-5, -1), 11)
        .into_styled(filled)
        .draw(target)
        .ok();
}

fn draw_weather_icon(
    target: &mut Framebuffer,
    kind: Option<WeatherKind>,
    center: Point,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    match kind {
        Some(WeatherKind::Sunny) => draw_weather_sun(target, center, thin),
        Some(WeatherKind::Cloudy) => draw_outline_cloud(target, center, thin),
        Some(WeatherKind::Fog) => {
            draw_outline_cloud(target, center + Point::new(0, -4), thin);
            for (x1, x2, y) in [(-15, 15, 7), (-11, 11, 11), (-15, 15, 15)] {
                Line::new(center + Point::new(x1, y), center + Point::new(x2, y))
                    .into_styled(thin)
                    .draw(target)
                    .ok();
            }
        }
        Some(WeatherKind::Rain) => {
            draw_filled_cloud(target, center + Point::new(0, -3), filled);
            for x in [-9, 0, 9] {
                Line::new(
                    center + Point::new(x + 2, 6),
                    center + Point::new(x - 1, 13),
                )
                .into_styled(thin)
                .draw(target)
                .ok();
            }
        }
        Some(WeatherKind::Snow) => {
            draw_filled_cloud(target, center + Point::new(0, -3), filled);
            for x in [-9, 0, 9] {
                Line::new(
                    center + Point::new(x - 2, 11),
                    center + Point::new(x + 2, 11),
                )
                .into_styled(thin)
                .draw(target)
                .ok();
                Line::new(center + Point::new(x, 9), center + Point::new(x, 13))
                    .into_styled(thin)
                    .draw(target)
                    .ok();
            }
        }
        None => {
            let style = MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On);
            let centered = TextStyleBuilder::new()
                .alignment(Alignment::Center)
                .baseline(Baseline::Top)
                .build();
            Text::with_text_style("?", center + Point::new(0, -6), style, centered)
                .draw(target)
                .ok();
        }
    }
}

fn draw_weather_sun(target: &mut Framebuffer, center: Point, thin: PrimitiveStyle<BinaryColor>) {
    Circle::new(center + Point::new(-6, -6), 13)
        .into_styled(thin)
        .draw(target)
        .ok();
    for (start, end) in [
        ((0, -11), (0, -8)),
        ((0, 7), (0, 11)),
        ((-11, 0), (-8, 0)),
        ((8, 0), (12, 0)),
        ((-8, -8), (-6, -6)),
        ((6, 6), (9, 9)),
        ((-8, 8), (-6, 6)),
        ((6, -6), (9, -9)),
    ] {
        Line::new(
            center + Point::new(start.0, start.1),
            center + Point::new(end.0, end.1),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }
}

fn draw_outline_cloud(target: &mut Framebuffer, center: Point, thin: PrimitiveStyle<BinaryColor>) {
    Circle::new(center + Point::new(-15, -5), 11)
        .into_styled(thin)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-8, -10), 16)
        .into_styled(thin)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(5, -5), 11)
        .into_styled(thin)
        .draw(target)
        .ok();
    Line::new(center + Point::new(-14, 5), center + Point::new(15, 5))
        .into_styled(thin)
        .draw(target)
        .ok();
}

fn draw_filled_cloud(target: &mut Framebuffer, center: Point, filled: PrimitiveStyle<BinaryColor>) {
    Circle::new(center + Point::new(-15, -5), 11)
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-8, -10), 16)
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(5, -5), 11)
        .into_styled(filled)
        .draw(target)
        .ok();
    Rectangle::new(center + Point::new(-10, 0), Size::new(21, 6))
        .into_styled(filled)
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
    battery: Option<BatteryReading>,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(Point::new(171, 6), Size::new(23, 12))
        .into_styled(thin)
        .draw(target)
        .ok();
    Rectangle::new(Point::new(194, 10), Size::new(3, 4))
        .into_styled(filled)
        .draw(target)
        .ok();

    if let Some(battery) = battery {
        Rectangle::new(
            Point::new(173, 8),
            Size::new(battery_fill_width(battery.percent), 8),
        )
        .into_styled(filled)
        .draw(target)
        .ok();
    }
}

fn draw_battery_status(
    target: &mut Framebuffer,
    status: BatteryStatus,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    if status.usb_powered {
        draw_lightning(target, filled);
    }
    draw_battery(target, status.reading, thin, filled);
}

fn battery_fill_width(percent: u8) -> u32 {
    match percent {
        0..=10 => 2,
        11..=25 => 5,
        26..=50 => 10,
        51..=75 => 14,
        _ => 18,
    }
}

fn draw_lightning(target: &mut Framebuffer, filled: PrimitiveStyle<BinaryColor>) {
    // Two overlapping filled triangles form a legible bolt at status-bar size.
    Triangle::new(Point::new(164, 5), Point::new(157, 12), Point::new(163, 12))
        .into_styled(filled)
        .draw(target)
        .ok();
    Triangle::new(Point::new(162, 10), Point::new(160, 19), Point::new(169, 9))
        .into_styled(filled)
        .draw(target)
        .ok();
}
