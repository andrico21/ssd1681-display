# ssd1681-display

`no_std`, zero-`unsafe`, async driver for **SSD1681**-based e-ink displays.

Written for the **GDEY0154D67** (1.54", 200x200, used in the
[M5Stack Air Quality Kit](https://docs.m5stack.com/en/core/Air_Quality)),
but should work with any SSD1681-based panel of the same resolution.

## Origin

This crate is an AI-aided Rust port and refactoring of the
[M5GFX](https://github.com/m5stack/M5GFX) C++ library (v0.2.19)
by M5Stack / lovyan03, specifically the `Panel_GDEW0154D67` panel driver
(`lgfx/v1/panel/Panel_GDEW0154D67.cpp`). The init sequence, register
values, and waveform timing were derived from that reference implementation,
then adapted to idiomatic async Rust using `embedded-hal` 1.0 traits.

## Features

- **`no_std`** - runs on bare-metal microcontrollers.
- **Zero `unsafe`** - the entire crate contains no `unsafe` blocks.
- **Async** - uses `embassy-time` for non-blocking delays during init/reset.
- **`embedded-graphics` integration** - the included `FrameBuf` implements
  `DrawTarget<Color = BinaryColor>`, so you can draw with any
  `embedded-graphics` primitive (text, shapes, images).
- **Full refresh mode control** - 12 preset modes covering every useful
  combination of the SSD1681 0x22 register, plus raw `u8` access for
  experimentation.

## Architecture

Three layers, each usable independently:

| Layer | Type | Purpose |
|-------|------|---------|
| `Display` | High-level facade | Owns driver + framebuffer, tracks power state |
| `Ssd1681` | Hardware driver | Direct SPI register access, any refresh mode |
| `FrameBuf` | Framebuffer | 200x200 1-bit buffer, `embedded-graphics` `DrawTarget` |

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `embedded-hal` | 1.0 | SPI + GPIO traits |
| `embedded-graphics-core` | 0.4 | `DrawTarget`, `BinaryColor`, geometry |
| `embassy-time` | 0.5 | Async `Timer` and `Instant` |

## Wiring

The driver is generic over `embedded-hal` traits - no GPIO numbers are
hardcoded. Pass any SPI device and GPIO pins that implement the right traits.

Reference pin assignment for the M5Stack Air Quality Kit (StampS3):

| Signal | GPIO | Direction |
|--------|------|-----------|
| MOSI | 6 | Output |
| SCLK | 5 | Output |
| CS | 4 | Output |
| D/C | 3 | Output |
| RST | 2 | Output |
| BUSY | 1 | Input (active high) |

## Quick start

```rust
use ssd1681_display::{Display, RefreshMode};
use embedded_graphics::{prelude::*, text::Text, mono_font::*};
use embedded_graphics::pixelcolor::BinaryColor;

// Create display from your board's SPI + GPIO pins:
let mut display = Display::new(spi_device, dc_pin, rst_pin, busy_pin);

// Initialize (hardware reset + SSD1681 register setup):
display.init().await;

// Draw with embedded-graphics:
let style = MonoTextStyle::new(&ascii::FONT_10X20, BinaryColor::Off);
Text::new("Hello e-ink!", Point::new(10, 30), style)
    .draw(display.draw_target())
    .unwrap();

// Refresh the physical display:
display.full_refresh();  // ~2089 ms, perfect quality
```

## Refresh modes

The SSD1681 0x22 register controls which steps execute during a refresh.
Different bit combinations trade speed for quality. All timings measured
on hardware with the GDEY0154D67 at 40 MHz SPI, ~25 C.

### Preset modes

| Mode | 0x22 value | Time | Quality | Flicker | Power after |
|------|------------|------|---------|---------|-------------|
| `FullF7` | 0xF7 | ~2089 ms | Perfect | Yes | Off |
| `ReuseC7` | 0xC7 | ~2086 ms | Good | Yes | Off |
| `QualityD7` | 0xD7 | ~2087 ms | Perfect | Yes | Off |
| `Mode2CF` | 0xCF | ~2086 ms | Good | Minimal | Off |
| `PoweredQuality` | 0xD4 | ~1945 ms | Perfect | Yes | On |
| `BareQuality` | 0x14 | ~1854 ms | Perfect | Yes | On (required) |
| `PoweredFast` | 0xDC | ~400 ms | Partial | No | On |
| `FullFastFC` | 0xFC | ~400 ms | Partial | No | On |
| `BareFast` | 0x1C | ~309 ms | Partial | No | On (required) |
| `Everything` | 0xFF | ~545 ms | Ghosting | Yes | Off |

"On (required)" means the mode expects clock/analog already enabled.
The `Display` facade handles this automatically with `fast_refresh()`.

### Convenience methods

```rust
// Full quality - reloads LUT, flickers, perfect output:
display.full_refresh();              // FullF7, ~2089 ms

// Fast differential - no flicker, auto powers on if needed:
display.fast_refresh();              // BareFast, ~309 ms

// Self-contained fast - no power tracking needed:
display.fast_refresh_self_contained(); // PoweredFast, ~400 ms
```

### Explicit mode selection

```rust
display.refresh(RefreshMode::QualityD7);  // ~2087 ms
display.refresh(RefreshMode::Mode2CF);    // ~2086 ms, differential
```

### Raw 0x22 parameter

For experimentation with undocumented combinations:

```rust
display.refresh_raw(0xDC);  // Same as PoweredFast
display.refresh_raw(0x94);  // Custom combination
```

### 0x22 register bit layout

| Bit | Function |
|-----|----------|
| 7 | Enable clock |
| 6 | Enable analog |
| 5 | Load temperature value |
| 4 | Load LUT - Display Mode 1 |
| 3 | Load LUT - Display Mode 2 (combined with bit 4) |
| 2 | Display with Mode 1 |
| 1 | Disable analog |
| 0 | Disable clock |

## Low-level driver access

For register-level control (custom LUT loading, partial windows, etc.),
access the `Ssd1681` driver directly:

```rust
let driver = display.driver();
driver.write_ram_both(framebuf.as_bytes());
driver.set_refresh_mode(RefreshMode::FullF7);
let busy_ms = driver.trigger_refresh();

// Keep power tracking in sync:
display.set_powered(false);
```

## Color mapping

| `BinaryColor` | Pixel | Bit value |
|----------------|-------|-----------|
| `Off` | Black | 0 |
| `On` | White | 1 |

The framebuffer is initialized to all-white (0xFF).

## License

MIT OR Apache-2.0
