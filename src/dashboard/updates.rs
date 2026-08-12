//! The firmware update screens, which take over the whole panel.
//!
//! Every state here is shown for at most a minute or two during an update, so
//! the module depends on nothing but the framebuffer, `style`, and
//! `embedded-graphics`: `tools/backdrop-preview` includes it and draws all of
//! them on a host, which is the only practical way to iterate on screens that
//! otherwise appear once per release.

use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, Rectangle};
use embedded_graphics::text::Text;

use super::style::{center_x, centered_top, filled, large_value_style, small, thin, value_style};
use crate::ink_stacks::Framebuffer;

/// What the panel shows while an update is being looked for or installed.
/// `Hidden` leaves the panel to the dashboard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Screen {
    Hidden,
    Checking,
    Available {
        version: String,
        size: usize,
    },
    /// Byte counts come along with the percentage: progress is reported once
    /// per ten percent, so a stalled transfer looks exactly like a slow one
    /// unless the screen says how far it actually got.
    Downloading {
        version: String,
        downloaded: usize,
        total: usize,
    },
    Finalizing {
        version: String,
    },
    Restarting {
        version: String,
    },
    UpToDate,
    Failed {
        message: String,
    },
}

impl Screen {
    pub const fn is_visible(&self) -> bool {
        !matches!(self, Self::Hidden)
    }

    pub const fn can_accept(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    pub const fn can_start_check(&self) -> bool {
        matches!(self, Self::Hidden | Self::UpToDate | Self::Failed { .. })
    }

    pub const fn can_cancel(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Available { .. } | Self::Downloading { .. }
        )
    }
}

/// The download and verification screens share this geometry, so moving from
/// one to the other changes the words rather than the layout.
const TITLE_Y: i32 = 57;
const HEADLINE_Y: i32 = 89;
const PROGRESS_BAR: Rectangle = Rectangle::new(Point::new(20, 124), Size::new(160, 18));
const DETAIL_Y: i32 = 148;
const FOOTER_Y: i32 = 172;

pub fn render(target: &mut Framebuffer, screen: &Screen, current_version: &str) {
    let bounds = target.bounds();
    let center = center_x(bounds);
    Text::with_text_style(
        "FIRMWARE UPDATE",
        Point::new(center, 18),
        value_style(),
        centered_top(),
    )
    .draw(target)
    .ok();
    Line::new(Point::new(18, 42), Point::new(181, 42))
        .into_styled(thin())
        .draw(target)
        .ok();

    match screen {
        Screen::Hidden => {}
        Screen::Checking => {
            centered(target, center, "Loading...", 78, large_value_style());
            centered(target, center, "Checking update manifest", 111, small());
            centered(target, center, "PWR  Cancel", FOOTER_Y, small());
        }
        Screen::Available { version, size } => {
            centered(target, center, "Update available", TITLE_Y, value_style());
            centered(
                target,
                center,
                &format!("v{version}"),
                82,
                large_value_style(),
            );
            centered(target, center, &size_label(*size), 112, small());
            centered(target, center, "Auto-cancel in 1 minute", 137, small());
            centered(
                target,
                center,
                "BOOT Update   PWR Cancel",
                FOOTER_Y,
                small(),
            );
        }
        Screen::Downloading {
            version,
            downloaded,
            total,
        } => {
            let percent = percent(*downloaded, *total);
            centered(
                target,
                center,
                &format!("Installing v{version}"),
                TITLE_Y,
                value_style(),
            );
            centered(
                target,
                center,
                &format!("{percent}%"),
                HEADLINE_Y,
                large_value_style(),
            );
            draw_progress_bar(target, percent, PROGRESS_BAR);
            centered(
                target,
                center,
                &transferred_label(*downloaded, *total),
                DETAIL_Y,
                small(),
            );
            centered(target, center, "PWR  Cancel", FOOTER_Y, small());
        }
        // `esp_ota_end` verifies the whole image in one blocking call that
        // reports nothing on the way, so the bar is full and the word under it
        // says what is happening. An animated bar here would be invented.
        Screen::Finalizing { version } => {
            centered(
                target,
                center,
                &format!("Finalizing v{version}"),
                TITLE_Y,
                value_style(),
            );
            centered(target, center, "Verifying", HEADLINE_Y, large_value_style());
            draw_progress_bar(target, 100, PROGRESS_BAR);
            centered(target, center, "Firmware written", DETAIL_Y, small());
            centered(target, center, "Do not power off", FOOTER_Y, value_style());
        }
        Screen::Restarting { version } => {
            centered(
                target,
                center,
                &format!("v{version} installed"),
                69,
                value_style(),
            );
            centered(target, center, "Restarting...", 105, large_value_style());
        }
        Screen::UpToDate => {
            centered(target, center, "Already up to date", 75, value_style());
            centered(
                target,
                center,
                &format!("v{current_version}"),
                105,
                large_value_style(),
            );
            centered(target, center, "PWR  Back", FOOTER_Y, small());
        }
        Screen::Failed { message } => {
            centered(target, center, "Update failed", 55, value_style());
            for (index, line) in compact_error_lines(message).iter().enumerate() {
                centered(target, center, line, 84 + index as i32 * 13, small());
            }
            centered(target, center, "PWR  Back", FOOTER_Y, small());
        }
    }
}

fn centered(
    target: &mut Framebuffer,
    center: i32,
    value: &str,
    y: i32,
    style: MonoTextStyle<'static, BinaryColor>,
) {
    Text::with_text_style(value, Point::new(center, y), style, centered_top())
        .draw(target)
        .ok();
}

fn draw_progress_bar(target: &mut Framebuffer, percent: u8, bounds: Rectangle) {
    bounds.into_styled(thin()).draw(target).ok();
    let width = bounds
        .size
        .width
        .saturating_sub(4)
        .saturating_mul(u32::from(percent.min(100)))
        / 100;
    if width > 0 {
        Rectangle::new(
            bounds.top_left + Point::new(2, 2),
            Size::new(width, bounds.size.height.saturating_sub(4)),
        )
        .into_styled(filled())
        .draw(target)
        .ok();
    }
}

fn percent(downloaded: usize, total: usize) -> u8 {
    if total == 0 {
        return 0;
    }
    ((downloaded.min(total) as u64 * 100) / total as u64) as u8
}

/// Sizes are shown the same way on every screen, so the total counted up during
/// the download is the number the user accepted on the offer before it.
fn size_label(bytes: usize) -> String {
    if bytes >= MEBIBYTE {
        format!("{:.2} MB", mebibytes(bytes))
    } else {
        format!("{:.0} KB", kibibytes(bytes))
    }
}

fn transferred_label(downloaded: usize, total: usize) -> String {
    if total >= MEBIBYTE {
        format!("{:.2} / {:.2} MB", mebibytes(downloaded), mebibytes(total))
    } else {
        format!("{:.0} / {:.0} KB", kibibytes(downloaded), kibibytes(total))
    }
}

const MEBIBYTE: usize = 1024 * 1024;

fn mebibytes(bytes: usize) -> f32 {
    bytes as f32 / MEBIBYTE as f32
}

fn kibibytes(bytes: usize) -> f32 {
    bytes as f32 / 1024.0
}

fn compact_error_lines(message: &str) -> Vec<String> {
    const LINE_LENGTH: usize = 29;
    const LINE_COUNT: usize = 5;
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in message.split_whitespace() {
        if !current.is_empty() && current.len() + word.len() + 1 > LINE_LENGTH {
            lines.push(std::mem::take(&mut current));
            if lines.len() == LINE_COUNT {
                break;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.extend(word.chars().take(LINE_LENGTH.saturating_sub(current.len())));
    }
    if lines.len() < LINE_COUNT && !current.is_empty() {
        lines.push(current);
    }
    lines
}
