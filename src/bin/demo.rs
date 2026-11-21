//! Offline demo of the brainSOL display: no Wi-Fi, no tracker. Generates test data on the board
//! and runs it through the same display code as the real monitor (`solmon::display`).
//!
//! Same 60 s cycle as `tools/fake_tracker.py`:
//!    0- 3 s  empty history            -> "no data yet", blue LED
//!    3-40 s  sunny, the panel sweeps  -> angles change, LED bar mostly full
//!   40-50 s  a cloud passes           -> light, power and LED bar drop
//!   50-55 s  outage                   -> after 3 failed polls: "brainSOL offline", red LED
//!   55-60 s  sunny again
//!
//! With the joystick plugged in (VRX GPIO4, VRY GPIO5, SW GPIO6, see src/hw.rs) the demo also
//! shows what solmon would send to the tracker (`rate +0.6 -0.2`, `-> HOLD`) and plays the
//! tracker's part: steering switches to MAN and moves the panel, a press cycles AUTO/HOLD/PARK.
//! Good for checking the wiring and the stick directions without Wi-Fi.
//!
//! Run with: cargo run --release --bin demo     (no environment variables needed)

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::rmt::Rmt;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal_smartled::{RmtSmartLeds, buffer_size, color_order};
use libm::sinf;
use log::{info, warn};
use smart_leds::RGB8;
use core::fmt::Write as _;
use solmon::control::{Cmd, Joystick};
use solmon::display::{BLUE, Buf, NUM_LEDS, RED, light_bar, set_leds, show, show_sample, single};
use solmon::hw::JoystickHw;
use solmon::sol::Mode;
use solmon::lcd1602::{self, Lcd1602};
use solmon::sol::Sample;

esp_bootloader_esp_idf::esp_app_desc!();

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Empty,
    Sunny,
    Cloud,
    Down,
}

fn phase(t: f32) -> Phase {
    let s = t % 60.0;
    if s < 3.0 {
        Phase::Empty
    } else if s < 40.0 || s >= 55.0 {
        Phase::Sunny
    } else if s < 50.0 {
        Phase::Cloud
    } else {
        Phase::Down
    }
}

/// Same formulas as `row()` in tools/fake_tracker.py.
fn sample(t: f32) -> Sample {
    let pan = 60.0 * sinf(t / 20.0);
    let tilt = 45.0 + 15.0 * sinf(t / 13.0);
    let mut lvl = 0.85 + 0.1 * sinf(t / 3.0);
    if phase(t) == Phase::Cloud {
        let s = (t % 60.0) - 40.0; // 0..10 s into the cloud
        lvl *= 1.0 - 0.8 * sinf(core::f32::consts::PI * s / 10.0);
    }
    let lvl = lvl.clamp(0.0, 1.0);
    Sample { mode: Some(Mode::Auto), pan: Some(pan), tilt: Some(tilt), lvl: Some(lvl), lux: Some(1100.0 * lvl), pmw: Some(16.0 * lvl) }
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // LCD on the shared I2C bus (SCL GPIO10, SDA GPIO11), same as the real monitor.
    let mut i2c = I2c::new(peripherals.I2C0, I2cConfig::default().with_frequency(Rate::from_khz(100)))
        .unwrap()
        .with_scl(peripherals.GPIO10)
        .with_sda(peripherals.GPIO11);
    let lcd_addr = lcd1602::ADDRESSES
        .into_iter()
        .find(|&a| i2c.read(a, &mut [0u8; 1]).is_ok());
    let mut lcd = match lcd_addr {
        Some(a) => Lcd1602::new(&mut i2c, a).ok(),
        None => {
            warn!("no LCD found, LEDs and log only");
            None
        }
    };

    // 7x WS2812B on GPIO38, GRB.
    let freq = Rate::from_mhz(80);
    let rmt = Rmt::new(peripherals.RMT, freq).unwrap();
    let mut leds = RmtSmartLeds::<{ buffer_size::<RGB8>(NUM_LEDS) }, _, RGB8, color_order::Grb>::new_with_memsize(
        esp_hal_smartled::WS2812B_TIMING,
        rmt.channel0,
        peripherals.GPIO38,
        4,
        freq,
    )
    .unwrap();

    // Joystick, calibrated at boot (don't touch it while starting).
    let mut joy_hw = JoystickHw::new(peripherals.ADC1, peripherals.GPIO4, peripherals.GPIO5, peripherals.GPIO6);
    let mut joystick = joy_hw.calibrate().map(|(cx, cy)| (joy_hw, Joystick::new(cx, cy)));

    show(&mut lcd, "brainSOL monitor", "offline DEMO");
    set_leds(&mut leds, &single(BLUE));
    info!("offline demo: 60 s cycle empty -> sunny -> cloud -> outage -> sunny");
    Timer::after(Duration::from_secs(2)).await;

    // The simulated tracker's reaction to the joystick, like brainsolpi: steering -> manual,
    // press -> the mode solmon asks for. VMAX = brainsolpi's default vmax (config.py:32).
    const VMAX_DEG_S: f32 = 60.0;
    let mut mode = Mode::Auto;
    let (mut man_pan, mut man_tilt) = (0.0f32, 90.0f32);
    let mut rate = (0.0f32, 0.0f32);
    let mut status: Buf<16> = Buf::new();
    let mut status_until = Instant::now();
    let mut next_draw = Instant::now();

    let start = Instant::now();
    loop {
        let t = start.elapsed().as_millis() as f32 / 1000.0;

        // --- joystick, every 50 ms ---
        if let Some((hw, joy)) = joystick.as_mut() {
            let (x, y, pressed) = hw.read();
            if let Some(cmd) = joy.update(x, y, pressed, Some(mode), Instant::now().as_millis()) {
                status.clear();
                match cmd {
                    Cmd::Rate { pan, tilt } => {
                        if mode != Mode::Manual {
                            // take over from wherever the panel is now
                            let cur = current(mode, t, man_pan, man_tilt);
                            (man_pan, man_tilt) = cur;
                            mode = Mode::Manual;
                        }
                        rate = (pan, tilt);
                        let _ = write!(status, "rate {:+.1} {:+.1}", pan, tilt);
                    }
                    Cmd::Mode(m) => {
                        // HOLD freezes wherever the panel is right now
                        (man_pan, man_tilt) = current(mode, t, man_pan, man_tilt);
                        mode = m;
                        rate = (0.0, 0.0);
                        let _ = write!(status, "-> {}", m.label());
                    }
                }
                info!("would send {:?}  (x={:.0} y={:.0} mV)", cmd, x, y);
                status_until = Instant::now() + Duration::from_millis(1500);
                next_draw = Instant::now();
            }
        }
        if mode == Mode::Manual {
            man_pan = (man_pan + rate.0 * VMAX_DEG_S * 0.05).clamp(-90.0, 90.0);
            man_tilt = (man_tilt + rate.1 * VMAX_DEG_S * 0.05).clamp(0.0, 180.0);
        }

        // --- display, every 250 ms ---
        if Instant::now() >= next_draw {
            next_draw = Instant::now() + Duration::from_millis(250);
            let st = (Instant::now() < status_until).then(|| status.as_str());
            match phase(t) {
                Phase::Empty => {
                    show(&mut lcd, "brainSOL", st.unwrap_or("no data yet"));
                    set_leds(&mut leds, &single(BLUE));
                }
                Phase::Sunny | Phase::Cloud => {
                    let mut s = sample(t);
                    let (p, tl) = current(mode, t, man_pan, man_tilt);
                    s.mode = Some(mode);
                    s.pan = Some(p);
                    s.tilt = Some(tl);
                    show_sample(&mut lcd, &s, st);
                    set_leds(&mut leds, &light_bar(s.lvl));
                }
                Phase::Down => {
                    // like the real monitor: "offline" only after a few seconds without data
                    if (t % 60.0) - 50.0 >= 3.0 {
                        show(&mut lcd, "brainSOL offline", "DEMO outage");
                        set_leds(&mut leds, &single(RED));
                    }
                }
            }
        }
        Timer::after(Duration::from_millis(50)).await;
    }
}

/// Panel position the simulated tracker reports in `mode`.
fn current(mode: Mode, t: f32, man_pan: f32, man_tilt: f32) -> (f32, f32) {
    match mode {
        Mode::Manual | Mode::Hold => (man_pan, man_tilt),
        Mode::Park => (0.0, 90.0),
        _ => {
            let s = sample(t);
            (s.pan.unwrap_or(0.0), s.tilt.unwrap_or(0.0))
        }
    }
}
