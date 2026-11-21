//! solmon: status display and joystick remote for the brainSOL solar tracker.
//!
//! Keeps one WebSocket open to brainsolpi (`ws://SOL_HOST:SOL_PORT/ws`), shows mode, panel
//! angles, light and power on the LCD1602 and a light-level bar on the board's 7 RGB LEDs.
//! With `SOL_PIN` set it logs in as admin and the joystick steers the tracker (tilt) and
//! cycles modes auto → hold → park (press). See src/control.rs and src/sol.rs for the protocol.
//!
//! Build-time settings (environment variables, so nothing secret ends up in the code):
//!   WIFI_SSID, WIFI_PASS  - network (2.4 GHz, WPA2; WPA3-only is not supported by esp-radio)
//!   SOL_HOST              - IP address of the tracker (or the laptop running the simulation)
//!   SOL_PORT              - optional, default 8080
//!   SOL_PIN               - optional admin PIN; without it solmon is a read-only display
//! Example:
//!   WIFI_SSID=hotspot WIFI_PASS=secret SOL_HOST=192.168.43.10 SOL_PIN=1234 cargo run --release
//!
//! Safety: wrong PINs lock out every admin of the tracker, incl. the web page (brainsolpi
//! auth.py: 5 tries, then 30 s doubling up to 1 h). solmon therefore sends the PIN at most once
//! per boot and never retries after a refusal; reconnects use the token from the first login.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources, dns::DnsQueryType, tcp::TcpSocket};
use embassy_time::{Duration, Instant, Timer, with_timeout};
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
use smart_leds::{RGB8, SmartLedsWrite};
use solmon::control::{Cmd, Joystick, auth_json, cmd_json};
use solmon::display::{BLUE, Buf, NUM_LEDS, RED, light_bar, sample_lines, set_leds, show, single};
use solmon::hw::JoystickHw;
use solmon::lcd1602::{self, Lcd1602};
use solmon::sol::{Frame, Sample, parse_frame};
use solmon::ws;
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const WIFI_SSID: &str = env!("WIFI_SSID");
const WIFI_PASS: &str = env!("WIFI_PASS");
const SOL_HOST: &str = env!("SOL_HOST");
const SOL_PORT: Option<&str> = option_env!("SOL_PORT");
const SOL_PIN: Option<&str> = option_env!("SOL_PIN");

/// Joystick poll interval.
const TICK: Duration = Duration::from_millis(50);
/// LCD/LED refresh limit; state frames arrive at 20 Hz and an LCD update takes ~50 ms.
const DRAW_EVERY: Duration = Duration::from_millis(250);
/// How long a status message replaces the second LCD line.
const STATUS_FOR: Duration = Duration::from_millis(1500);
/// No frame for this long = connection dead (the tracker sends 20 frames per second).
const SILENCE: Duration = Duration::from_secs(5);

#[allow(clippy::large_stack_frames, reason = "buffers live in main")]
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

    // --- joystick (GPIO4/5/6); calibrated now, so don't touch it while booting ---
    let mut joy_hw = JoystickHw::new(peripherals.ADC1, peripherals.GPIO4, peripherals.GPIO5, peripherals.GPIO6);
    let joystick = joy_hw.calibrate().map(|(cx, cy)| (joy_hw, Joystick::new(cx, cy)));
    if SOL_PIN.is_none() {
        info!("SOL_PIN not set: read-only display, joystick ignored");
    }

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
    info!("connecting to ws://{}:{}/ws", SOL_HOST, port);
    show(&mut lcd, "brainSOL monitor", "tracker...");

    let mut app = App {
        lcd,
        leds,
        joystick,
        rng,
        token: Buf::new(),
        pin_failed: false,
        admin: false,
        auth_sent_token: false,
        last: None,
        got_state: false,
        status: Buf::new(),
        status_until: Instant::now(),
        next_draw: Instant::now(),
        drawn: (Buf::new(), Buf::new()),
        cmd_id: 0,
        warned_read_only: false,
    };

    let mut fails = 0u32;
    loop {
        app.got_state = false;
        let result = session(stack, port, &mut app).await;
        warn!("connection ended: {:?}", result);
        app.admin = false;
        fails = if app.got_state { 1 } else { fails + 1 };
        // one hiccup is normal on Wi-Fi; only show offline after a few in a row
        if fails >= 3 {
            show(&mut app.lcd, "brainSOL offline", SOL_HOST);
            app.drawn = (Buf::new(), Buf::new());
            set_leds(&mut app.leds, &single(RED));
        }
        Timer::after(Duration::from_secs(2)).await;
    }
}

struct App<'a, 'd, L> {
    lcd: Option<Lcd1602<'a, 'd>>,
    leds: L,
    joystick: Option<(JoystickHw<'d>, Joystick)>,
    rng: Rng,
    /// Token from the first successful PIN login, reused on reconnects.
    token: Buf<64>,
    /// A PIN was refused: never send it again until reboot (lockout affects the whole team).
    pin_failed: bool,
    admin: bool,
    auth_sent_token: bool,
    last: Option<Sample>,
    got_state: bool,
    status: Buf<16>,
    status_until: Instant,
    next_draw: Instant,
    drawn: (Buf<16>, Buf<16>),
    cmd_id: u32,
    warned_read_only: bool,
}

#[derive(Debug)]
enum SessionError {
    Dns,
    Connect,
    Handshake,
    Io,
    Closed,
    Silent,
}

/// One WebSocket connection, from connect until it breaks.
async fn session<L>(stack: Stack<'_>, port: u16, app: &mut App<'_, '_, L>) -> Result<(), SessionError>
where
    L: SmartLedsWrite<Color = RGB8>,
    L::Error: core::fmt::Debug,
{
    let addr = *stack
        .dns_query(SOL_HOST, DnsQueryType::A)
        .await
        .map_err(|_| SessionError::Dns)?
        .first()
        .ok_or(SessionError::Dns)?;

    let mut rx = [0u8; 4096];
    let mut tx = [0u8; 1024];
    let mut sock = TcpSocket::new(stack, &mut rx, &mut tx);
    sock.set_timeout(Some(SILENCE));
    with_timeout(Duration::from_secs(5), sock.connect((addr, port)))
        .await
        .map_err(|_| SessionError::Connect)?
        .map_err(|_| SessionError::Connect)?;

    // --- HTTP upgrade ---
    let mut key = [0u8; 16];
    app.rng.read(&mut key);
    let mut buf = [0u8; 2048];
    let n = ws::handshake_request(SOL_HOST, port, &key, &mut buf);
    sock.write_all(&buf[..n]).await.map_err(|_| SessionError::Io)?;
    let mut len = 0;
    let hdr = loop {
        let k = sock.read(&mut buf[len..]).await.map_err(|_| SessionError::Io)?;
        if k == 0 {
            return Err(SessionError::Closed);
        }
        len += k;
        match ws::handshake_response(&buf[..len]) {
            Some(Ok(h)) => break h,
            Some(Err(())) => return Err(SessionError::Handshake),
            None if len == buf.len() => return Err(SessionError::Handshake),
            None => {}
        }
    };
    buf.copy_within(hdr..len, 0);
    len -= hdr;
    info!("WebSocket open");

    // --- login, if we are allowed to try ---
    if let Some(pin) = SOL_PIN {
        let token = (!app.token.is_empty()).then(|| app.token.as_str());
        if token.is_some() || !app.pin_failed {
            let mut msg: Buf<128> = Buf::new();
            let _ = auth_json(Some(pin), token, &mut msg);
            app.auth_sent_token = token.is_some();
            send_text(&mut sock, &mut app.rng, msg.as_bytes()).await?;
        }
    }

    // --- frames + joystick ---
    let mut discard: u64 = 0; // bytes left of a frame too big for `buf`
    let mut next_tick = Instant::now() + TICK;
    let mut last_frame = Instant::now();
    loop {
        // handle every complete frame in the buffer
        while let Some(h) = ws::parse_header(&buf[..len]) {
            let total = h.hdr_len as u64 + h.len;
            if total > buf.len() as u64 {
                // e.g. a large `hello`; we don't need it, skip its bytes
                discard = total - len as u64;
                len = 0;
                break;
            }
            let total = total as usize;
            if len < total {
                break;
            }
            last_frame = Instant::now();
            let payload = &mut buf[h.hdr_len..total];
            if let Some(m) = h.mask {
                ws::unmask(payload, m);
            }
            match h.opcode {
                ws::OP_TEXT => {
                    if let Ok(text) = core::str::from_utf8(payload) {
                        app.on_text(text);
                    }
                }
                ws::OP_PING => {
                    let mut pong = [0u8; 131];
                    let n = ws::encode_frame(ws::OP_PONG, payload, mask(&app.rng), &mut pong);
                    sock.write_all(&pong[..n]).await.map_err(|_| SessionError::Io)?;
                }
                ws::OP_CLOSE => return Err(SessionError::Closed),
                _ => {}
            }
            buf.copy_within(total..len, 0);
            len -= total;
        }

        // wait for more bytes or the next joystick tick
        match select(sock.read(&mut buf[len..]), Timer::at(next_tick)).await {
            Either::First(Ok(0)) => return Err(SessionError::Closed),
            Either::First(Ok(k)) => {
                if discard > 0 {
                    let drop = (k as u64).min(discard) as usize;
                    discard -= drop as u64;
                    buf.copy_within(len + drop..len + k, len);
                    len += k - drop;
                } else {
                    len += k;
                }
            }
            Either::First(Err(_)) => return Err(SessionError::Io),
            Either::Second(()) => {
                next_tick += TICK;
                if Instant::now() > next_tick {
                    next_tick = Instant::now() + TICK; // we fell behind (slow LCD write), don't burst
                }
                if last_frame.elapsed() > SILENCE {
                    return Err(SessionError::Silent);
                }
                if let Some(cmd) = app.poll_joystick() {
                    let mut msg: Buf<128> = Buf::new();
                    app.cmd_id = app.cmd_id.wrapping_add(1);
                    let _ = cmd_json(app.cmd_id, cmd, &mut msg);
                    send_text(&mut sock, &mut app.rng, msg.as_bytes()).await?;
                }
                app.draw();
            }
        }
    }
}

impl<L> App<'_, '_, L>
where
    L: SmartLedsWrite<Color = RGB8>,
    L::Error: core::fmt::Debug,
{
    fn on_text(&mut self, text: &str) {
        match parse_frame(text) {
            Frame::State(s) => {
                self.last = Some(s);
                self.got_state = true;
            }
            Frame::Auth { ok: true, token, .. } => {
                self.admin = true;
                if let Some(t) = token {
                    self.token.clear();
                    let _ = self.token.write_str(t);
                }
                info!("admin login ok, joystick control on");
                self.set_status(if self.joystick.is_some() { "control on" } else { "no joystick" });
            }
            Frame::Auth { ok: false, msg, wait, .. } => {
                warn!("login refused: {}", msg.unwrap_or("?"));
                if self.auth_sent_token {
                    // expired token: costs no PIN try; the next connection logs in with the PIN once
                    self.token.clear();
                } else {
                    self.pin_failed = true;
                    let mut s: Buf<16> = Buf::new();
                    if wait > 0.0 {
                        let _ = write!(s, "locked {:.0}s", wait);
                    } else {
                        let _ = s.write_str("PIN wrong");
                    }
                    self.set_status(s.as_str());
                }
            }
            Frame::Ack { ok: false, msg, id } => {
                warn!("command {:?} refused: {}", id, msg.unwrap_or("?"));
                self.set_status("refused");
            }
            Frame::Ack { .. } | Frame::Other => {}
        }
    }

    /// Reads the stick; returns a command only when we may send it.
    fn poll_joystick(&mut self) -> Option<Cmd> {
        let (hw, joy) = self.joystick.as_mut()?;
        let (x, y, pressed) = hw.read();
        let mode = self.last.and_then(|s| s.mode);
        let cmd = joy.update(x, y, pressed, mode, Instant::now().as_millis())?;
        if !self.admin {
            if !self.warned_read_only {
                self.warned_read_only = true;
                warn!("joystick used, but not logged in as admin (SOL_PIN missing or refused)");
            }
            self.set_status(if SOL_PIN.is_none() { "read only" } else { "no admin" });
            return None;
        }
        if let Cmd::Mode(m) = cmd {
            let mut s: Buf<16> = Buf::new();
            let _ = write!(s, "-> {}", m.label());
            self.set_status(s.as_str());
        }
        info!("send {:?}", cmd);
        Some(cmd)
    }

    fn set_status(&mut self, s: &str) {
        self.status.clear();
        let _ = self.status.write_str(s);
        self.status_until = Instant::now() + STATUS_FOR;
        self.next_draw = Instant::now(); // show it right away
    }

    /// Redraws LCD and LEDs from the latest state, at most every DRAW_EVERY, and only lines that changed.
    fn draw(&mut self) {
        let now = Instant::now();
        if now < self.next_draw {
            return;
        }
        self.next_draw = now + DRAW_EVERY;
        let Some(s) = self.last else { return };
        let status = (now < self.status_until).then(|| self.status.as_str());
        let (l0, l1) = sample_lines(&s, status);
        if let Some(lcd) = self.lcd.as_mut() {
            if l0 != self.drawn.0 {
                let _ = lcd.write_line(0, l0.as_str());
            }
            if l1 != self.drawn.1 {
                let _ = lcd.write_line(1, l1.as_str());
            }
        }
        self.drawn = (l0, l1);
        set_leds(&mut self.leds, &light_bar(s.lvl));
    }
}

fn mask(rng: &Rng) -> [u8; 4] {
    rng.random().to_le_bytes()
}

async fn send_text(sock: &mut TcpSocket<'_>, rng: &mut Rng, payload: &[u8]) -> Result<(), SessionError> {
    let mut frame = [0u8; 160];
    let n = ws::encode_frame(ws::OP_TEXT, payload, mask(rng), &mut frame);
    if n == 0 {
        return Err(SessionError::Io);
    }
    sock.write_all(&frame[..n]).await.map_err(|_| SessionError::Io)
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
