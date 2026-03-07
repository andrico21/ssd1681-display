//! `ssd1681-display` - async driver for SSD1681-based e-ink displays.
//!
//! AI-aided Rust port and refactoring of the
//! [M5GFX](https://github.com/m5stack/M5GFX) C++ library by M5Stack/lovyan03,
//! specifically the `Panel_GDEW0154D67` panel driver
//! (`lgfx/v1/panel/Panel_GDEW0154D67.cpp`).
//!
//! `no_std`, zero `unsafe`, async-first via `embassy-time`.
//!
//! # Architecture
//!
//! Three layers, each usable independently:
//!
//! | Layer | Type | Purpose |
//! |-------|------|---------|
//! | [`Display`] | High-level facade | Owns driver + framebuffer, tracks power state |
//! | [`Ssd1681`] | Hardware driver | Direct SPI register access, any refresh mode |
//! | [`FrameBuf`] | Framebuffer | 200x200 1-bit buffer, `embedded-graphics` `DrawTarget` |
//!
//! # Pin wiring
//!
//! All pin types are generic over `embedded-hal` traits - no GPIO numbers
//! are hardcoded. Pass any `OutputPin`/`InputPin`/`SpiDevice` implementation:
//!
//! ```ignore
//! let spi_dev = ExclusiveDevice::new(spi_bus, cs_pin, NoDelay).unwrap();
//! let mut display = Display::new(spi_dev, dc_pin, rst_pin, busy_pin);
//! display.init().await;
//! ```
//!
//! # Refresh modes
//!
//! The SSD1681 supports many refresh strategies via the 0x22 register.
//! Use the convenience methods for common cases, or pass any [`RefreshMode`]
//! (or raw `u8`) to [`Display::refresh`] / [`Display::refresh_raw`]:
//!
//! ```ignore
//! // Convenience (recommended):
//! display.full_refresh();              // ~2089 ms, perfect quality
//! display.fast_refresh();              // ~309 ms, differential, auto power-on
//!
//! // Explicit mode:
//! display.refresh(RefreshMode::PoweredFast);  // ~400 ms, self-contained
//! display.refresh(RefreshMode::Mode2CF);      // ~2086 ms, differential with power-down
//!
//! // Raw 0x22 parameter (for experimentation):
//! display.refresh_raw(0xDC);
//! ```
//!
//! See [`RefreshMode`] for the full list with measured timings.

#![no_std]

pub mod framebuf;
pub mod ssd1681;

pub use framebuf::FrameBuf;
pub use ssd1681::{BUF_SIZE, HEIGHT, RefreshMode, Ssd1681, WIDTH};

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;

/// High-level display controller.
///
/// Generic over SPI and GPIO types - works with any board that wires
/// an SSD1681-based panel to an `embedded-hal`-compatible MCU.
pub struct Display<SPI, DC, RST, BUSY> {
    driver: Ssd1681<SPI, DC, RST, BUSY>,
    fb: FrameBuf,
    powered: bool,
}

impl<SPI, DC, RST, BUSY, E> Display<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    /// Create a new display. Call [`init`] before drawing.
    pub fn new(spi: SPI, dc: DC, rst: RST, busy: BUSY) -> Self {
        Self {
            driver: Ssd1681::new(spi, dc, rst, busy),
            fb: FrameBuf::new(),
            powered: false,
        }
    }

    /// Initialize the SSD1681 and prime both RAM buffers white.
    pub async fn init(&mut self) {
        self.driver.init().await;
        self.fb.clear_white();
        self.driver.write_ram_both(self.fb.as_bytes());
        self.driver.set_refresh_mode(RefreshMode::FullF7);
        self.driver.trigger_refresh();
        self.powered = false;
    }

    /// Mutable reference to the framebuffer for drawing with `embedded-graphics`.
    ///
    /// The returned `FrameBuf` implements `DrawTarget<Color = BinaryColor>`,
    /// so you can use any `embedded-graphics` primitive, font, or image:
    ///
    /// ```ignore
    /// use embedded_graphics::{prelude::*, text::Text, mono_font::*};
    /// let style = MonoTextStyle::new(&ascii::FONT_10X20, BinaryColor::Off);
    /// Text::new("Hello", Point::new(10, 30), style)
    ///     .draw(display.draw_target())?;
    /// display.full_refresh();
    /// ```
    pub fn draw_target(&mut self) -> &mut FrameBuf {
        &mut self.fb
    }

    // -- Convenience refresh methods ----------------------------------------

    /// Full-quality refresh (~2089 ms). Reloads LUT, flickers, perfect output.
    /// Powers down afterwards. Writes both RAM banks for clean transitions.
    pub fn full_refresh(&mut self) {
        self.full_refresh_clean();
    }

    /// Full-quality refresh that also overwrites the OLD RAM buffer.
    ///
    /// Use this after switching screen content (e.g. splash → UI) to prevent
    /// ghosting from the previous image. Writes the framebuffer to *both*
    /// SSD1681 RAM banks before triggering the refresh.
    pub fn full_refresh_clean(&mut self) {
        self.driver.set_refresh_param_raw(RefreshMode::FullF7 as u8);
        self.driver.write_ram_both(self.fb.as_bytes());
        self.driver.trigger_refresh();
        self.powered = false;
    }

    /// Fast differential refresh (~309 ms). No flicker, partial-update quality.
    /// Automatically powers on clock/analog if needed.
    /// Writes both RAM banks so OLD RAM stays current for the next diff.
    pub fn fast_refresh(&mut self) {
        self.ensure_powered();
        self.driver.set_refresh_param_raw(RefreshMode::BareFast as u8);
        self.driver.write_ram_both(self.fb.as_bytes());
        self.driver.trigger_refresh();
        self.powered = true;
    }

    /// Self-contained differential refresh (~2086 ms, minimal flicker).
    /// Uses LUT2 + display mode 2 with power-down — better contrast than
    /// the fast modes while avoiding the heavy full-screen flash.
    /// Writes both RAM banks so OLD RAM stays current for the next diff.
    pub fn fast_refresh_self_contained(&mut self) {
        self.driver.set_refresh_param_raw(RefreshMode::Mode2CF as u8);
        self.driver.write_ram_both(self.fb.as_bytes());
        self.driver.trigger_refresh();
        self.powered = (RefreshMode::Mode2CF as u8 & 0x03) == 0;
    }

    // -- Generic refresh ----------------------------------------------------

    /// Refresh the display using any [`RefreshMode`].
    ///
    /// Writes the current framebuffer to BW RAM, then triggers the refresh
    /// with the given mode. Power state is updated based on the mode:
    /// modes ending in power-down (bit 0+1 set) clear `powered`; modes
    /// that leave clock/analog on keep it set.
    pub fn refresh(&mut self, mode: RefreshMode) {
        self.refresh_raw(mode as u8);
    }

    /// Refresh with a raw 0x22 parameter value.
    ///
    /// Use this for experimentation or modes not covered by [`RefreshMode`].
    /// The parameter bits are:
    ///
    /// | Bit | Function |
    /// |-----|----------|
    /// | 7   | Enable clock |
    /// | 6   | Enable analog |
    /// | 5   | Load temperature |
    /// | 4   | Load LUT (Mode 1) |
    /// | 3   | Load LUT (Mode 2) - combined with bit 4 |
    /// | 2   | Display (Mode 1) |
    /// | 1   | Disable analog |
    /// | 0   | Disable clock |
    pub fn refresh_raw(&mut self, param: u8) {
        self.driver.set_refresh_param_raw(param);
        self.driver.write_ram(self.fb.as_bytes());
        self.driver.trigger_refresh();
        // Track power state: bits 0+1 = disable clock+analog
        self.powered = (param & 0x03) == 0;
    }

    // -- Power management ---------------------------------------------------

    /// Enter deep sleep. Requires [`init`] to wake again.
    pub fn sleep(&mut self) {
        self.driver.sleep();
        self.powered = false;
    }

    /// Returns `true` if clock/analog are currently powered on.
    pub fn is_powered(&self) -> bool {
        self.powered
    }

    /// Ensure clock/analog are on (for bare modes that need it).
    fn ensure_powered(&mut self) {
        if !self.powered {
            self.driver.set_refresh_mode(RefreshMode::PowerOn);
            self.driver.trigger_refresh();
            self.powered = true;
        }
    }

    // -- Low-level access ---------------------------------------------------

    /// Direct access to the SSD1681 driver for advanced usage.
    ///
    /// Use when you need register-level control (custom LUT loading,
    /// partial window updates, dual-RAM writes for Mode 2, etc.).
    /// After manual driver operations, call [`set_powered`] to keep
    /// the facade's power tracking accurate.
    pub fn driver(&mut self) -> &mut Ssd1681<SPI, DC, RST, BUSY> {
        &mut self.driver
    }

    /// Manually override the power state flag.
    ///
    /// Call after direct [`driver`] operations that change the power state,
    /// so the facade's auto-power-on logic stays correct.
    pub fn set_powered(&mut self, powered: bool) {
        self.powered = powered;
    }
}
