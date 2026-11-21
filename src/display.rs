//! What the LCD and the RGB LEDs show. Shared by the real monitor (`bin/main.rs`) and the
//! offline demo (`bin/demo.rs`), so the demo shows exactly what the real thing would.

use core::fmt::{self, Write as _};

use log::warn;
use smart_leds::{RGB8, SmartLedsWrite, brightness};

use crate::lcd1602::Lcd1602;
use crate::sol::Sample;

pub const NUM_LEDS: usize = 7;
/// 0..255; the on-board LEDs are very bright, keep this low.
pub const LED_BRIGHTNESS: u8 = 20;

pub const BLUE: RGB8 = RGB8::new(0, 0, 255);
pub const RED: RGB8 = RGB8::new(255, 0, 0);

/// Two lines of text; a missing LCD is simply skipped.
pub fn show(lcd: &mut Option<Lcd1602<'_, '_>>, l0: &str, l1: &str) {
    if let Some(lcd) = lcd {
        let _ = lcd.write_line(0, l0);
        let _ = lcd.write_line(1, l1);
    }
}

/// The two LCD lines for a sample: `AUTO P  21 T  42` / `   957lx 15.2mW`.
/// `status` (e.g. `-> PARK`, `PIN wrong`) replaces the second line while it is shown.
pub fn sample_lines(s: &Sample, status: Option<&str>) -> (Buf<16>, Buf<16>) {
    let mut l0: Buf<16> = Buf::new();
    let mut l1: Buf<16> = Buf::new();
    let mode = s.mode.map_or("", |m| m.label());
    let _ = write!(
        l0,
        "{:<4} P{:>4.0} T{:>4.0}",
        mode,
        s.pan.unwrap_or(f32::NAN),
        s.tilt.unwrap_or(f32::NAN)
    );
    if let Some(st) = status {
        let _ = l1.write_str(st);
        return (l0, l1);
    }
    match s.lux {
        Some(lux) => {
            let _ = write!(l1, "{:>6.0}lx", lux);
        }
        None => {
            let _ = write!(l1, "     -lx");
        }
    }
    if let Some(p) = s.pmw {
        let _ = write!(l1, "{:>5.1}mW", p);
    }
    (l0, l1)
}

pub fn show_sample(lcd: &mut Option<Lcd1602<'_, '_>>, s: &Sample, status: Option<&str>) {
    let (l0, l1) = sample_lines(s, status);
    show(lcd, l0.as_str(), l1.as_str());
}

/// Light level 0..1 as a bar: more LEDs and warmer colour (red -> yellow) the brighter it is.
pub fn light_bar(lvl: Option<f32>) -> [RGB8; NUM_LEDS] {
    let Some(lvl) = lvl else {
        return single(BLUE); // connected, but the tracker has no light value yet
    };
    let mut out = [RGB8::default(); NUM_LEDS];
    let lvl = lvl.clamp(0.0, 1.0);
    let lit = ((lvl * NUM_LEDS as f32) + 0.5) as usize;
    for (i, led) in out.iter_mut().enumerate().take(lit.max(1)) {
        let g = (255 * i / (NUM_LEDS - 1)) as u8; // red at the first LED, yellow at the last
        *led = RGB8::new(255, g, 0);
    }
    out
}

/// Only the first LED on, in `c`.
pub fn single(c: RGB8) -> [RGB8; NUM_LEDS] {
    let mut out = [RGB8::default(); NUM_LEDS];
    out[0] = c;
    out
}

pub fn set_leds<L: SmartLedsWrite<Color = RGB8>>(leds: &mut L, colors: &[RGB8; NUM_LEDS])
where
    L::Error: fmt::Debug,
{
    if let Err(e) = leds.write(brightness(colors.iter().copied(), LED_BRIGHTNESS)) {
        warn!("LED write failed: {:?}", e);
    }
}

/// Tiny fixed-size string buffer so we can format text without a heap.
pub struct Buf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

impl<const N: usize> PartialEq for Buf<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl<const N: usize> Default for Buf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len < N {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}
