//! Joystick hardware: Freenove joystick module on header P1 (wire by signal label; schematic and
//! Waveshare's pinout image number the header from opposite ends, see README):
//!   VCC → 3V3, GND → GND, VRX → GPIO4, VRY → GPIO5, SW → GPIO6
//! GPIO4/5 are ADC1 channels; ADC1 keeps working while Wi-Fi is on (ADC2 would not).
//! SW pulls to GND when pressed, so GPIO6 uses the internal pull-up.

use esp_hal::Blocking;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::peripherals::{ADC1, GPIO4, GPIO5, GPIO6};
use log::{info, warn};

type Cal<'d> = AdcCalCurve<ADC1<'d>>;

/// A resting stick reads about half the supply. Outside this window (or noisy) at boot we assume
/// nothing is plugged in: floating ADC pins would otherwise send random steering commands.
const CENTER_MIN_MV: f32 = 1100.0;
const CENTER_MAX_MV: f32 = 2200.0;
const CENTER_MAX_SPREAD_MV: f32 = 150.0;

pub struct JoystickHw<'d> {
    adc: Adc<'d, ADC1<'d>, Blocking>,
    x: AdcPin<GPIO4<'d>, ADC1<'d>, Cal<'d>>,
    y: AdcPin<GPIO5<'d>, ADC1<'d>, Cal<'d>>,
    sw: Input<'d>,
}

impl<'d> JoystickHw<'d> {
    pub fn new(adc1: ADC1<'d>, x: GPIO4<'d>, y: GPIO5<'d>, sw: GPIO6<'d>) -> Self {
        let mut cfg = AdcConfig::new();
        let x = cfg.enable_pin_with_cal::<_, Cal<'d>>(x, Attenuation::_11dB);
        let y = cfg.enable_pin_with_cal::<_, Cal<'d>>(y, Attenuation::_11dB);
        let adc = Adc::new(adc1, cfg);
        let sw = Input::new(sw, InputConfig::default().with_pull(Pull::Up));
        Self { adc, x, y, sw }
    }

    /// (x mV, y mV, pressed), each axis averaged over 4 samples.
    pub fn read(&mut self) -> (f32, f32, bool) {
        let (mut sx, mut sy) = (0u32, 0u32);
        for _ in 0..4 {
            sx += self.adc.read_blocking(&mut self.x) as u32;
            sy += self.adc.read_blocking(&mut self.y) as u32;
        }
        (sx as f32 / 4.0, sy as f32 / 4.0, self.sw.is_low())
    }

    /// Measures the resting position (don't touch the stick at boot). `None` = no joystick found.
    pub fn calibrate(&mut self) -> Option<(f32, f32)> {
        let (mut sum_x, mut sum_y) = (0.0f32, 0.0f32);
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        const N: usize = 32;
        for _ in 0..N {
            let (x, y, _) = self.read();
            sum_x += x;
            sum_y += y;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let (cx, cy) = (sum_x / N as f32, sum_y / N as f32);
        let ok = |c: f32, lo: f32, hi: f32| {
            (CENTER_MIN_MV..=CENTER_MAX_MV).contains(&c) && hi - lo <= CENTER_MAX_SPREAD_MV
        };
        if ok(cx, min_x, max_x) && ok(cy, min_y, max_y) {
            info!("joystick center x={cx:.0} mV y={cy:.0} mV");
            Some((cx, cy))
        } else {
            warn!(
                "no joystick (center x={cx:.0} [{min_x:.0}..{max_x:.0}] y={cy:.0} [{min_y:.0}..{max_y:.0}] mV) - control off"
            );
            None
        }
    }
}
