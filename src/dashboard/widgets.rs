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
use crate::ink_stacks::{Framebuffer, Stack, StackItem, Widget};
use crate::shtc3::ClimateReading;
use crate::weather::{DailyForecast, Weather, WeatherKind};
use crate::wifi::wifi_signal_bars;

use super::{DashboardData, DashboardScreen};

pub fn render(framebuffer: &mut Framebuffer, data: &DashboardData, screen: DashboardScreen) {
    framebuffer.clear(BinaryColor::Off);
    let status = StatusBar::new(data);
    let home = HomeScreen::new(data);
    let today = TodayScreen::new(data);
    let forecast = ForecastScreen::new(data);
    let page: &dyn Widget = match screen {
        DashboardScreen::Home => &home,
        DashboardScreen::Today => &today,
        DashboardScreen::Forecast => &forecast,
    };
    let children = [StackItem::fixed(&status, 26), StackItem::fill(page, 1)];
    let bounds = framebuffer.bounds();
    Stack::vertical(&children).draw(framebuffer, bounds);
}

struct StatusBar<'a> {
    data: &'a DashboardData,
}

impl<'a> StatusBar<'a> {
    const fn new(data: &'a DashboardData) -> Self {
        Self { data }
    }
}

impl Widget for StatusBar<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let wifi = WifiBadge {
            connected: self.data.wifi_connected,
            signal_dbm: self.data.wifi_signal_dbm,
            ssid: self.data.wifi_ssid.as_deref(),
        };
        let battery = BatteryWidget(self.data.battery);
        let children = [StackItem::fill(&wifi, 1), StackItem::fixed(&battery, 44)];
        Stack::horizontal(&children)
            .with_spacing(1)
            .draw(target, bounds);

        let y = bounds.top_left.y + bounds.size.height.saturating_sub(1) as i32;
        Line::new(
            Point::new(bounds.top_left.x + 4, y),
            Point::new(bounds.top_left.x + bounds.size.width as i32 - 5, y),
        )
        .into_styled(thin())
        .draw(target)
        .ok();
    }
}

struct WifiBadge<'a> {
    connected: bool,
    signal_dbm: Option<i8>,
    ssid: Option<&'a str>,
}

impl Widget for WifiBadge<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_wifi_badge(
            target,
            bounds,
            self.connected,
            self.signal_dbm,
            self.ssid,
            filled(),
        );
    }
}

struct BatteryWidget(BatteryStatus);

impl Widget for BatteryWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_battery_status(target, bounds, self.0, thin(), filled());
    }
}

struct HomeScreen<'a> {
    data: &'a DashboardData,
}

impl<'a> HomeScreen<'a> {
    const fn new(data: &'a DashboardData) -> Self {
        Self { data }
    }
}

impl Widget for HomeScreen<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let clock = ClockWidget(self.data.time);
        let indoor = IndoorWidget(self.data.climate);
        let separator = VerticalRule {
            top_inset: 9,
            bottom_inset: 0,
        };
        let weather = CurrentWeatherWidget(self.data.weather);
        let columns = [
            StackItem::fixed(&indoor, 100),
            StackItem::fixed(&separator, 1),
            StackItem::fill(&weather, 1),
        ];
        let content = Stack::horizontal(&columns);
        let indicator = PageIndicator { active: 0 };
        let rows = [
            StackItem::fixed(&clock, 80),
            StackItem::fixed(&content, 84),
            StackItem::fill(&indicator, 1),
        ];
        Stack::vertical(&rows).draw(target, bounds);
    }
}

struct ClockWidget(Option<DateTime>);

impl Widget for ClockWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let time = self
            .0
            .map(|value| format!("{:02}:{:02}", value.hour, value.minute))
            .unwrap_or_else(|| "--:--".to_owned());
        let color = draw_time_background(target, bounds, self.0, thin(), filled());
        draw_double_size_time(
            target,
            &time,
            Point::new(center_x(bounds), bounds.top_left.y + 11),
            color,
        );
        let date = self
            .0
            .map(|value| value.short_date())
            .unwrap_or_else(|| "--- -- ---".to_owned());
        Text::with_text_style(
            &date,
            Point::new(center_x(bounds), bounds.top_left.y + 52),
            MonoTextStyle::new(&FONT_9X15_BOLD, color),
            centered_top(),
        )
        .draw(target)
        .ok();

        let y = bounds.top_left.y + bounds.size.height as i32 - 1;
        Line::new(
            Point::new(bounds.top_left.x + 4, y),
            Point::new(bounds.top_left.x + bounds.size.width as i32 - 5, y),
        )
        .into_styled(thin())
        .draw(target)
        .ok();
    }
}

struct IndoorWidget(Option<ClimateReading>);

impl Widget for IndoorWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        Text::with_text_style(
            "INDOOR",
            Point::new(center_x(bounds), bounds.top_left.y + 10),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        let temperature = self
            .0
            .map(|reading| format!("{:.1} C", reading.temperature_c))
            .unwrap_or_else(|| "--.- C".to_owned());
        let humidity = self
            .0
            .map(|reading| format!("{:.0}%", reading.humidity_percent))
            .unwrap_or_else(|| "--%".to_owned());
        draw_thermometer_icon(
            target,
            bounds.top_left + Point::new(18, 37),
            thin(),
            filled(),
        );
        draw_drop_icon(target, bounds.top_left + Point::new(18, 60), filled());
        Text::with_text_style(
            &temperature,
            bounds.top_left + Point::new(31, 29),
            value_style(),
            left_top(),
        )
        .draw(target)
        .ok();
        Text::with_text_style(
            &humidity,
            bounds.top_left + Point::new(31, 54),
            value_style(),
            left_top(),
        )
        .draw(target)
        .ok();
    }
}

struct CurrentWeatherWidget(Option<Weather>);

impl Widget for CurrentWeatherWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_weather_widget(target, bounds, self.0);
    }
}

struct VerticalRule {
    top_inset: i32,
    bottom_inset: i32,
}

impl Widget for VerticalRule {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let x = bounds.top_left.x;
        Line::new(
            Point::new(x, bounds.top_left.y + self.top_inset),
            Point::new(
                x,
                bounds.top_left.y + bounds.size.height as i32 - self.bottom_inset,
            ),
        )
        .into_styled(thin())
        .draw(target)
        .ok();
    }
}

struct PageIndicator {
    active: usize,
}

impl Widget for PageIndicator {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_page_indicator(target, bounds, self.active, thin(), filled());
    }
}

const fn thin() -> PrimitiveStyle<BinaryColor> {
    PrimitiveStyle::with_stroke(BinaryColor::On, 1)
}

const fn filled() -> PrimitiveStyle<BinaryColor> {
    PrimitiveStyle::with_fill(BinaryColor::On)
}

const fn small() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyle::new(&FONT_6X10, BinaryColor::On)
}

const fn value_style() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On)
}

fn centered_top() -> embedded_graphics::text::TextStyle {
    TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build()
}

fn left_top() -> embedded_graphics::text::TextStyle {
    TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build()
}

fn center_x(bounds: Rectangle) -> i32 {
    bounds.top_left.x + bounds.size.width as i32 / 2
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
        let target_size = self.target.size();
        Size::new(
            target_size.width / self.scale as u32,
            target_size.height / self.scale as u32,
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
    bounds: Rectangle,
    connected: bool,
    signal_dbm: Option<i8>,
    ssid: Option<&str>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    let active_bars = wifi_signal_bars(connected, signal_dbm) as usize;
    for (index, height) in [3_i32, 6, 9].into_iter().enumerate() {
        let height = if index < active_bars { height } else { 1 };
        Rectangle::new(
            Point::new(
                bounds.top_left.x + 5 + index as i32 * 4,
                bounds.top_left.y + 17 - height,
            ),
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
    Text::with_baseline(
        &label,
        bounds.top_left + Point::new(20, 7),
        style,
        Baseline::Top,
    )
    .draw(target)
    .ok();
}

fn draw_weather_widget(target: &mut Framebuffer, bounds: Rectangle, weather: Option<Weather>) {
    let center = center_x(bounds);
    Text::with_text_style(
        "WEATHER",
        Point::new(center, bounds.top_left.y + 10),
        small(),
        centered_top(),
    )
    .draw(target)
    .ok();
    draw_weather_icon(
        target,
        weather.map(Weather::kind),
        Point::new(center, bounds.top_left.y + 30),
        thin(),
        filled(),
    );

    let temperature = weather
        .map(|value| format!("{:.1} C", value.temperature_c))
        .unwrap_or_else(|| "--.- C".to_owned());
    Text::with_text_style(
        &temperature,
        Point::new(center, bounds.top_left.y + 45),
        value_style(),
        centered_top(),
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
    Text::with_text_style(
        &range,
        Point::new(center, bounds.top_left.y + 61),
        small(),
        centered_top(),
    )
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
    Text::with_text_style(
        &details,
        Point::new(center, bounds.top_left.y + 72),
        small(),
        centered_top(),
    )
    .draw(target)
    .ok();
}

struct TodayScreen<'a> {
    data: &'a DashboardData,
}

impl<'a> TodayScreen<'a> {
    const fn new(data: &'a DashboardData) -> Self {
        Self { data }
    }
}

impl Widget for TodayScreen<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let content = TodayContent { data: self.data };
        let indicator = PageIndicator { active: 1 };
        let rows = [
            StackItem::fixed(&content, 164),
            StackItem::fill(&indicator, 1),
        ];
        Stack::vertical(&rows).draw(target, bounds);
    }
}

struct TodayContent<'a> {
    data: &'a DashboardData,
}

impl Widget for TodayContent<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let x = bounds.top_left.x;
        let y = bounds.top_left.y;
        let title = self
            .data
            .time
            .map(|value| format!("TODAY  {}", value.short_date()))
            .unwrap_or_else(|| "TODAY".to_owned());
        Text::with_text_style(
            &title,
            Point::new(center_x(bounds), y + 5),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        Line::new(
            Point::new(x + 8, y + 18),
            Point::new(x + bounds.size.width as i32 - 8, y + 18),
        )
        .into_styled(thin())
        .draw(target)
        .ok();

        draw_weather_icon(
            target,
            self.data.weather.map(Weather::kind),
            Point::new(x + 47, y + 42),
            thin(),
            filled(),
        );
        Text::with_text_style(
            self.data
                .weather
                .map(Weather::condition)
                .unwrap_or("NO DATA"),
            Point::new(x + 47, y + 59),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        let temperature = self
            .data
            .weather
            .map(|value| format!("{:.1} C", value.temperature_c))
            .unwrap_or_else(|| "--.- C".to_owned());
        Text::with_text_style(
            &temperature,
            Point::new(x + 142, y + 31),
            value_style(),
            centered_top(),
        )
        .draw(target)
        .ok();
        draw_drop_icon(target, Point::new(x + 120, y + 62), filled());
        let humidity = self
            .data
            .weather
            .map(|value| format!("{}%", value.relative_humidity_percent))
            .unwrap_or_else(|| "--%".to_owned());
        Text::with_text_style(
            &humidity,
            Point::new(x + 132, y + 55),
            value_style(),
            left_top(),
        )
        .draw(target)
        .ok();

        Line::new(
            Point::new(x + 8, y + 80),
            Point::new(x + bounds.size.width as i32 - 8, y + 80),
        )
        .into_styled(thin())
        .draw(target)
        .ok();
        for offset in [67, 133] {
            Line::new(
                Point::new(x + offset, y + 86),
                Point::new(x + offset, y + 146),
            )
            .into_styled(thin())
            .draw(target)
            .ok();
        }

        let day = self.data.weather.map(|value| value.today);
        let low = MetricWidget {
            label: "LOW",
            value: day.map(|value| format!("{:.0} C", value.minimum_temperature_c)),
        };
        let high = MetricWidget {
            label: "HIGH",
            value: day.map(|value| format!("{:.0} C", value.maximum_temperature_c)),
        };
        let rain = MetricWidget {
            label: "RAIN",
            value: day
                .and_then(|value| value.precipitation_probability_percent)
                .map(|value| format!("{value}%")),
        };
        let metrics = [
            StackItem::fill(&low, 1),
            StackItem::fill(&high, 1),
            StackItem::fill(&rain, 1),
        ];
        Stack::horizontal(&metrics).draw(
            target,
            Rectangle::new(Point::new(x + 1, y + 86), Size::new(198, 60)),
        );
    }
}

struct MetricWidget {
    label: &'static str,
    value: Option<String>,
}

impl Widget for MetricWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let position = Point::new(center_x(bounds), bounds.top_left.y + 6);
        Text::with_text_style(self.label, position, small(), centered_top())
            .draw(target)
            .ok();
        Text::with_text_style(
            self.value.as_deref().unwrap_or("--"),
            position + Point::new(0, 18),
            value_style(),
            centered_top(),
        )
        .draw(target)
        .ok();
    }
}

struct ForecastScreen {
    weather: Option<Weather>,
}

impl ForecastScreen {
    const fn new(data: &DashboardData) -> Self {
        Self {
            weather: data.weather,
        }
    }
}

impl Widget for ForecastScreen {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let header = SectionHeader("FORECAST");
        let today = ForecastDayWidget {
            label: "TODAY",
            day: self.weather.map(|value| value.today),
        };
        let separator = VerticalRule {
            top_inset: 0,
            bottom_inset: 12,
        };
        let tomorrow = ForecastDayWidget {
            label: "TOMORROW",
            day: self.weather.map(|value| value.tomorrow),
        };
        let columns = [
            StackItem::fixed(&today, 100),
            StackItem::fixed(&separator, 1),
            StackItem::fill(&tomorrow, 1),
        ];
        let forecast = Stack::horizontal(&columns);
        let content_rows = [StackItem::fixed(&header, 24), StackItem::fill(&forecast, 1)];
        let content = Stack::vertical(&content_rows);
        let indicator = PageIndicator { active: 2 };
        let rows = [
            StackItem::fixed(&content, 164),
            StackItem::fill(&indicator, 1),
        ];
        Stack::vertical(&rows).draw(target, bounds);
    }
}

struct SectionHeader(&'static str);

impl Widget for SectionHeader {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        Text::with_text_style(
            self.0,
            Point::new(center_x(bounds), bounds.top_left.y + 5),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        Line::new(
            bounds.top_left + Point::new(8, 18),
            Point::new(
                bounds.top_left.x + bounds.size.width as i32 - 8,
                bounds.top_left.y + 18,
            ),
        )
        .into_styled(thin())
        .draw(target)
        .ok();
    }
}

struct ForecastDayWidget {
    label: &'static str,
    day: Option<DailyForecast>,
}

impl Widget for ForecastDayWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let center = center_x(bounds);
        let y = bounds.top_left.y;
        Text::with_text_style(
            self.label,
            Point::new(center, y + 1),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        draw_weather_icon(
            target,
            self.day.map(|value| value.kind()),
            Point::new(center, y + 30),
            thin(),
            filled(),
        );
        Text::with_text_style(
            self.day.map(|value| value.condition()).unwrap_or("NO DATA"),
            Point::new(center, y + 47),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();

        let high = self
            .day
            .map(|value| format!("{:.0} C", value.maximum_temperature_c))
            .unwrap_or_else(|| "-- C".to_owned());
        let low = self
            .day
            .map(|value| format!("{:.0} C", value.minimum_temperature_c))
            .unwrap_or_else(|| "-- C".to_owned());
        let rain = self
            .day
            .and_then(|value| value.precipitation_probability_percent)
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "--%".to_owned());
        for (label, value, label_y, value_y) in [("HIGH", &high, 63, 75), ("LOW", &low, 93, 105)] {
            Text::with_text_style(
                label,
                Point::new(center, y + label_y),
                small(),
                centered_top(),
            )
            .draw(target)
            .ok();
            Text::with_text_style(
                value,
                Point::new(center, y + value_y),
                value_style(),
                centered_top(),
            )
            .draw(target)
            .ok();
        }
        Text::with_text_style(
            &format!("RAIN {rain}"),
            Point::new(center, y + 124),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
    }
}

fn draw_page_indicator(
    target: &mut Framebuffer,
    bounds: Rectangle,
    active: usize,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    for index in 0..3 {
        Circle::new(
            Point::new(
                center_x(bounds) - 9 + index as i32 * 8,
                bounds.top_left.y + 4,
            ),
            4,
        )
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
    bounds: Rectangle,
    time: Option<DateTime>,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) -> BinaryColor {
    let card = Rectangle::new(
        bounds.top_left + Point::new(4, 3),
        Size::new(bounds.size.width.saturating_sub(8), 72),
    );
    let Some(period) = time.map(|value| TimeOfDay::from_hour(value.hour)) else {
        card.into_styled(thin).draw(target).ok();
        return BinaryColor::On;
    };

    match period {
        TimeOfDay::Morning => {
            card.into_styled(thin).draw(target).ok();
            draw_sunrise(target, bounds, false, thin);
            BinaryColor::On
        }
        TimeOfDay::Day => {
            card.into_styled(thin).draw(target).ok();
            draw_day_sky(target, bounds, thin);
            BinaryColor::On
        }
        TimeOfDay::Evening => {
            card.into_styled(thin).draw(target).ok();
            draw_sunrise(target, bounds, true, thin);
            BinaryColor::On
        }
        TimeOfDay::Night => {
            card.into_styled(filled).draw(target).ok();
            draw_night_sky(target, bounds);
            BinaryColor::Off
        }
    }
}

fn draw_sunrise(
    target: &mut Framebuffer,
    bounds: Rectangle,
    evening: bool,
    thin: PrimitiveStyle<BinaryColor>,
) {
    let center_x = if evening {
        bounds.top_left.x + bounds.size.width as i32 - 23
    } else {
        bounds.top_left.x + 22
    };
    Circle::new(Point::new(center_x - 10, bounds.top_left.y + 57), 21)
        .into_styled(thin)
        .draw(target)
        .ok();
    for y in [96, 99] {
        let y = bounds.top_left.y + y - 26;
        Line::new(
            Point::new(bounds.top_left.x + 8, y),
            Point::new(bounds.top_left.x + bounds.size.width as i32 - 8, y),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }
    for (dx, dy) in [(-15, -8), (-11, -15), (0, -19), (11, -15), (15, -8)] {
        Line::new(
            Point::new(center_x + dx, bounds.top_left.y + 67 + dy),
            Point::new(center_x + dx / 2, bounds.top_left.y + 67 + dy / 2),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }
}

fn draw_day_sky(target: &mut Framebuffer, bounds: Rectangle, thin: PrimitiveStyle<BinaryColor>) {
    let right = bounds.top_left.x + bounds.size.width as i32;
    let y = bounds.top_left.y;
    Circle::new(Point::new(right - 27, y + 16), 13)
        .into_styled(thin)
        .draw(target)
        .ok();
    for (start, end) in [
        ((-21, 11), (-21, 14)),
        ((-21, 30), (-21, 33)),
        ((-32, 22), (-29, 22)),
        ((-13, 22), (-10, 22)),
        ((-29, 14), (-27, 16)),
        ((-15, 28), (-13, 30)),
        ((-29, 30), (-27, 28)),
        ((-15, 16), (-13, 14)),
    ] {
        Line::new(
            Point::new(right + start.0, y + start.1),
            Point::new(right + end.0, y + end.1),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }
    draw_outline_cloud(target, bounds.top_left + Point::new(20, 53), thin);
}

fn draw_night_sky(target: &mut Framebuffer, bounds: Rectangle) {
    let white_fill = PrimitiveStyle::with_fill(BinaryColor::Off);
    let black_fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let white_line = PrimitiveStyle::with_stroke(BinaryColor::Off, 1);
    Circle::new(bounds.top_left + Point::new(15, 17), 20)
        .into_styled(white_fill)
        .draw(target)
        .ok();
    Circle::new(bounds.top_left + Point::new(22, 13), 20)
        .into_styled(black_fill)
        .draw(target)
        .ok();
    for point in [
        bounds.top_left + Point::new(bounds.size.width as i32 - 22, 17),
        bounds.top_left + Point::new(bounds.size.width as i32 - 36, 39),
        bounds.top_left + Point::new(bounds.size.width as i32 - 15, 52),
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
    bounds: Rectangle,
    battery: Option<BatteryReading>,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(bounds.top_left + Point::new(15, 6), Size::new(23, 12))
        .into_styled(thin)
        .draw(target)
        .ok();
    Rectangle::new(bounds.top_left + Point::new(38, 10), Size::new(3, 4))
        .into_styled(filled)
        .draw(target)
        .ok();

    if let Some(battery) = battery {
        Rectangle::new(
            bounds.top_left + Point::new(17, 8),
            Size::new(battery_fill_width(battery.percent), 8),
        )
        .into_styled(filled)
        .draw(target)
        .ok();
    }
}

fn draw_battery_status(
    target: &mut Framebuffer,
    bounds: Rectangle,
    status: BatteryStatus,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    if status.usb_powered {
        draw_lightning(target, bounds, filled);
    }
    draw_battery(target, bounds, status.reading, thin, filled);
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

fn draw_lightning(
    target: &mut Framebuffer,
    bounds: Rectangle,
    filled: PrimitiveStyle<BinaryColor>,
) {
    // Two overlapping filled triangles form a legible bolt at status-bar size.
    Triangle::new(
        bounds.top_left + Point::new(8, 5),
        bounds.top_left + Point::new(1, 12),
        bounds.top_left + Point::new(7, 12),
    )
    .into_styled(filled)
    .draw(target)
    .ok();
    Triangle::new(
        bounds.top_left + Point::new(6, 10),
        bounds.top_left + Point::new(4, 19),
        bounds.top_left + Point::new(13, 9),
    )
    .into_styled(filled)
    .draw(target)
    .ok();
}
