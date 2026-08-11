//! A deliberately small layout layer for monochrome embedded displays.
//!
//! Widgets receive a rectangle and draw relative to it. `Stack` is the only
//! container for now: it places fixed-size and weighted-fill children along
//! either axis. That is enough to keep dashboard composition readable without
//! committing the firmware to a large UI framework.

use std::convert::Infallible;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::Pixel;
use embedded_graphics::primitives::Rectangle;

/// A row-major, one-bit-per-pixel drawing surface.
///
/// The row stride is derived from the requested size, so the UI is not tied to
/// the current 200 x 200 panel. Panel drivers remain responsible for validating
/// that a framebuffer matches their hardware.
pub struct Framebuffer {
    bytes: Box<[u8]>,
    size: Size,
    row_bytes: usize,
}

impl Framebuffer {
    pub fn new(size: Size) -> Self {
        let row_bytes = size.width.div_ceil(8) as usize;
        Self {
            bytes: vec![0xff; row_bytes * size.height as usize].into_boxed_slice(),
            size,
            row_bytes,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bounds(&self) -> Rectangle {
        Rectangle::new(Point::zero(), self.size)
    }

    pub fn clear(&mut self, color: BinaryColor) {
        self.bytes.fill(match color {
            BinaryColor::On => 0x00,
            BinaryColor::Off => 0xff,
        });
    }

    /// Returns byte-aligned rectangles containing the pixels that changed.
    /// Adjacent rows with the same changed-byte run are coalesced. Pathological
    /// patterns fall back to one bounding box to keep panel command overhead
    /// bounded.
    pub fn changed_regions(&self, next: &Self) -> Vec<Rectangle> {
        const MAX_REGIONS: usize = 24;

        if self.size != next.size {
            return vec![next.bounds()];
        }

        let mut regions: Vec<Rectangle> = Vec::new();
        for y in 0..self.size.height as usize {
            let row = y * self.row_bytes;
            let mut x_byte = 0_usize;
            while x_byte < self.row_bytes {
                while x_byte < self.row_bytes
                    && self.bytes[row + x_byte] == next.bytes[row + x_byte]
                {
                    x_byte += 1;
                }
                if x_byte == self.row_bytes {
                    break;
                }
                let start = x_byte;
                while x_byte < self.row_bytes
                    && self.bytes[row + x_byte] != next.bytes[row + x_byte]
                {
                    x_byte += 1;
                }
                let left = (start * 8) as i32;
                let right = (x_byte * 8).min(self.size.width as usize) as u32;
                let width = right - left as u32;
                if let Some(region) = regions.iter_mut().rev().find(|region| {
                    region.top_left.x == left
                        && region.size.width == width
                        && region.top_left.y + region.size.height as i32 == y as i32
                }) {
                    region.size.height += 1;
                } else {
                    regions.push(Rectangle::new(
                        Point::new(left, y as i32),
                        Size::new(width, 1),
                    ));
                    if regions.len() > MAX_REGIONS {
                        return self.changed_bounds(next).into_iter().collect();
                    }
                }
            }
        }
        regions
    }

    fn changed_bounds(&self, next: &Self) -> Option<Rectangle> {
        let mut left_byte = self.row_bytes;
        let mut right_byte = 0_usize;
        let mut top = self.size.height as usize;
        let mut bottom = 0_usize;
        let mut changed = false;

        for y in 0..self.size.height as usize {
            let row = y * self.row_bytes;
            for x_byte in 0..self.row_bytes {
                if self.bytes[row + x_byte] != next.bytes[row + x_byte] {
                    changed = true;
                    left_byte = left_byte.min(x_byte);
                    right_byte = right_byte.max(x_byte);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
        }

        changed.then(|| {
            let left = (left_byte * 8) as i32;
            let right = ((right_byte + 1) * 8).min(self.size.width as usize) as u32;
            Rectangle::new(
                Point::new(left, top as i32),
                Size::new(right - left as u32, (bottom - top + 1) as u32),
            )
        })
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        self.size
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
            if (0..self.size.width as i32).contains(&point.x)
                && (0..self.size.height as i32).contains(&point.y)
            {
                let index = point.y as usize * self.row_bytes + (point.x as usize >> 3);
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

/// A renderable UI unit whose placement is controlled by its parent.
pub trait Widget {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Length {
    Fixed(u32),
    Fill(u16),
}

#[derive(Clone, Copy)]
pub struct StackItem<'a> {
    widget: &'a dyn Widget,
    length: Length,
}

impl<'a> StackItem<'a> {
    pub const fn fixed(widget: &'a dyn Widget, pixels: u32) -> Self {
        Self {
            widget,
            length: Length::Fixed(pixels),
        }
    }

    pub const fn fill(widget: &'a dyn Widget, weight: u16) -> Self {
        Self {
            widget,
            length: Length::Fill(weight),
        }
    }
}

/// Places children sequentially along one axis.
pub struct Stack<'a> {
    axis: Axis,
    spacing: u32,
    children: &'a [StackItem<'a>],
}

impl<'a> Stack<'a> {
    pub const fn horizontal(children: &'a [StackItem<'a>]) -> Self {
        Self {
            axis: Axis::Horizontal,
            spacing: 0,
            children,
        }
    }

    pub const fn vertical(children: &'a [StackItem<'a>]) -> Self {
        Self {
            axis: Axis::Vertical,
            spacing: 0,
            children,
        }
    }

    pub const fn with_spacing(mut self, pixels: u32) -> Self {
        self.spacing = pixels;
        self
    }
}

impl Widget for Stack<'_> {
    fn draw(&self, target: &mut Framebuffer, bounds: Rectangle) {
        if self.children.is_empty() {
            return;
        }

        let available = match self.axis {
            Axis::Horizontal => bounds.size.width,
            Axis::Vertical => bounds.size.height,
        };
        let gaps = self
            .spacing
            .saturating_mul(self.children.len().saturating_sub(1) as u32);
        let fixed = self.children.iter().fold(0_u32, |total, child| {
            total.saturating_add(match child.length {
                Length::Fixed(pixels) => pixels,
                Length::Fill(_) => 0,
            })
        });
        let fill_space = available.saturating_sub(gaps).saturating_sub(fixed);
        let total_weight = self.children.iter().fold(0_u32, |total, child| {
            total.saturating_add(match child.length {
                Length::Fill(weight) => u32::from(weight),
                Length::Fixed(_) => 0,
            })
        });

        let mut cursor = 0_u32;
        let mut distributed_fill = 0_u32;
        let last_fill = self
            .children
            .iter()
            .rposition(|child| matches!(child.length, Length::Fill(weight) if weight > 0));

        for (index, child) in self.children.iter().enumerate() {
            let requested = match child.length {
                Length::Fixed(pixels) => pixels,
                Length::Fill(_) if Some(index) == last_fill => {
                    fill_space.saturating_sub(distributed_fill)
                }
                Length::Fill(weight) if total_weight > 0 => {
                    let pixels = (u64::from(fill_space) * u64::from(weight)
                        / u64::from(total_weight)) as u32;
                    distributed_fill += pixels;
                    pixels
                }
                Length::Fill(_) => 0,
            };
            let length = requested.min(available.saturating_sub(cursor));
            let (origin, size) = match self.axis {
                Axis::Horizontal => (
                    bounds.top_left + Point::new(cursor as i32, 0),
                    Size::new(length, bounds.size.height),
                ),
                Axis::Vertical => (
                    bounds.top_left + Point::new(0, cursor as i32),
                    Size::new(bounds.size.width, length),
                ),
            };
            child.widget.draw(target, Rectangle::new(origin, size));
            cursor = cursor.saturating_add(length).saturating_add(self.spacing);
        }
    }
}
