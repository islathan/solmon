//! solmon: a small status display for the brainSOL solar tracker.
//!
//! Every second it fetches `http://SOL_HOST:SOL_PORT/api/history?span=2` from brainsolpi over
//! Wi-Fi and shows panel angles, light and power on the LCD1602, plus a light-level bar on the
//! board's 7 RGB LEDs.
//!
//! Build-time settings (environment variables, so nothing secret ends up in the code):
//!   WIFI_SSID, WIFI_PASS  - network (2.4 GHz, WPA2; WPA3-only is not supported by esp-radio)
//!   SOL_HOST              - IP address of the tracker (or the laptop running the simulation)
//!   SOL_PORT              - optional, default 8080
//! Example:
//!   WIFI_SSID=hotspot WIFI_PASS=secret SOL_HOST=192.168.43.10 cargo run --release

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources, dns::DnsQueryType, tcp::TcpSocket};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::Write as _;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::rmt::Rmt;
use esp_hal::rng::Rng;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal_smartled::{RmtSmartLeds, buffer_size, color_order};
use esp_radio::wifi::{AuthenticationMethodConfig, Config, Interface, WifiController, sta::StationConfig};
use log::{info, warn};
use smart_leds::RGB8;
use solmon::display::{BLUE, Buf, NUM_LEDS, RED, light_bar, set_leds, show, show_sample, single};
use solmon::lcd1602::{self, Lcd1602};
use solmon::sol::parse_history;
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");
const SOL_HOST: &str = env!("SOL_HOST");
const SOL_PORT: Option<&str> = option_env!("SOL_PORT");


#[allow(
    clippy::large_stack_frames,
    reason = "buffers for HTTP live in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- LCD on the shared I2C bus (SCL GPIO10, SDA GPIO11; schematic header P1) ---
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
            warn!("no LCD found, continuing with LEDs and log only");
            None
        }
    };
    show(&mut lcd, "brainSOL monitor", "WiFi connecting");

    // --- 7x WS2812B on GPIO38 (schematic: LED_DIN), GRB order ---
    let freq = Rate::from_mhz(80);
    let rmt = Rmt::new(peripherals.RMT, freq).unwrap();
    // memsize 4 = 4 RMT blocks, enough to hold all 7*24 bits + reset in one go.
    let mut leds = RmtSmartLeds::<{ buffer_size::<RGB8>(NUM_LEDS) }, _, RGB8, color_order::Grb>::new_with_memsize(
        esp_hal_smartled::WS2812B_TIMING,
        rmt.channel0,
        peripherals.GPIO38,
        4,
        freq,
    )
    .unwrap();
    set_leds(&mut leds, &single(BLUE));

    // --- Wi-Fi station + network stack ---
    let mut controller = WifiController::new(peripherals.WIFI, Default::default()).expect("Wi-Fi init failed");
    let auth = if WIFI_PASS.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::Wpa2Personal(WIFI_PASS.try_into().expect("WIFI_PASS too long"))
    };
    controller
        .set_config(&Config::Station(
            StationConfig::default()
                .with_ssid(WIFI_SSID.try_into().expect("WIFI_SSID too long"))
                .with_authentication(auth),
        ))
        .expect("Wi-Fi config failed");

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());

    info!("waiting for Wi-Fi '{}' and DHCP...", WIFI_SSID);
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        info!("got IP {}", cfg.address);
    }

    let port: u16 = SOL_PORT.and_then(|p| p.parse().ok()).unwrap_or(8080);
    info!("polling http://{}:{}/api/history", SOL_HOST, port);
    show(&mut lcd, "brainSOL monitor", "tracker...");

    let mut body = [0u8; 2048];
    let mut fails = 0u32;
    loop {
        match fetch(stack, port, &mut body).await {
            Ok(n) => match core::str::from_utf8(&body[..n]).ok().map(parse_history) {
                Some(Ok(Some(s))) => {
                    fails = 0;
                    info!("{:?}", s);
                    show_sample(&mut lcd, &s);
                    set_leds(&mut leds, &light_bar(s.lvl));
                }
                Some(Ok(None)) => show(&mut lcd, "brainSOL", "no data yet"),
                _ => {
                    warn!("could not parse answer");
                    show(&mut lcd, "brainSOL", "bad answer");
                }
            },
            Err(e) => {
                fails += 1;
                warn!("fetch failed: {:?}", e);
                // one hiccup is normal on Wi-Fi; only show offline after a few in a row
                if fails >= 3 {
                    show(&mut lcd, "brainSOL offline", SOL_HOST);
                    set_leds(&mut leds, &single(RED));
                }
            }
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[derive(Debug)]
enum FetchError {
    Dns,
    Connect,
    Io,
    Timeout,
    Http,
}

/// Plain HTTP/1.0 GET; returns the length of the body written into `out`.
async fn fetch(stack: Stack<'_>, port: u16, out: &mut [u8]) -> Result<usize, FetchError> {
    let addr = *stack
        .dns_query(SOL_HOST, DnsQueryType::A)
        .await
        .map_err(|_| FetchError::Dns)?
        .first()
        .ok_or(FetchError::Dns)?;

    let mut rx = [0u8; 2048];
    let mut tx = [0u8; 256];
    let mut sock = TcpSocket::new(stack, &mut rx, &mut tx);
    sock.set_timeout(Some(Duration::from_secs(5)));
    with_timeout(Duration::from_secs(5), sock.connect((addr, port)))
        .await
        .map_err(|_| FetchError::Timeout)?
        .map_err(|_| FetchError::Connect)?;

    let mut req: Buf<160> = Buf::new();
    let _ = write!(
        req,
        "GET /api/history?span=2 HTTP/1.0\r\nHost: {SOL_HOST}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(req.as_bytes()).await.map_err(|_| FetchError::Io)?;

    // Read until the server closes the connection.
    let mut n = 0;
    loop {
        if n == out.len() {
            break;
        }
        match sock.read(&mut out[n..]).await {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return Err(FetchError::Io),
        }
    }
    sock.close();

    let resp = &out[..n];
    if !(resp.starts_with(b"HTTP/1.0 200") || resp.starts_with(b"HTTP/1.1 200")) {
        return Err(FetchError::Http);
    }
    let header_end = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or(FetchError::Http)? + 4;
    out.copy_within(header_end..n, 0);
    Ok(n - header_end)
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    loop {
        if controller.is_connected() {
            let _ = controller.wait_for_disconnect_async().await;
            warn!("Wi-Fi disconnected");
            Timer::after(Duration::from_secs(2)).await;
        }
        match controller.connect_async().await {
            Ok(_) => info!("Wi-Fi connected"),
            Err(e) => {
                warn!("Wi-Fi connect failed: {:?}", e);
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}
