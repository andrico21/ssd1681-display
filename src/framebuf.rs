//! 1-bit framebuffer with `embedded-graphics` `DrawTarget` support.
//!
//! 200x200 px, 25 bytes per row, MSB-first bit order.
//! `BinaryColor::On` = white (bit set), `BinaryColor::Off` = black (bit clear).

use embedded_graphics_core::pixelcolor::BinaryColor;
use embedded_graphics_core::prelude::*;

use crate::ssd1681::BUF_SIZE;

/// 1-bit framebuffer for the 200x200 e-ink display.
pub struct FrameBuf {
    buf: [u8; BUF_SIZE],
}

impl FrameBuf {
    /// Create a new framebuffer filled with white (0xFF).
    pub fn new() -> Self {
        Self { buf: [0xFF; BUF_SIZE] }
    }

    /// Raw byte slice for passing to the SSD1681 driver.
    pub fn as_bytes(&self) -> &[u8; BUF_SIZE] {
        &self.buf
    }

    /// Fill the entire buffer with white.
    pub fn clear_white(&mut self) {
        self.buf.fill(0xFF);
    }

    /// Fill the entire buffer with black.
    pub fn clear_black(&mut self) {
        self.buf.fill(0x00);
    }
}

impl OriginDimensions for FrameBuf {
    fn size(&self) -> Size {
        Size::new(200, 200)
    }
}

impl DrawTarget for FrameBuf {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x >= 0 && point.x < 200 && point.y >= 0 && point.y < 200 {
                let idx = (point.y as usize) * 25 + (point.x as usize) / 8;
                let bit = 7 - ((point.x as usize) % 8);
                match color {
                    BinaryColor::Off => self.buf[idx] &= !(1 << bit),
                    BinaryColor::On => self.buf[idx] |= 1 << bit,
                }
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: BinaryColor) -> Result<(), Self::Error> {
        match color {
            BinaryColor::On => self.buf.fill(0xFF),
            BinaryColor::Off => self.buf.fill(0x00),
        }
        Ok(())
    }
}
