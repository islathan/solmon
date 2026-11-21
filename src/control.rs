//! Joystick → brainsolpi commands. Pure logic (no hardware, no IO), tested on the laptop.
//!
//! - Tilting the stick sends `{"op":"rate","pan":..,"tilt":..}` (-1..1 each). brainsolpi switches
//!   to manual by itself and lets the rate fade to 0 when it is not refreshed within 0.4 s
//!   (brainsolpi/brainsolpi/engine.py:710, modes.py RATE_TTL), so we resend every 150 ms while
//!   the stick is out of its dead zone and send one 0/0 when it comes back.
//! - Pressing the stick sends `{"op":"mode",..}`: auto → hold → park → auto; from any other mode
//!   (e.g. manual after steering) back to auto. (brainsolpi/brainsolpi/commands.py:98)

use core::fmt::{self, Write};

use crate::sol::Mode;

/// Fraction of the travel around the center that counts as "not touched".
pub const DEAD_ZONE: f32 = 0.12;
/// Resend interval while steering; must stay well below brainsolpi's 400 ms RATE_TTL.
pub const RESEND_MS: u64 = 150;
/// Ignore button edges closer together than this (contact bounce).
pub const DEBOUNCE_MS: u64 = 200;
/// ADC reading at full deflection with 11 dB attenuation (roughly 3.1 V, from memory of the
/// ESP32-S3 datasheet; readings above are clamped anyway).
pub const ADC_MAX_MV: f32 = 3100.0;

/// Mounting of the stick: flip these if the panel moves the wrong way.
pub const SWAP_AXES: bool = false;
pub const INVERT_PAN: bool = false;
pub const INVERT_TILT: bool = true; // pushing the stick away usually reads low on these modules

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Rate { pan: f32, tilt: f32 },
    Mode(Mode),
}

/// Maps a raw reading (mV) to -1..1 around the calibrated center, with dead zone.
pub fn axis(raw_mv: f32, center_mv: f32) -> f32 {
    let d = raw_mv - center_mv;
    let span = if d < 0.0 { center_mv } else { ADC_MAX_MV - center_mv };
    if span <= 1.0 {
        return 0.0;
    }
    let v = (d / span).clamp(-1.0, 1.0);
    let a = if v < 0.0 { -v } else { v };
    if a < DEAD_ZONE {
        0.0
    } else {
        let scaled = (a - DEAD_ZONE) / (1.0 - DEAD_ZONE);
        if v < 0.0 { -scaled } else { scaled }
    }
}

/// The mode a button press switches to, given what the tracker is doing now.
pub fn next_mode(current: Option<Mode>) -> Mode {
    match current {
        Some(Mode::Auto) => Mode::Hold,
        Some(Mode::Hold) => Mode::Park,
        _ => Mode::Auto,
    }
}

pub struct Joystick {
    pub center_x: f32,
    pub center_y: f32,
    steering: bool,
    last_rate_ms: u64,
    pressed: bool,
    last_edge_ms: u64,
}

impl Joystick {
    /// `center_*` = readings with the stick at rest, measured at boot.
    pub fn new(center_x: f32, center_y: f32) -> Self {
        Self { center_x, center_y, steering: false, last_rate_ms: 0, pressed: false, last_edge_ms: 0 }
    }

    /// Current stick position as (pan, tilt), -1..1, after dead zone and mounting flips.
    pub fn position(&self, x_mv: f32, y_mv: f32) -> (f32, f32) {
        let (x, y) = (axis(x_mv, self.center_x), axis(y_mv, self.center_y));
        let (mut pan, mut tilt) = if SWAP_AXES { (y, x) } else { (x, y) };
        if INVERT_PAN {
            pan = -pan;
        }
        if INVERT_TILT {
            tilt = -tilt;
        }
        // `-0.0 + 0.0 == +0.0`: no "-0.00" in the JSON when an inverted axis rests
        (pan + 0.0, tilt + 0.0)
    }

    /// Call every ~50 ms. Returns the command to send now, if any.
    /// `mode` = the tracker's current mode from the latest state frame.
    pub fn update(&mut self, x_mv: f32, y_mv: f32, pressed: bool, mode: Option<Mode>, now_ms: u64) -> Option<Cmd> {
        // Button: act on the press edge, debounced.
        if pressed != self.pressed && now_ms.saturating_sub(self.last_edge_ms) >= DEBOUNCE_MS {
            self.pressed = pressed;
            self.last_edge_ms = now_ms;
            if pressed {
                return Some(Cmd::Mode(next_mode(mode)));
            }
        }

        let (pan, tilt) = self.position(x_mv, y_mv);
        if pan != 0.0 || tilt != 0.0 {
            if !self.steering || now_ms.saturating_sub(self.last_rate_ms) >= RESEND_MS {
                self.steering = true;
                self.last_rate_ms = now_ms;
                return Some(Cmd::Rate { pan, tilt });
            }
        } else if self.steering {
            self.steering = false;
            self.last_rate_ms = now_ms;
            return Some(Cmd::Rate { pan: 0.0, tilt: 0.0 });
        }
        None
    }
}

/// `{"t":"cmd","id":7,"op":"rate","pan":0.52,"tilt":-0.1}` / `{"t":"cmd","id":8,"op":"mode","mode":"park"}`
pub fn cmd_json(id: u32, cmd: Cmd, out: &mut impl Write) -> fmt::Result {
    match cmd {
        Cmd::Rate { pan, tilt } => {
            write!(out, r#"{{"t":"cmd","id":{id},"op":"rate","pan":{pan:.2},"tilt":{tilt:.2}}}"#)
        }
        Cmd::Mode(m) => write!(out, r#"{{"t":"cmd","id":{id},"op":"mode","mode":"{}"}}"#, m.as_str()),
    }
}

/// `{"t":"auth","pin":"1234"}` (first login) or `{"t":"auth","token":".."}` (after a reconnect).
/// PINs are digits and tokens URL-safe base64 (brainsolpi auth.py), so no escaping is needed.
pub fn auth_json(pin: Option<&str>, token: Option<&str>, out: &mut impl Write) -> fmt::Result {
    match (token, pin) {
        (Some(t), _) => write!(out, r#"{{"t":"auth","token":"{t}"}}"#),
        (None, Some(p)) => write!(out, r#"{{"t":"auth","pin":"{p}"}}"#),
        (None, None) => Err(fmt::Error),
    }
}
