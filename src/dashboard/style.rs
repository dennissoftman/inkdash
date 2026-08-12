//! The drawing vocabulary the dashboard screens share: three text sizes, two
//! stroke styles, and the alignments everything is laid out with.
//!
//! Like `backdrops`, the module depends on nothing but `embedded-graphics`, so
//! `tools/backdrop-preview` can include it and draw the real screens on a host.

use embedded_graphics::mono_font::iso_8859_5::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, TextStyle, TextStyleBuilder};

pub const fn thin() -> PrimitiveStyle<BinaryColor> {
    PrimitiveStyle::with_stroke(BinaryColor::On, 1)
}

pub const fn filled() -> PrimitiveStyle<BinaryColor> {
    PrimitiveStyle::with_fill(BinaryColor::On)
}

/// Labels, secondary readings, and button hints.
pub const fn small() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyle::new(&FONT_6X10, BinaryColor::On)
}

/// The default weight for a reading the screen is about.
pub const fn value_style() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyle::new(&FONT_9X15_BOLD, BinaryColor::On)
}

/// One reading per screen may claim this size; more than one flattens both.
pub const fn large_value_style() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyle::new(&FONT_10X20, BinaryColor::On)
}

pub fn centered_top() -> TextStyle {
    TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build()
}

pub fn left_top() -> TextStyle {
    TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build()
}

pub fn center_x(bounds: Rectangle) -> i32 {
    bounds.top_left.x + bounds.size.width as i32 / 2
}
