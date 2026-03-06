//! SSD1681 e-ink controller driver for the GDEY0154D67 1.54" 200x200 display.
//!
//! Drives the SSD1681 via SPI using embedded-hal traits. Provides full control
//! over refresh mode parameters (0x22 command) for different speed/quality
//! trade-offs. Init sequence matches the M5GFX Panel_GDEW0154D67 reference.

use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use embassy_time::{Duration, Instant, Timer};

// -- SSD1681 command constants ----------------------------------------------

const CMD_GATE_DRIVER_OUTPUT: u8 = 0x01;
const CMD_BOOSTER_SOFT_START: u8 = 0x0C;
const CMD_DEEP_SLEEP: u8 = 0x10;
const CMD_DATA_ENTRY_MODE: u8 = 0x11;
const CMD_SW_RESET: u8 = 0x12;
const CMD_TEMP_SENSOR: u8 = 0x18;
const CMD_MASTER_ACTIVATION: u8 = 0x20;
const CMD_DISPLAY_UPDATE_CTRL1: u8 = 0x21;
const CMD_DISPLAY_UPDATE_CTRL2: u8 = 0x22;
const CMD_WRITE_RAM_BW: u8 = 0x24;
const CMD_WRITE_RAM_RED: u8 = 0x26;
const CMD_BORDER_WAVEFORM: u8 = 0x3C;
const CMD_SET_X_RANGE: u8 = 0x44;
const CMD_SET_Y_RANGE: u8 = 0x45;
const CMD_SET_X_COUNTER: u8 = 0x4E;
const CMD_SET_Y_COUNTER: u8 = 0x4F;

// -- Public constants -------------------------------------------------------

/// Display width in pixels.
pub const WIDTH: u16 = 200;
/// Display height in pixels.
pub const HEIGHT: u16 = 200;
/// Framebuffer size in bytes: (WIDTH/8) x HEIGHT.
pub const BUF_SIZE: usize = (WIDTH as usize / 8) * HEIGHT as usize; // 5000

// -- Refresh modes ----------------------------------------------------------

/// Display Update Control 2 (0x22) parameter presets.
///
/// These control which sub-steps the SSD1681 executes during a refresh:
/// clock enable, analog enable, temperature read, LUT load, display drive,
/// and power-down. Different combinations give different speed/quality
/// trade-offs and power-state transitions.
///
/// ## Measured timings (GDEY0154D67 @ 40 MHz SPI, 25 C)
///
/// | Mode | Value | Time | Quality | Flicker | Power after |
/// |------|-------|------|---------|---------|-------------|
/// | [`FullF7`](Self::FullF7) | 0xF7 | ~2089 ms | Perfect | Yes | Off |
/// | [`ReuseC7`](Self::ReuseC7) | 0xC7 | ~2086 ms | Good | Yes | Off |
/// | [`QualityD7`](Self::QualityD7) | 0xD7 | ~2087 ms | Perfect | Yes | Off |
/// | [`Mode2CF`](Self::Mode2CF) | 0xCF | ~2086 ms | Good | Minimal | Off |
/// | [`PoweredQuality`](Self::PoweredQuality) | 0xD4 | ~1945 ms | Perfect | Yes | **On** |
/// | [`BareQuality`](Self::BareQuality) | 0x14 | ~1854 ms | Perfect | Yes | On (required) |
/// | [`PoweredFast`](Self::PoweredFast) | 0xDC | ~400 ms | Partial | No | **On** |
/// | [`FullFastFC`](Self::FullFastFC) | 0xFC | ~400 ms | Partial | No | **On** |
/// | [`BareFast`](Self::BareFast) | 0x1C | ~309 ms | Partial | No | On (required) |
/// | [`Everything`](Self::Everything) | 0xFF | ~545 ms | Ghosting | Yes | Off |
///
/// "On (required)" means the mode needs clock/analog already enabled via
/// [`PowerOn`](Self::PowerOn) or a prior powered mode.
///
/// ## 0x22 register bit layout
///
/// | Bit | Function |
/// |-----|----------|
/// | 7   | Enable clock |
/// | 6   | Enable analog |
/// | 5   | Load temperature value |
/// | 4   | Load LUT - Display Mode 1 |
/// | 3   | Load LUT - Display Mode 2 (combined with bit 4) |
/// | 2   | Display with Mode 1 |
/// | 1   | Disable analog |
/// | 0   | Disable clock |
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum RefreshMode {
    /// 0xF8 - Startup: clock+analog+temp+LUT1, no display, no power-down.
    Startup = 0xF8,

    /// 0xF7 - Full quality (~2089 ms): clock+analog+temp+LUT1+display1+power-down.
    FullF7 = 0xF7,

    /// 0xC7 - Reuse LUT (~2086 ms): clock+analog+display1+power-down.
    ReuseC7 = 0xC7,

    /// 0xD7 - LUT reload (~2087 ms): clock+analog+LUT1+display1+power-down.
    QualityD7 = 0xD7,

    /// 0xD4 - Powered quality (~1945 ms): clock+analog+LUT1+display1, no power-down.
    PoweredQuality = 0xD4,

    /// 0xDC - Powered fast (~400 ms): clock+analog+LUT2+display2, no power-down.
    PoweredFast = 0xDC,

    /// 0xFC - Full fast with temp (~400 ms): clock+analog+temp+LUT2+display2, no power-down.
    FullFastFC = 0xFC,

    /// 0xFF - Everything (~545 ms, may ghost): all bits set.
    Everything = 0xFF,

    /// 0xCF - Mode 2 with power-down (~2086 ms): clock+analog+LUT2+display2+power-down.
    Mode2CF = 0xCF,

    /// 0x14 - Bare quality (~1854 ms): LUT1+display1 only. Requires clock/analog already on.
    BareQuality = 0x14,

    /// 0x1C - Bare fast (~309 ms): LUT2+display2 only. Requires clock/analog already on.
    BareFast = 0x1C,

    /// 0xC0 - Power on only: enable clock+analog, no LUT, no display, no power-down.
    PowerOn = 0xC0,
}

// -- Driver -----------------------------------------------------------------

/// SSD1681 driver with direct SPI register access.
pub struct Ssd1681<SPI, DC, RST, BUSY> {
    spi: SPI,
    dc: DC,
    rst: RST,
    busy: BUSY,
    refresh_param: u8,
}

impl<SPI, DC, RST, BUSY, E> Ssd1681<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    /// Create a new driver. Does NOT initialize the display - call [`init`] next.
    pub fn new(spi: SPI, dc: DC, rst: RST, busy: BUSY) -> Self {
        Self {
            spi,
            dc,
            rst,
            busy,
            refresh_param: RefreshMode::FullF7 as u8,
        }
    }

    /// Hardware reset: RST low 10 ms - high, wait 10 ms.
    pub async fn hw_reset(&mut self) {
        let _ = self.rst.set_low();
        Timer::after(Duration::from_millis(10)).await;
        let _ = self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
    }

    /// Full init sequence matching M5GFX Panel_GDEW0154D67.
    pub async fn init(&mut self) {
        self.hw_reset().await;
        self.wait_busy();

        // SW Reset
        self.cmd(CMD_SW_RESET);
        Timer::after(Duration::from_millis(10)).await;
        self.wait_busy();

        // Gate driver output: 199 lines, scan direction
        self.cmd_data(CMD_GATE_DRIVER_OUTPUT, &[199, 0, 0]);

        // Data entry mode: X+, Y+ (0x03)
        self.cmd_data(CMD_DATA_ENTRY_MODE, &[0x03]);

        // Border waveform
        self.cmd_data(CMD_BORDER_WAVEFORM, &[0x05]);

        // Built-in temperature sensor
        self.cmd_data(CMD_TEMP_SENSOR, &[0x80]);

        // Booster soft start
        self.cmd_data(CMD_BOOSTER_SOFT_START, &[0x8B, 0x9C, 0x96, 0x0F]);

        // Display update control 1: no inversion
        self.cmd_data(CMD_DISPLAY_UPDATE_CTRL1, &[0x00]);

        // Startup power-up
        self.cmd_data(CMD_DISPLAY_UPDATE_CTRL2, &[RefreshMode::Startup as u8]);
        self.wait_busy();
    }

    // -- RAM writes ---------------------------------------------------------

    /// Write full framebuffer to BW RAM. Returns SPI transfer time in ms.
    pub fn write_ram(&mut self, buf: &[u8; BUF_SIZE]) -> u64 {
        self.wait_busy();
        self.set_window_full();
        self.wait_busy();
        let t0 = Instant::now();
        self.cmd(CMD_WRITE_RAM_BW);
        self.data(buf);
        t0.elapsed().as_millis()
    }

    /// Write full framebuffer to both BW and RED RAM.
    ///
    /// Needed before Mode 2 (differential) refreshes so both buffers
    /// contain the current image.
    pub fn write_ram_both(&mut self, buf: &[u8; BUF_SIZE]) {
        self.wait_busy();
        self.set_window_full();
        self.wait_busy();
        self.cmd(CMD_WRITE_RAM_BW);
        self.data(buf);

        self.set_window_full();
        self.cmd(CMD_WRITE_RAM_RED);
        self.data(buf);
    }

    // -- Refresh control ----------------------------------------------------

    /// Set the refresh mode for subsequent [`trigger_refresh`] calls.
    pub fn set_refresh_mode(&mut self, mode: RefreshMode) {
        self.refresh_param = mode as u8;
    }

    /// Set a raw 0x22 parameter value.
    pub fn set_refresh_param_raw(&mut self, param: u8) {
        self.refresh_param = param;
    }

    /// Send 0x22 + 0x20 and wait for completion (5 s timeout).
    /// Returns busy-wait time in ms.
    pub fn trigger_refresh(&mut self) -> u64 {
        self.wait_busy_timeout(5000);
        self.cmd_data(CMD_DISPLAY_UPDATE_CTRL2, &[self.refresh_param]);
        self.cmd(CMD_MASTER_ACTIVATION);
        let (_, ms) = self.wait_busy_timeout(5000);
        ms
    }

    /// Send 0x22 + 0x20 without waiting for completion.
    pub fn trigger_refresh_no_wait(&mut self) {
        self.wait_busy_timeout(5000);
        self.cmd_data(CMD_DISPLAY_UPDATE_CTRL2, &[self.refresh_param]);
        self.cmd(CMD_MASTER_ACTIVATION);
    }

    /// Write RAM + trigger refresh + wait. Returns (spi_ms, trigger_ms, busy_ms).
    pub fn update_and_wait(&mut self, buf: &[u8; BUF_SIZE]) -> (u64, u64, u64) {
        let spi_ms = self.write_ram(buf);

        let t0 = Instant::now();
        self.trigger_refresh_no_wait();
        let trigger_ms = t0.elapsed().as_millis();

        let (_, busy_ms) = self.wait_busy_timeout(5000);

        (spi_ms, trigger_ms, busy_ms)
    }

    /// Enter deep sleep. Requires [`hw_reset`] to wake.
    pub fn sleep(&mut self) {
        self.wait_busy();
        self.cmd_data(CMD_DISPLAY_UPDATE_CTRL2, &[0x03]);
        self.cmd_data(CMD_DEEP_SLEEP, &[0x03]);
    }

    // -- Busy polling -------------------------------------------------------

    /// Spin-wait until BUSY goes LOW. Returns wait time in ms.
    pub fn wait_busy(&mut self) -> u64 {
        let start = Instant::now();
        while self.busy.is_high().unwrap_or(true) {}
        start.elapsed().as_millis()
    }

    /// Spin-wait with timeout. Returns (was_busy_at_start, elapsed_ms).
    pub fn wait_busy_timeout(&mut self, timeout_ms: u64) -> (bool, u64) {
        let start = Instant::now();
        let was_busy = self.busy.is_high().unwrap_or(false);
        while self.busy.is_high().unwrap_or(true) {
            if start.elapsed().as_millis() > timeout_ms {
                return (was_busy, start.elapsed().as_millis());
            }
        }
        (was_busy, start.elapsed().as_millis())
    }

    // -- Low-level SPI helpers ----------------------------------------------

    fn cmd(&mut self, command: u8) {
        let _ = self.dc.set_low();
        let _ = self.spi.write(&[command]);
    }

    fn data(&mut self, data: &[u8]) {
        let _ = self.dc.set_high();
        let _ = self.spi.write(data);
    }

    fn cmd_data(&mut self, command: u8, data: &[u8]) {
        self.cmd(command);
        self.data(data);
    }

    fn set_window_full(&mut self) {
        self.cmd_data(CMD_SET_X_RANGE, &[0, (WIDTH / 8 - 1) as u8]);
        self.cmd_data(CMD_SET_X_COUNTER, &[0]);
        self.cmd_data(CMD_SET_Y_RANGE, &[0, 0, (HEIGHT - 1) as u8, 0]);
        self.cmd_data(CMD_SET_Y_COUNTER, &[0, 0]);
    }
}
