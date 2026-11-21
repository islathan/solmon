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
use solmon::display::{BLUE, NUM_LEDS, RED, light_bar, set_leds, show, show_sample, single};
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
    Sample { pan: Some(pan), tilt: Some(tilt), lvl: Some(lvl), lux: Some(1100.0 * lvl), pmw: Some(16.0 * lvl) }
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

    show(&mut lcd, "brainSOL monitor", "offline DEMO");
    set_leds(&mut leds, &single(BLUE));
    info!("offline demo: 60 s cycle empty -> sunny -> cloud -> outage -> sunny");
    Timer::after(Duration::from_secs(2)).await;

    let start = Instant::now();
    let mut fails = 0u32;
    loop {
        let t = start.elapsed().as_millis() as f32 / 1000.0;
        let p = phase(t);
        match p {
            Phase::Empty => {
                fails = 0;
                show(&mut lcd, "brainSOL", "no data yet");
                set_leds(&mut leds, &single(BLUE));
            }
            Phase::Sunny | Phase::Cloud => {
                fails = 0;
                let s = sample(t);
                show_sample(&mut lcd, &s);
                set_leds(&mut leds, &light_bar(s.lvl));
            }
            Phase::Down => {
                // like the real monitor: only show "offline" after 3 failed polls in a row
                fails += 1;
                if fails >= 3 {
                    show(&mut lcd, "brainSOL offline", "DEMO outage");
                    set_leds(&mut leds, &single(RED));
                }
            }
        }
        info!("[{:5.1}s {:?}] fails={}", t % 60.0, p, fails);
        Timer::after(Duration::from_secs(1)).await;
    }
}
