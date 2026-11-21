//! Minimal driver for an HD44780 16x2 LCD behind a PCF8574 I2C backpack
//! (the Freenove FNK0020 "I2C LCD1602" module).
//!
//! PCF8574 bit layout, taken from the kit's own example
//! `Freenove_.../Code/C_Code/20.1.1_I2CLCD1602/I2CLCD1602.c`:
//! P0 = RS, P1 = RW, P2 = EN, P3 = backlight, P4..P7 = D4..D7.
//!
//! The init sequence (three times 0x3, then 0x2 to enter 4-bit mode) and command
//! codes follow the HD44780 datasheet; see also `Datasheet/LCD1602.pdf` in the kit repo.

use core::fmt;

use esp_hal::{Blocking, delay::Delay, i2c::master::{Error, I2c}};

const RS: u8 = 0b0000_0001;
const EN: u8 = 0b0000_0100;
const BACKLIGHT: u8 = 0b0000_1000;

/// Common I2C addresses of the backpack: PCF8574T = 0x27, PCF8574AT = 0x3F.
pub const ADDRESSES: [u8; 2] = [0x27, 0x3F];

pub struct Lcd1602<'a, 'd> {
    i2c: &'a mut I2c<'d, Blocking>,
    addr: u8,
    backlight: u8,
    delay: Delay,
}

impl<'a, 'd> Lcd1602<'a, 'd> {
    /// Initialises the display: 4-bit mode, 2 lines, display on, cursor off, cleared.
    pub fn new(i2c: &'a mut I2c<'d, Blocking>, addr: u8) -> Result<Self, Error> {
        let mut lcd = Self { i2c, addr, backlight: BACKLIGHT, delay: Delay::new() };

        lcd.delay.delay_millis(50); // power-on wait (>40 ms after Vcc rises)
        lcd.write_nibble(0x3, 0)?;
        lcd.delay.delay_micros(4500);
        lcd.write_nibble(0x3, 0)?;
        lcd.delay.delay_micros(4500);
        lcd.write_nibble(0x3, 0)?;
        lcd.delay.delay_micros(150);
        lcd.write_nibble(0x2, 0)?; // switch to 4-bit interface

        lcd.command(0x28)?; // function set: 4-bit, 2 lines, 5x8 font
        lcd.command(0x0C)?; // display on, cursor off, blink off
        lcd.clear()?;
        lcd.command(0x06)?; // entry mode: move cursor right, no shift
        Ok(lcd)
    }

    pub fn clear(&mut self) -> Result<(), Error> {
        self.command(0x01)?;
        self.delay.delay_millis(2); // clear is slow (~1.5 ms)
        Ok(())
    }

    /// Moves the cursor; `row` is 0 or 1, `col` 0..=15.
    pub fn set_cursor(&mut self, col: u8, row: u8) -> Result<(), Error> {
        let row_offset = if row == 0 { 0x00 } else { 0x40 };
        self.command(0x80 | (row_offset + col))
    }

    pub fn set_backlight(&mut self, on: bool) -> Result<(), Error> {
        self.backlight = if on { BACKLIGHT } else { 0 };
        self.i2c.write(self.addr, &[self.backlight])
    }

    pub fn write_str(&mut self, s: &str) -> Result<(), Error> {
        for b in s.bytes() {
            self.write_byte(b, RS)?;
        }
        Ok(())
    }

    /// Writes a whole 16-character line, padding with spaces so old text disappears.
    pub fn write_line(&mut self, row: u8, s: &str) -> Result<(), Error> {
        self.set_cursor(0, row)?;
        let mut n = 0;
        for b in s.bytes().take(16) {
            self.write_byte(b, RS)?;
            n += 1;
        }
        for _ in n..16 {
            self.write_byte(b' ', RS)?;
        }
        Ok(())
    }

    fn command(&mut self, cmd: u8) -> Result<(), Error> {
        self.write_byte(cmd, 0)
    }

    fn write_byte(&mut self, value: u8, mode: u8) -> Result<(), Error> {
        self.write_nibble(value >> 4, mode)?;
        self.write_nibble(value & 0x0F, mode)
    }

    /// Puts 4 data bits on D4..D7 and pulses EN so the LCD latches them.
    fn write_nibble(&mut self, nibble: u8, mode: u8) -> Result<(), Error> {
        let data = (nibble << 4) | mode | self.backlight;
        self.i2c.write(self.addr, &[data | EN])?;
        self.delay.delay_micros(1);
        self.i2c.write(self.addr, &[data])?;
        self.delay.delay_micros(50); // most commands need ~37 us
        Ok(())
    }
}

/// Lets you use `write!(lcd, "T: {}", x)`; errors are mapped to `fmt::Error`.
impl fmt::Write for Lcd1602<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        Lcd1602::write_str(self, s).map_err(|_| fmt::Error)
    }
}
