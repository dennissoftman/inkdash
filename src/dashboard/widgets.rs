use std::convert::Infallible;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::mono_font::iso_8859_5::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle, Triangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use crate::battery::{BatteryReading, BatteryStatus};
use crate::datetime::DateTime;
use crate::ink_stacks::{Framebuffer, Stack, StackItem, Widget};
use crate::language::Language;
use crate::power::PowerSource;
use crate::shtc3::ClimateReading;
use crate::weather::{ForecastPeriod, Weather, WeatherKind};
use crate::wifi::wifi_signal_bars;

use super::backdrops;
use super::style::{
    center_x, centered_top, filled, large_value_style, left_top, small, thin, value_style,
};
use super::{DashboardData, DashboardScreen};

pub fn render(framebuffer: &mut Framebuffer, data: &DashboardData, screen: DashboardScreen) {
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
            language: self.data.language,
        };
        let battery = BatteryWidget {
            battery: self.data.battery,
            power_source: self.data.power_source,
        };
        let children = [StackItem::fill(&wifi, 1), StackItem::fixed(&battery, 44)];
        Stack::horizontal(&children)
            .with_spacing(1)
            .draw(target, bounds);

        if self.data.update_available {
            // Between the network name, which is clipped to twenty characters,
            // and the battery, which owns the last 44 pixels.
            draw_update_badge(target, bounds.top_left + Point::new(144, 6), filled());
        }

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
    language: Language,
}

impl Widget for WifiBadge<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_wifi_badge(
            target,
            bounds,
            self.connected,
            self.signal_dbm,
            self.ssid,
            self.language,
            filled(),
        );
    }
}

struct BatteryWidget {
    battery: BatteryStatus,
    power_source: PowerSource,
}

impl Widget for BatteryWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_battery_status(
            target,
            bounds,
            self.battery,
            self.power_source,
            thin(),
            filled(),
        );
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
        let clock = ClockWidget {
            time: self.data.time,
            language: self.data.language,
            sky: sky_for(self.data.weather.as_deref()),
        };
        let indoor = IndoorWidget {
            reading: self.data.climate,
            language: self.data.language,
        };
        let separator = VerticalRule {
            top_inset: 9,
            bottom_inset: 0,
        };
        let weather = CurrentWeatherWidget {
            weather: self.data.weather.as_deref(),
            language: self.data.language,
        };
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

struct ClockWidget {
    time: Option<DateTime>,
    language: Language,
    sky: backdrops::Sky,
}

/// Translates the current conditions into the scene the artwork draws. Without
/// weather yet, the hour's own clear-sky scene is the right thing to show.
pub(super) fn sky_for(weather: Option<&Weather>) -> backdrops::Sky {
    match weather.map(|value| value.kind()) {
        None | Some(WeatherKind::Sunny) => backdrops::Sky::Clear,
        Some(WeatherKind::Cloudy) => backdrops::Sky::Cloudy,
        Some(WeatherKind::Fog) => backdrops::Sky::Fog,
        Some(WeatherKind::Rain) => backdrops::Sky::Rain,
        Some(WeatherKind::Snow) => backdrops::Sky::Snow,
        Some(WeatherKind::Storm) => backdrops::Sky::Storm,
    }
}

impl Widget for ClockWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let time = self
            .time
            .map(|value| format!("{:02}:{:02}", value.hour, value.minute))
            .unwrap_or_else(|| "--:--".to_owned());
        let color = match self.time {
            Some(time) => backdrops::draw(
                target,
                bounds,
                backdrops::Slot::from_hour(time.hour),
                self.sky,
            ),
            None => backdrops::draw_blank(target, bounds),
        };
        draw_double_size_time(
            target,
            &time,
            Point::new(center_x(bounds), bounds.top_left.y + 11),
            color,
        );
        let date = self
            .time
            .map(|value| value.short_date(self.language))
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

struct IndoorWidget {
    reading: Option<ClimateReading>,
    language: Language,
}

impl Widget for IndoorWidget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        Text::with_text_style(
            self.language.translations().indoor,
            Point::new(center_x(bounds), bounds.top_left.y + 10),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        let temperature = self
            .reading
            .map(|reading| format!("{:.1} C", reading.temperature_c))
            .unwrap_or_else(|| "--.- C".to_owned());
        let humidity = self
            .reading
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

struct CurrentWeatherWidget<'a> {
    weather: Option<&'a Weather>,
    language: Language,
}

impl Widget for CurrentWeatherWidget<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        draw_weather_widget(target, bounds, self.weather, self.language);
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
    language: Language,
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

    // Reserve the right side for the battery indicator.
    let label = compact_ssid(ssid, language.translations().no_wifi);
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

fn draw_weather_widget(
    target: &mut Framebuffer,
    bounds: Rectangle,
    weather: Option<&Weather>,
    language: Language,
) {
    let center = center_x(bounds);
    let text = language.translations();
    Text::with_text_style(
        text.weather,
        Point::new(center, bounds.top_left.y + 10),
        small(),
        centered_top(),
    )
    .draw(target)
    .ok();
    draw_weather_icon(
        target,
        weather.map(|value| value.kind()),
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

    let average = weather
        .map(|value| format!("{} {:.1} C", text.average, value.today.mean_temperature_c))
        .unwrap_or_else(|| format!("{} --.- C", text.average));
    Text::with_text_style(
        &average,
        Point::new(center, bounds.top_left.y + 61),
        small(),
        centered_top(),
    )
    .draw(target)
    .ok();

    let details = weather
        .map(
            |value| match value.today.precipitation_probability_percent {
                Some(rain) => format!(
                    "{}{}% {}{}%",
                    text.humidity_short, value.relative_humidity_percent, text.rain_short, rain
                ),
                None => format!(
                    "{}{}% {}--%",
                    text.humidity_short, value.relative_humidity_percent, text.rain_short
                ),
            },
        )
        .unwrap_or_else(|| format!("{}--% {}--%", text.humidity_short, text.rain_short));
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
        let weather = self.data.weather.as_deref();
        let language = self.data.language;
        let text = language.translations();
        let title = weather_location_title(weather, language);
        Text::with_text_style(
            &title,
            Point::new(center_x(bounds), y + 2),
            value_style(),
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
            weather.map(|value| value.kind()),
            Point::new(x + 47, y + 42),
            thin(),
            filled(),
        );
        Text::with_text_style(
            weather
                .map(|value| language.condition(value.weather_code))
                .unwrap_or(text.no_data),
            Point::new(x + 47, y + 59),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        let rain = weather
            .and_then(|value| value.today.precipitation_probability_percent)
            .map(|value| format!("{} {value}%", text.rain))
            .unwrap_or_else(|| format!("{} --%", text.rain));
        Text::with_text_style(&rain, Point::new(x + 47, y + 70), small(), centered_top())
            .draw(target)
            .ok();
        let temperature = weather
            .map(|value| format!("{:.1} C", value.temperature_c))
            .unwrap_or_else(|| "--.- C".to_owned());
        Text::with_text_style(
            text.now,
            Point::new(x + 142, y + 21),
            small(),
            centered_top(),
        )
        .draw(target)
        .ok();
        Text::with_text_style(
            &temperature,
            Point::new(x + 142, y + 33),
            large_value_style(),
            centered_top(),
        )
        .draw(target)
        .ok();
        draw_drop_icon(target, Point::new(x + 120, y + 62), filled());
        let humidity = weather
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

        let day = weather.map(|value| value.today);
        let average = MetricWidget {
            label: text.average,
            value: day.map(|value| format!("{:.1} C", value.mean_temperature_c)),
        };
        let low = MetricWidget {
            label: text.low,
            value: day.map(|value| format!("{:.0} C", value.minimum_temperature_c)),
        };
        let high = MetricWidget {
            label: text.high,
            value: day.map(|value| format!("{:.0} C", value.maximum_temperature_c)),
        };
        let metrics = [
            StackItem::fill(&low, 1),
            StackItem::fill(&average, 1),
            StackItem::fill(&high, 1),
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
            large_value_style(),
            centered_top(),
        )
        .draw(target)
        .ok();
    }
}

struct ForecastScreen<'a> {
    weather: Option<&'a Weather>,
    language: Language,
}

impl<'a> ForecastScreen<'a> {
    fn new(data: &'a DashboardData) -> Self {
        Self {
            weather: data.weather.as_deref(),
            language: data.language,
        }
    }
}

impl Widget for ForecastScreen<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let text = self.language.translations();
        let header = SectionHeader(weather_page_title(text.tomorrow, self.weather));
        let forecast = TomorrowPeriodsWidget {
            periods: self.weather.map(|value| &value.tomorrow_periods),
        };
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

struct SectionHeader(String);

impl Widget for SectionHeader {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        Text::with_text_style(
            &self.0,
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

struct TomorrowPeriodsWidget<'a> {
    periods: Option<&'a [ForecastPeriod; 4]>,
}

impl Widget for TomorrowPeriodsWidget<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        let labels = ["08:00", "12:00", "18:00", "23:00"];
        let row_height = bounds.size.height as i32 / 4;
        for (index, label) in labels.iter().enumerate() {
            let y = bounds.top_left.y + row_height * index as i32;
            let period = self.periods.map(|periods| periods[index]);
            Text::with_baseline(
                label,
                Point::new(bounds.top_left.x + 4, y + row_height / 2),
                small(),
                Baseline::Middle,
            )
            .draw(target)
            .ok();
            draw_weather_icon(
                target,
                period.map(|value| value.kind()),
                Point::new(bounds.top_left.x + 78, y + row_height / 2),
                thin(),
                filled(),
            );
            let temperature = period
                .map(|value| format!("{:.0} C", value.temperature_c))
                .unwrap_or_else(|| "-- C".to_owned());
            Text::with_text_style(
                &temperature,
                Point::new(bounds.top_left.x + 175, y + 9),
                value_style(),
                centered_top(),
            )
            .draw(target)
            .ok();
            if let Some(rain) = period
                .and_then(|value| value.precipitation_probability_percent)
                .filter(|value| *value > 0)
            {
                draw_rain_probability_icon(
                    target,
                    Point::new(bounds.top_left.x + 115, y + row_height / 2),
                    filled(),
                    thin(),
                );
                Text::with_baseline(
                    &format!("{rain}%"),
                    Point::new(bounds.top_left.x + 125, y + row_height / 2),
                    small(),
                    Baseline::Middle,
                )
                .draw(target)
                .ok();
            }
            if index < labels.len() - 1 {
                Line::new(
                    Point::new(bounds.top_left.x + 4, y + row_height - 1),
                    Point::new(
                        bounds.top_left.x + bounds.size.width as i32 - 5,
                        y + row_height - 1,
                    ),
                )
                .into_styled(thin())
                .draw(target)
                .ok();
            }
        }
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

/// An arrow dropping into a tray: a background check has found a newer release.
/// Installing it stays deliberate, with BOOT+PWR.
/// A boxed "E" reporting that the CPU is actually sleeping between events.
///
/// Sized and placed to occupy the lightning bolt's slot, which it is mutually
/// exclusive with.
fn draw_eco_badge(
    target: &mut Framebuffer,
    top_left: Point,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(top_left, Size::new(11, 13))
        .into_styled(thin)
        .draw(target)
        .ok();
    Text::with_baseline(
        "E",
        top_left + Point::new(3, 2),
        MonoTextStyle::new(&FONT_6X10, filled.stroke_color.unwrap_or(BinaryColor::On)),
        Baseline::Top,
    )
    .draw(target)
    .ok();
}

fn draw_update_badge(
    target: &mut Framebuffer,
    top_left: Point,
    filled: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(top_left + Point::new(3, 0), Size::new(3, 5))
        .into_styled(filled)
        .draw(target)
        .ok();
    Triangle::new(
        top_left + Point::new(0, 4),
        top_left + Point::new(8, 4),
        top_left + Point::new(4, 9),
    )
    .into_styled(filled)
    .draw(target)
    .ok();
    Rectangle::new(top_left + Point::new(0, 10), Size::new(9, 2))
        .into_styled(filled)
        .draw(target)
        .ok();
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

fn draw_rain_probability_icon(
    target: &mut Framebuffer,
    center: Point,
    filled: PrimitiveStyle<BinaryColor>,
    thin: PrimitiveStyle<BinaryColor>,
) {
    Rectangle::new(center + Point::new(-7, -4), Size::new(14, 5))
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-7, -8), 8)
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(-2, -11), 10)
        .into_styled(filled)
        .draw(target)
        .ok();
    Circle::new(center + Point::new(3, -7), 7)
        .into_styled(filled)
        .draw(target)
        .ok();
    for x in [-4, 3] {
        Line::new(center + Point::new(x + 1, 3), center + Point::new(x - 1, 7))
            .into_styled(thin)
            .draw(target)
            .ok();
    }
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
        Some(WeatherKind::Storm) => {
            draw_filled_cloud(target, center + Point::new(0, -3), filled);
            for x in [-10_i32, 9] {
                Line::new(
                    center + Point::new(x + 2, 6),
                    center + Point::new(x - 1, 13),
                )
                .into_styled(thin)
                .draw(target)
                .ok();
            }
            // A bolt between the outer streaks reads as thunder at icon size.
            Triangle::new(
                center + Point::new(1, 5),
                center + Point::new(-4, 12),
                center + Point::new(1, 12),
            )
            .into_styled(filled)
            .draw(target)
            .ok();
            Triangle::new(
                center + Point::new(-1, 10),
                center + Point::new(4, 8),
                center + Point::new(-2, 16),
            )
            .into_styled(filled)
            .draw(target)
            .ok();
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

fn compact_ssid(ssid: Option<&str>, missing_label: &str) -> String {
    const MAX_CHARS: usize = 20;

    let Some(ssid) = ssid else {
        return missing_label.to_owned();
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

fn weather_page_title(prefix: &str, weather: Option<&Weather>) -> String {
    const MAX_CHARS: usize = 32;

    let Some(weather) = weather else {
        return prefix.to_owned();
    };
    let location_width = MAX_CHARS.saturating_sub(prefix.chars().count() + 1);
    format!(
        "{prefix} {}",
        compact_location(
            weather.location.city(),
            weather.location.country_code(),
            location_width
        )
    )
}

fn weather_location_title(weather: Option<&Weather>, language: Language) -> String {
    // FONT_9X15_BOLD is nine pixels wide; 21 characters leave a small margin.
    const MAX_CHARS: usize = 21;

    weather
        .map(|weather| {
            compact_location(
                weather.location.city(),
                weather.location.country_code(),
                MAX_CHARS,
            )
        })
        .unwrap_or_else(|| language.translations().location.to_owned())
}

fn compact_location(city: &str, country: &str, max_chars: usize) -> String {
    const SEPARATOR_WIDTH: usize = 2;

    let city_length = city.chars().count();
    let country_length = country.chars().count();
    if city_length + SEPARATOR_WIDTH + country_length <= max_chars {
        return format!("{city}, {country}");
    }

    let available = max_chars.saturating_sub(SEPARATOR_WIDTH);
    let country_width = country_length.min(available);
    let city_width = available.saturating_sub(country_width);
    format!(
        "{}, {}",
        compact_component(city, city_width),
        compact_component(country, country_width)
    )
}

fn compact_component(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let mut compact: String = value.chars().take(max_chars - 3).collect();
    compact.push_str("...");
    compact
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
    power_source: PowerSource,
    thin: PrimitiveStyle<BinaryColor>,
    filled: PrimitiveStyle<BinaryColor>,
) {
    // The slot left of the battery carries one meaning at a time. External
    // power outranks eco: while charging, how the board idles is the less
    // interesting half, and the two never need to be read together.
    // Derived here rather than carried in the dashboard state: as a stored
    // field it went stale the instant the source changed, because the power
    // handler updates the source and renders without a full sensor refresh --
    // so unplugging dropped the bolt and drew no badge at all.
    if power_source.is_external() {
        draw_lightning(target, bounds, filled);
    } else {
        draw_eco_badge(target, bounds.top_left + Point::new(1, 5), thin, filled);
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
