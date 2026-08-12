//! Time-of-day and weather artwork for the clock card.
//!
//! A scene is composed from the hour and the current conditions: the hour picks
//! the sky, the sun or moon, and the ground, and the conditions add cloud,
//! precipitation, or fog over them. Everything draws into the 200 x 80 clock
//! area using coordinates relative to its top-left corner, and stays clear of
//! the two text zones the clock and date occupy.
//!
//! The module deliberately depends on nothing but the framebuffer and
//! `embedded-graphics`, so `tools/backdrop-preview` can render it on a host.

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, Line, Polyline, PrimitiveStyle, Rectangle, RoundedRectangle, Triangle,
};

use crate::ink_stacks::Framebuffer;

/// The card border inset from the clock area, and the card size it produces.
const CARD_OFFSET: Point = Point::new(4, 3);
const CARD_SIZE: Size = Size::new(192, 72);

/// Interior of the card, one pixel inside its border on every side.
const INTERIOR_LEFT: i32 = 5;
const INTERIOR_RIGHT: i32 = 194;
const INTERIOR_TOP: i32 = 4;
const INTERIOR_BOTTOM: i32 = 73;

/// Pixels the clock digits and the date claim, padded so artwork never crowds a
/// glyph. Kept in sync with `ClockWidget`.
const TEXT_LEFT: i32 = 48;
const TEXT_TOP: i32 = 9;
const TEXT_RIGHT: i32 = 152;
const TEXT_BOTTOM: i32 = 69;

// Custom backdrops: a bitmap of exactly CUSTOM_WIDTH x CUSTOM_HEIGHT pixels,
// one bit per pixel, row-major, most significant bit leftmost, each row padded
// to a byte boundary, and a set bit is ink.
//
// Nothing in the firmware supplies one yet. Keeping artwork in NVS costs about
// 7 KB of a 24 KB partition per four slots, which is not a good trade, so the
// source is meant to be an SD card. The drawing side below is storage-agnostic
// and stays exercised by `tools/backdrop-preview`.
#[allow(dead_code)]
pub const CUSTOM_WIDTH: u32 = CARD_SIZE.width;
#[allow(dead_code)]
pub const CUSTOM_HEIGHT: u32 = CARD_SIZE.height;
#[allow(dead_code)]
pub const CUSTOM_ROW_BYTES: usize = CUSTOM_WIDTH as usize / 8;
#[allow(dead_code)]
pub const CUSTOM_BYTES: usize = CUSTOM_ROW_BYTES * CUSTOM_HEIGHT as usize;

/// Which artwork a given hour belongs to. The variants double as the slots
/// custom uploads are stored under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    Morning,
    Day,
    Evening,
    Night,
}

impl Slot {
    pub const fn from_hour(hour: u8) -> Self {
        match hour {
            5..=10 => Self::Morning,
            11..=16 => Self::Day,
            17..=20 => Self::Evening,
            _ => Self::Night,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Morning => "MORNING",
            Self::Day => "DAY",
            Self::Evening => "EVENING",
            Self::Night => "NIGHT",
        }
    }

    /// Whether the card is drawn inverted.
    const fn is_dark(self) -> bool {
        matches!(self, Self::Night)
    }
}

/// What the weather adds on top of the hour's scene.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Sky {
    #[default]
    Clear,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Storm,
}

impl Sky {
    // The firmware only ever draws the condition it was handed; enumerating them
    // is what `tools/backdrop-preview` needs to render its matrix.
    #[allow(dead_code)]
    pub const ALL: [Self; 6] = [
        Self::Clear,
        Self::Cloudy,
        Self::Fog,
        Self::Rain,
        Self::Snow,
        Self::Storm,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "CLEAR",
            Self::Cloudy => "CLOUDY",
            Self::Fog => "FOG",
            Self::Rain => "RAIN",
            Self::Snow => "SNOW",
            Self::Storm => "STORM",
        }
    }

    /// Whether a cloud bank covers part of the sun or moon.
    const fn has_cloud(self) -> bool {
        matches!(self, Self::Cloudy | Self::Rain | Self::Snow | Self::Storm)
    }

    /// Clear skies keep their rays, halo, birds, and stars; everything else
    /// loses them, which is most of what makes the conditions readable.
    const fn is_clear(self) -> bool {
        matches!(self, Self::Clear)
    }
}

/// How an uploaded bitmap wants the clock drawn over it.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Style {
    /// Draw the clock and date in white, for artwork with a dark background.
    pub light_text: bool,
    /// Clear a plate behind the clock and date. Artwork that already leaves the
    /// centre empty looks better without one.
    pub plate: bool,
}

/// Draws the built-in artwork for `slot` under `sky`, and returns the colour the
/// clock and date should use.
pub fn draw(target: &mut Framebuffer, bounds: Rectangle, slot: Slot, sky: Sky) -> BinaryColor {
    let ink = if slot.is_dark() {
        BinaryColor::Off
    } else {
        BinaryColor::On
    };
    card(target, bounds, slot.is_dark());

    let (top, settled, fade) = wash_levels(slot, sky);
    wash(target, bounds, top, settled, fade, ink);

    // The sun or moon first, so cloud and fog can pass in front of it.
    let luminary = luminary(target, bounds, slot, sky, ink);
    if sky.is_clear() {
        match slot {
            Slot::Morning => {
                bird(target, bounds, Point::new(160, 24), 4, ink);
                bird(target, bounds, Point::new(173, 16), 3, ink);
                bird(target, bounds, Point::new(182, 29), 4, ink);
            }
            Slot::Evening => {
                bird(target, bounds, Point::new(30, 15), 4, ink);
                bird(target, bounds, Point::new(43, 23), 3, ink);
                bird(target, bounds, Point::new(19, 26), 3, ink);
            }
            Slot::Day => {}
            Slot::Night => stars(target, bounds),
        }
    }

    if sky.has_cloud() {
        let (center, scale) = cloud_anchor(slot);
        cloud(target, bounds, center, scale, ink);
        if matches!(sky, Sky::Storm) {
            bolt(target, bounds, center + Point::new(2, scale + 4), ink);
        }
    }

    let ground = ground_profile(slot);
    match slot {
        Slot::Night => skyline(target, bounds),
        _ => {
            hills(target, bounds, ground);
            if matches!(slot, Slot::Day) && sky.is_clear() {
                for (x, height) in [(13_i32, 11_i32), (24, 15), (35, 9)] {
                    let base = Point::new(x, horizon(ground, x) + 1);
                    conifer(target, bounds, base, height);
                }
            }
        }
    }

    match sky {
        Sky::Rain | Sky::Storm => {
            precipitation(target, bounds, Drop::Rain, luminary, ground, ink);
            if !slot.is_dark() {
                puddles(target, bounds, ground);
            }
        }
        Sky::Snow => {
            precipitation(target, bounds, Drop::Flake, luminary, ground, ink);
            if !slot.is_dark() {
                snow_line(target, bounds, ground);
            }
        }
        Sky::Fog => fog(target, bounds, ink),
        Sky::Clear | Sky::Cloudy => {}
    }

    ink
}

/// Draws an empty card, for when the clock has no time to categorise.
pub fn draw_blank(target: &mut Framebuffer, bounds: Rectangle) -> BinaryColor {
    card(target, bounds, false);
    BinaryColor::On
}

/// Draws an uploaded bitmap of [`CUSTOM_BYTES`] bytes and returns the colour the
/// clock and date should use. The bitmap covers the card exactly, border
/// included, so artwork can bleed to the edge. Weather is not drawn over it.
#[allow(dead_code)]
pub fn draw_custom(
    target: &mut Framebuffer,
    bounds: Rectangle,
    bitmap: &[u8],
    style: Style,
) -> BinaryColor {
    let ink = if style.light_text {
        BinaryColor::Off
    } else {
        BinaryColor::On
    };
    let origin = bounds.top_left + CARD_OFFSET;
    for (row, bytes) in bitmap.chunks_exact(CUSTOM_ROW_BYTES).enumerate() {
        let y = origin.y + row as i32;
        target
            .draw_iter(bytes.iter().enumerate().flat_map(|(index, byte)| {
                let x = origin.x + index as i32 * 8;
                (0..8).map(move |bit| {
                    let color = if byte & (0x80 >> bit) == 0 {
                        BinaryColor::Off
                    } else {
                        BinaryColor::On
                    };
                    Pixel(Point::new(x + bit, y), color)
                })
            }))
            .ok();
    }

    if style.plate {
        plate(target, bounds, ink);
    }
    ink
}

fn card(target: &mut Framebuffer, bounds: Rectangle, dark: bool) {
    let style = if dark {
        PrimitiveStyle::with_fill(BinaryColor::On)
    } else {
        PrimitiveStyle::with_stroke(BinaryColor::On, 1)
    };
    Rectangle::new(bounds.top_left + CARD_OFFSET, CARD_SIZE)
        .into_styled(style)
        .draw(target)
        .ok();
}

/// Clears a rounded plate behind the text so glyphs stay legible over arbitrary
/// artwork, outlined in the ink colour to look deliberate.
fn plate(target: &mut Framebuffer, bounds: Rectangle, ink: BinaryColor) {
    let shape = RoundedRectangle::with_equal_corners(
        Rectangle::new(
            bounds.top_left + Point::new(TEXT_LEFT, TEXT_TOP),
            Size::new(
                (TEXT_RIGHT - TEXT_LEFT) as u32,
                (TEXT_BOTTOM - TEXT_TOP) as u32,
            ),
        ),
        Size::new_equal(5),
    );
    shape
        .into_styled(PrimitiveStyle::with_fill(ink.invert()))
        .draw(target)
        .ok();
    shape
        .into_styled(PrimitiveStyle::with_stroke(ink, 1))
        .draw(target)
        .ok();
}

// ---------------------------------------------------------------------------
// Scene layout
// ---------------------------------------------------------------------------

/// A disc that later elements must not draw over: centre and radius.
type Luminary = (Point, i32);

/// Sun or moon for the hour. Overcast skies drop the rays and the halo.
fn luminary(
    target: &mut Framebuffer,
    bounds: Rectangle,
    slot: Slot,
    sky: Sky,
    ink: BinaryColor,
) -> Luminary {
    let (center, diameter, rays) = match slot {
        Slot::Morning => (Point::new(25, 65), 17, RayStyle::Rising),
        Slot::Day => (Point::new(171, 25), 15, RayStyle::High),
        Slot::Evening => (Point::new(170, 63), 23, RayStyle::Setting),
        Slot::Night => (Point::new(26, 27), 31, RayStyle::None),
    };

    if matches!(slot, Slot::Night) {
        moon(target, bounds, center, diameter, sky.is_clear());
    } else {
        sun(
            target,
            bounds,
            center,
            diameter,
            if sky.is_clear() { rays } else { RayStyle::None },
            ink,
        );
    }
    (bounds.top_left + center, diameter as i32 / 2)
}

/// Where the weather's cloud bank sits, and how large it is. Each anchor keeps
/// the cloud clear of the clock while still touching the sun or moon.
const fn cloud_anchor(slot: Slot) -> (Point, i32) {
    match slot {
        Slot::Morning => (Point::new(172, 24), 9),
        Slot::Day => (Point::new(176, 33), 9),
        Slot::Evening => (Point::new(28, 24), 10),
        // Small and low, so the crescent still shows above it.
        Slot::Night => (Point::new(36, 41), 7),
    }
}

/// Sky texture, as `(density at the top, density where it settles, the row it
/// settles on)` out of sixteen. Conditions thicken the hour's own sky; fog
/// instead pools low. The cloud and the precipitation carry most of the meaning,
/// so the texture stays a hint: past roughly half density the card turns into a
/// grey slab and the clock loses its contrast.
const fn wash_levels(slot: Slot, sky: Sky) -> (u8, u8, i32) {
    // The inverted card wears white texture, which reads much heavier.
    if slot.is_dark() {
        return match sky {
            Sky::Clear => (0, 0, 40),
            Sky::Cloudy | Sky::Snow => (2, 1, 56),
            Sky::Fog => (2, 4, INTERIOR_BOTTOM),
            Sky::Rain | Sky::Storm => (3, 2, 60),
        };
    }

    let (top, settled, fade) = match slot {
        Slot::Morning => (6, 0, 48),
        Slot::Day | Slot::Night => (0, 0, 40),
        Slot::Evening => (9, 3, 64),
    };
    match sky {
        Sky::Clear => (top, settled, fade),
        Sky::Cloudy => (cap(top + 2, 8), cap(settled + 1, 4), 56),
        Sky::Fog => (2, 5, INTERIOR_BOTTOM),
        Sky::Snow => (cap(top + 2, 8), cap(settled + 1, 4), 60),
        Sky::Rain => (cap(top + 3, 9), cap(settled + 2, 5), 64),
        Sky::Storm => (cap(top + 4, 9), cap(settled + 2, 5), 64),
    }
}

const fn cap(value: u8, ceiling: u8) -> u8 {
    if value > ceiling {
        ceiling
    } else {
        value
    }
}

const fn ground_profile(slot: Slot) -> Profile {
    match slot {
        Slot::Morning => HILLS_MORNING,
        Slot::Day => HILLS_DAY,
        Slot::Evening => HILLS_EVENING,
        // Not drawn: it only keeps precipitation off the skyline.
        Slot::Night => SKYLINE_CLEARANCE,
    }
}

// ---------------------------------------------------------------------------
// Sun, moon, and stars
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum RayStyle {
    /// Long rays fanned above the horizon.
    Rising,
    /// Eight rays all round, alternating length.
    High,
    /// A few short rays hugging the disc.
    Setting,
    /// A bare disc, for overcast skies and for the moon.
    None,
}

/// A pale disc with a rim, plus rays. Filling the disc stops the sky texture
/// from showing through it.
fn sun(
    target: &mut Framebuffer,
    bounds: Rectangle,
    center: Point,
    diameter: u32,
    rays: RayStyle,
    ink: BinaryColor,
) {
    let center = bounds.top_left + center;
    let thin = PrimitiveStyle::with_stroke(ink, 1);
    let radius = diameter as i32 / 2;

    let directions: &[(i32, i32)] = match rays {
        RayStyle::Rising => &[(-10, -3), (-8, -7), (0, -10), (8, -7), (10, -3)],
        RayStyle::High => &[
            (0, -10),
            (7, -7),
            (10, 0),
            (7, 7),
            (0, 10),
            (-7, 7),
            (-10, 0),
            (-7, -7),
        ],
        RayStyle::Setting => &[(-9, -5), (-5, -9), (5, -9), (9, -5)],
        RayStyle::None => &[],
    };
    let (near, far) = match rays {
        RayStyle::Rising => (radius + 4, radius + 12),
        RayStyle::High => (radius + 3, radius + 9),
        RayStyle::Setting | RayStyle::None => (radius + 3, radius + 7),
    };
    for (index, (dx, dy)) in directions.iter().enumerate() {
        // Alternating lengths keep the midday sun from looking mechanical.
        let far = if matches!(rays, RayStyle::High) && index % 2 == 1 {
            far - 3
        } else {
            far
        };
        Line::new(
            center + scaled(*dx, *dy, near),
            center + scaled(*dx, *dy, far),
        )
        .into_styled(thin)
        .draw(target)
        .ok();
    }

    Circle::with_center(center, diameter)
        .into_styled(PrimitiveStyle::with_fill(ink.invert()))
        .draw(target)
        .ok();
    Circle::with_center(center, diameter)
        .into_styled(thin)
        .draw(target)
        .ok();
}

/// A crescent: a white disc minus a disc offset up and to the right, with
/// craters punched into the lit edge and an optional broken halo.
fn moon(target: &mut Framebuffer, bounds: Rectangle, center: Point, diameter: u32, halo: bool) {
    let white_fill = PrimitiveStyle::with_fill(BinaryColor::Off);
    let dark_fill = PrimitiveStyle::with_fill(BinaryColor::On);

    Circle::with_center(bounds.top_left + center, diameter)
        .into_styled(white_fill)
        .draw(target)
        .ok();
    Circle::with_center(bounds.top_left + center + Point::new(9, -6), diameter - 1)
        .into_styled(dark_fill)
        .draw(target)
        .ok();
    for (offset, size) in [(Point::new(-9, 7), 4_u32), (Point::new(-5, -7), 3)] {
        Circle::with_center(bounds.top_left + center + offset, size)
            .into_styled(dark_fill)
            .draw(target)
            .ok();
    }

    if !halo {
        return;
    }
    for (offset, length) in [
        (Point::new(6, 11), 4_i32),
        (Point::new(3, 25), 3),
        (Point::new(9, 42), 4),
        (Point::new(27, 47), 4),
        (Point::new(41, 39), 3),
        (Point::new(43, 14), 3),
    ] {
        let start = bounds.top_left + offset;
        Line::new(start, start + Point::new(length, 0))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 1))
            .draw(target)
            .ok();
    }
}

fn stars(target: &mut Framebuffer, bounds: Rectangle) {
    for (offset, size) in [
        (Point::new(155, 13), 3_i32),
        (Point::new(179, 25), 2),
        (Point::new(164, 41), 3),
        (Point::new(187, 46), 2),
        (Point::new(88, 6), 2),
        (Point::new(122, 5), 3),
        (Point::new(60, 6), 2),
    ] {
        star(target, bounds.top_left + offset, size);
    }
    for offset in [
        Point::new(168, 8),
        Point::new(190, 15),
        Point::new(152, 31),
        Point::new(175, 53),
        Point::new(158, 56),
        Point::new(105, 6),
        Point::new(75, 5),
        Point::new(137, 7),
        Point::new(48, 13),
        Point::new(40, 6),
        Point::new(191, 34),
    ] {
        Pixel(bounds.top_left + offset, BinaryColor::Off)
            .draw(target)
            .ok();
    }
}

/// A four-pointed star; `size` is the arm length in pixels.
fn star(target: &mut Framebuffer, center: Point, size: i32) {
    let white = PrimitiveStyle::with_stroke(BinaryColor::Off, 1);
    Line::new(center - Point::new(size, 0), center + Point::new(size, 0))
        .into_styled(white)
        .draw(target)
        .ok();
    Line::new(center - Point::new(0, size), center + Point::new(0, size))
        .into_styled(white)
        .draw(target)
        .ok();
    if size > 2 {
        for (dx, dy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
            Pixel(center + Point::new(dx, dy), BinaryColor::Off)
                .draw(target)
                .ok();
        }
    }
}

/// Scales a direction roughly ten units long to `length`.
fn scaled(dx: i32, dy: i32, length: i32) -> Point {
    Point::new(dx * length / 10, dy * length / 10)
}

// ---------------------------------------------------------------------------
// Weather
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Eq, PartialEq)]
enum Drop {
    Rain,
    Flake,
}

/// Candidate positions for rain and snow, all outside the text zones. Each one
/// is skipped when it would land on the sun, the moon, or the ground.
const DROPS: [(i32, i32); 16] = [
    (10, 16),
    (20, 28),
    (30, 15),
    (40, 25),
    (14, 41),
    (26, 47),
    (38, 39),
    (18, 57),
    (156, 17),
    (168, 29),
    (180, 15),
    (190, 27),
    (160, 43),
    (174, 47),
    (186, 38),
    (166, 55),
];

fn precipitation(
    target: &mut Framebuffer,
    bounds: Rectangle,
    drop: Drop,
    luminary: Luminary,
    ground: Profile,
    ink: BinaryColor,
) {
    let (center, radius) = luminary;
    let thin = PrimitiveStyle::with_stroke(ink, 1);
    let fill = PrimitiveStyle::with_fill(ink);

    for (index, (x, y)) in DROPS.into_iter().enumerate() {
        let point = bounds.top_left + Point::new(x, y);
        // Keep clear of the disc, and of the ground it would fall through.
        let gap = point - center;
        if gap.x * gap.x + gap.y * gap.y <= (radius + 5) * (radius + 5) {
            continue;
        }
        if y + 8 > horizon(ground, x) {
            continue;
        }

        match drop {
            Drop::Rain => {
                // Two lengths, so the fall does not look like a comb.
                let length = if index % 3 == 0 { 7 } else { 5 };
                Line::new(point, point + Point::new(-2, length))
                    .into_styled(thin)
                    .draw(target)
                    .ok();
            }
            Drop::Flake if index % 3 == 0 => {
                Line::new(point - Point::new(2, 0), point + Point::new(2, 0))
                    .into_styled(thin)
                    .draw(target)
                    .ok();
                Line::new(point - Point::new(0, 2), point + Point::new(0, 2))
                    .into_styled(thin)
                    .draw(target)
                    .ok();
            }
            Drop::Flake => {
                Rectangle::new(point, Size::new_equal(2))
                    .into_styled(fill)
                    .draw(target)
                    .ok();
            }
        }
    }
}

/// A zigzag bolt hanging from a cloud. Kept short enough to stay out of the
/// ground, which at night is a skyline the bolt would otherwise cross.
fn bolt(target: &mut Framebuffer, bounds: Rectangle, from: Point, ink: BinaryColor) {
    let from = bounds.top_left + from;
    let points = [
        from,
        from + Point::new(-4, 6),
        from + Point::new(0, 6),
        from + Point::new(-3, 12),
    ];
    Polyline::new(&points)
        .into_styled(PrimitiveStyle::with_stroke(ink, 2))
        .draw(target)
        .ok();
}

/// Pale dashes along the top of the ground, reading as settled snow.
fn snow_line(target: &mut Framebuffer, bounds: Rectangle, profile: Profile) {
    let style = PrimitiveStyle::with_fill(BinaryColor::Off);
    for x in INTERIOR_LEFT..=INTERIOR_RIGHT {
        Rectangle::new(
            bounds.top_left + Point::new(x, horizon(profile, x) + 1),
            Size::new(1, 2),
        )
        .into_styled(style)
        .draw(target)
        .ok();
    }
}

/// Pale puddles just below the ground line.
fn puddles(target: &mut Framebuffer, bounds: Rectangle, profile: Profile) {
    let style = PrimitiveStyle::with_fill(BinaryColor::Off);
    for (x, width, drop) in [
        (12_i32, 9_u32, 3_i32),
        (30, 6, 5),
        (86, 11, 4),
        (120, 7, 3),
        (166, 10, 4),
        (184, 6, 3),
    ] {
        let y = horizon(profile, x) + drop;
        if y > INTERIOR_BOTTOM {
            continue;
        }
        Rectangle::new(bounds.top_left + Point::new(x, y), Size::new(width, 1))
            .into_styled(style)
            .draw(target)
            .ok();
    }
}

/// Fog: broken drifts in the sky, and pale bands over the ground so the
/// landscape looks like it is fading out rather than being underlined. Solid
/// lines would read as strikethrough, so every drift is split by a gap.
fn fog(target: &mut Framebuffer, bounds: Rectangle, ink: BinaryColor) {
    for (x, width, y, pale) in [
        (8_i32, 13_u32, 34_i32, false),
        (25, 17, 34, false),
        (156, 15, 31, false),
        (176, 16, 31, false),
        (12, 19, 44, false),
        (36, 8, 44, false),
        (152, 10, 42, false),
        (167, 22, 42, false),
        (6, 42, 62, true),
        (150, 44, 60, true),
        (10, 34, 67, true),
        (158, 34, 66, true),
        (18, 24, 71, true),
        (166, 24, 70, true),
    ] {
        let color = if pale { ink.invert() } else { ink };
        Rectangle::new(bounds.top_left + Point::new(x, y), Size::new(width, 1))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(target)
            .ok();
    }
}

// ---------------------------------------------------------------------------
// Ground and scenery
// ---------------------------------------------------------------------------

/// Horizon control points: `(x, y)` pairs interpolated into a silhouette. The
/// profile rises in the left and right margins and stays low behind the date.
type Profile = &'static [(i32, i32)];

const HILLS_MORNING: Profile = &[
    (5, 70),
    (14, 62),
    (26, 57),
    (38, 61),
    (48, 68),
    (70, 71),
    (130, 71),
    (152, 69),
    (166, 61),
    (180, 58),
    (194, 66),
];

const HILLS_DAY: Profile = &[
    (5, 70),
    (16, 65),
    (30, 64),
    (46, 69),
    (80, 72),
    (120, 72),
    (150, 71),
    (170, 68),
    (184, 69),
    (194, 72),
];

const HILLS_EVENING: Profile = &[
    (5, 65),
    (16, 58),
    (28, 61),
    (44, 66),
    (60, 70),
    (110, 72),
    (140, 71),
    (158, 67),
    (176, 64),
    (194, 68),
];

/// The skyline is drawn from its own table; this only tells precipitation how
/// low it may fall before it would land on a tower.
const SKYLINE_CLEARANCE: Profile = &[(5, 52), (194, 52)];

/// Height of the silhouette at `x`, linearly interpolated between control
/// points.
fn horizon(profile: Profile, x: i32) -> i32 {
    if x <= profile[0].0 {
        return profile[0].1;
    }
    for window in profile.windows(2) {
        let (left_x, left_y) = window[0];
        let (right_x, right_y) = window[1];
        if x <= right_x {
            let span = (right_x - left_x).max(1);
            return left_y + (right_y - left_y) * (x - left_x) / span;
        }
    }
    profile[profile.len() - 1].1
}

/// Fills the silhouette from the horizon to one row past the interior, so a disc
/// sitting on the horizon cannot leak out below the card.
fn hills(target: &mut Framebuffer, bounds: Rectangle, profile: Profile) {
    let style = PrimitiveStyle::with_fill(BinaryColor::On);
    for x in INTERIOR_LEFT..=INTERIOR_RIGHT {
        let top = horizon(profile, x);
        Rectangle::new(
            bounds.top_left + Point::new(x, top),
            Size::new(1, (INTERIOR_BOTTOM + 1 - top).max(1) as u32),
        )
        .into_styled(style)
        .draw(target)
        .ok();
    }
}

/// A skyline of dark towers outlined in white, standing on a lit horizon.
fn skyline(target: &mut Framebuffer, bounds: Rectangle) {
    let white = PrimitiveStyle::with_stroke(BinaryColor::Off, 1);
    let dark = PrimitiveStyle::with_fill(BinaryColor::On);
    let base = INTERIOR_BOTTOM;

    Line::new(
        bounds.top_left + Point::new(INTERIOR_LEFT, base),
        bounds.top_left + Point::new(INTERIOR_RIGHT, base),
    )
    .into_styled(white)
    .draw(target)
    .ok();

    // (x, width, height) towers. They stand in the margins the text leaves
    // free; behind the date only the lit horizon runs through.
    for (x, width, height) in [
        (7_i32, 11_u32, 15_i32),
        (19, 8, 21),
        (28, 13, 10),
        (42, 8, 13),
        (150, 9, 12),
        (161, 12, 19),
        (174, 8, 11),
        (183, 11, 16),
    ] {
        let top = base - height;
        let tower = Rectangle::new(
            bounds.top_left + Point::new(x, top),
            Size::new(width, height as u32),
        );
        tower.into_styled(dark).draw(target).ok();
        tower.into_styled(white).draw(target).ok();
        // Lit windows in pairs, inset from the outline.
        let mut window_y = top + 3;
        while window_y < base - 2 {
            for window_x in [x + 3, x + width as i32 - 4] {
                if window_x > x + 1 && window_x < x + width as i32 - 1 {
                    Pixel(
                        bounds.top_left + Point::new(window_x, window_y),
                        BinaryColor::Off,
                    )
                    .draw(target)
                    .ok();
                }
            }
            window_y += 4;
        }
    }
}

/// A gull-wing chevron, cleared out of the sky texture so it stays readable.
fn bird(target: &mut Framebuffer, bounds: Rectangle, center: Point, span: i32, ink: BinaryColor) {
    let thin = PrimitiveStyle::with_stroke(ink, 1);
    let center = bounds.top_left + center;
    Rectangle::new(
        center - Point::new(span + 1, span / 2 + 2),
        Size::new(span as u32 * 2 + 3, span as u32 / 2 + 4),
    )
    .into_styled(PrimitiveStyle::with_fill(ink.invert()))
    .draw(target)
    .ok();
    Line::new(center - Point::new(span, span / 2), center)
        .into_styled(thin)
        .draw(target)
        .ok();
    Line::new(center, center + Point::new(span, -span / 2))
        .into_styled(thin)
        .draw(target)
        .ok();
}

/// A conifer: two stacked skirts over a short trunk.
fn conifer(target: &mut Framebuffer, bounds: Rectangle, base: Point, height: i32) {
    let dark = PrimitiveStyle::with_fill(BinaryColor::On);
    let base = bounds.top_left + base;
    let half = (height / 3).max(2);

    for (lift, half) in [(0, half), (height / 3, half * 3 / 4)] {
        Triangle::new(
            Point::new(base.x - half, base.y - lift),
            Point::new(base.x + half, base.y - lift),
            Point::new(base.x, base.y - height),
        )
        .into_styled(dark)
        .draw(target)
        .ok();
    }
}

/// A cloud silhouette with a flat base: lobes are drawn in ink, hollowed out one
/// pixel inside themselves so no internal arcs show, and then trimmed to a
/// straight bottom edge.
fn cloud(target: &mut Framebuffer, bounds: Rectangle, center: Point, scale: i32, ink: BinaryColor) {
    let center = bounds.top_left + center;
    // Odd diameters keep the lobes centred on whole pixels.
    let side = (scale * 3 / 2) | 1;
    let middle = (scale * 2) | 1;
    let lobes = [
        (Point::new(-scale, 0), side),
        (Point::new(0, -scale / 2), middle),
        (Point::new(scale, 0), side),
    ];
    let half_width = scale + side / 2;
    let base_y = side / 2;

    for (color, inset) in [(ink, 0), (ink.invert(), 1)] {
        let style = PrimitiveStyle::with_fill(color);
        for (offset, diameter) in lobes {
            Circle::with_center(center + offset, (diameter - inset * 2).max(1) as u32)
                .into_styled(style)
                .draw(target)
                .ok();
        }
        Rectangle::new(
            center + Point::new(-half_width + inset, -scale / 2),
            Size::new(
                (half_width * 2 - inset * 2).max(1) as u32,
                (base_y + scale / 2 - inset).max(1) as u32,
            ),
        )
        .into_styled(style)
        .draw(target)
        .ok();
    }

    // Trim the lobe bulges below the base, then close the silhouette.
    Rectangle::new(
        center + Point::new(-half_width, base_y + 1),
        Size::new(half_width as u32 * 2, side as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(ink.invert()))
    .draw(target)
    .ok();
    Line::new(
        center + Point::new(-half_width, base_y),
        center + Point::new(half_width, base_y),
    )
    .into_styled(PrimitiveStyle::with_stroke(ink, 1))
    .draw(target)
    .ok();
}

/// Ordered-dither sky texture, densest at the top of the card and settling by
/// `fade_to`. Density also falls off towards the text so the wash meets the
/// clock with a soft edge instead of a hard rectangle.
fn wash(
    target: &mut Framebuffer,
    bounds: Rectangle,
    top_level: u8,
    settled_level: u8,
    fade_to: i32,
    ink: BinaryColor,
) {
    // 4 x 4 Bayer matrix: a pixel lights when its threshold is below the level.
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    // A long ramp: a short one leaves a dense sky looking like a rectangle has
    // been cut out around the clock.
    const FALLOFF: i32 = 16;

    if top_level == 0 && settled_level == 0 {
        return;
    }
    let span = (fade_to - INTERIOR_TOP).max(1);
    for y in INTERIOR_TOP..=INTERIOR_BOTTOM {
        let progress = (y - INTERIOR_TOP).min(span);
        let level = i32::from(top_level)
            - (i32::from(top_level) - i32::from(settled_level)) * progress / span;
        if level <= 0 {
            continue;
        }
        let row = &BAYER[(y as usize) % 4];
        let vertical_gap = (TEXT_TOP - y).max(y - TEXT_BOTTOM).max(0);
        target
            .draw_iter((INTERIOR_LEFT..=INTERIOR_RIGHT).filter_map(|x| {
                let gap = (TEXT_LEFT - x).max(x - TEXT_RIGHT).max(vertical_gap);
                let level = level * gap.min(FALLOFF) / FALLOFF;
                (i32::from(row[(x as usize) % 4]) < level)
                    .then(|| Pixel(bounds.top_left + Point::new(x, y), ink))
            }))
            .ok();
    }
}
