//! Parsing brainsolpi's WebSocket frames (`/ws`). Every frame is one JSON object whose first key
//! is `"t"`, the frame type (brainsolpi/brainsolpi/server.py:213, engine.py:586-646):
//!
//! - `{"t":"s","ts":..,"mode":"auto","st":..,"pan":21.4,"tilt":42.4,...,"lvl":0.97,"lux":956.8,
//!    "pmw":15.19,...,"tgt":{"pan":..},"sun":{..,"t":..},"sim":{..,"lux":..}}` — state, 20 Hz.
//!   Top-level keys come before nested ones with the same name (`tgt.pan`, `sim.lux`, `sun.t`),
//!   so taking the first `"key":` in the text is correct.
//! - `{"t":"auth","ok":true,"token":"..."}` / `{"t":"auth","ok":false,"err":"pin","msg":"..","wait":30}`
//! - `{"t":"ack","id":3,"ok":true,"data":{..}}` / `{"t":"ack","id":3,"ok":false,"err":"..","msg":".."}`
//! - `hello`, `sys`, `scan`, `evt`, ... — not needed, reported as `Other`.
//!
//! Field meaning (engine.py:586-604): pan/tilt = panel angles in degrees, lvl = light level seen
//! by the tracker's sensors (0..1), lux = BH1750 light sensor, pmw = estimated panel power in mW.

/// Tracker modes (brainsolpi/brainsolpi/modes.py:262).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
    Sun,
    Hold,
    Park,
    Demo,
    Plan,
    Unknown,
}

impl Mode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "auto" => Mode::Auto,
            "manual" => Mode::Manual,
            "sun" => Mode::Sun,
            "hold" => Mode::Hold,
            "park" => Mode::Park,
            "demo" => Mode::Demo,
            "plan" => Mode::Plan,
            _ => Mode::Unknown,
        }
    }
    /// Name as brainsolpi expects it in `{"op":"mode","mode":..}`.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Manual => "manual",
            Mode::Sun => "sun",
            Mode::Hold => "hold",
            Mode::Park => "park",
            Mode::Demo => "demo",
            Mode::Plan => "plan",
            Mode::Unknown => "?",
        }
    }
    /// Short upper-case label for the LCD (max 4 characters).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Auto => "AUTO",
            Mode::Manual => "MAN",
            Mode::Sun => "SUN",
            Mode::Hold => "HOLD",
            Mode::Park => "PARK",
            Mode::Demo => "DEMO",
            Mode::Plan => "PLAN",
            Mode::Unknown => "?",
        }
    }
}

/// One state sample. `None` where the tracker has no value (e.g. no lux sensor).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub mode: Option<Mode>,
    pub pan: Option<f32>,
    pub tilt: Option<f32>,
    pub lvl: Option<f32>,
    pub lux: Option<f32>,
    pub pmw: Option<f32>,
}

#[derive(Debug, PartialEq)]
pub enum Frame<'a> {
    State(Sample),
    Auth { ok: bool, token: Option<&'a str>, msg: Option<&'a str>, wait: f32 },
    Ack { id: Option<u32>, ok: bool, msg: Option<&'a str> },
    Other,
}

pub fn parse_frame(text: &str) -> Frame<'_> {
    match str_field(text, "t") {
        Some("s") => Frame::State(Sample {
            mode: str_field(text, "mode").map(Mode::from_str),
            pan: num_field(text, "pan"),
            tilt: num_field(text, "tilt"),
            lvl: num_field(text, "lvl"),
            lux: num_field(text, "lux"),
            pmw: num_field(text, "pmw"),
        }),
        Some("auth") => Frame::Auth {
            ok: bool_field(text, "ok").unwrap_or(false),
            token: str_field(text, "token"),
            msg: str_field(text, "msg"),
            wait: num_field(text, "wait").unwrap_or(0.0),
        },
        Some("ack") => Frame::Ack {
            id: num_field(text, "id").map(|v| v as u32),
            ok: bool_field(text, "ok").unwrap_or(false),
            msg: str_field(text, "msg"),
        },
        _ => Frame::Other,
    }
}

/// Text right after the first `"key":` (whitespace around `:` allowed).
fn value_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text;
    loop {
        let i = rest.find('"')?;
        let after = &rest[i + 1..];
        if let Some(tail) = after.strip_prefix(key).and_then(|t| t.strip_prefix('"')) {
            if let Some(v) = tail.trim_start().strip_prefix(':') {
                return Some(v.trim_start());
            }
        }
        rest = after;
    }
}

/// A number, or `None` for `null`, `NaN`, missing key or anything else.
pub fn num_field(text: &str, key: &str) -> Option<f32> {
    let v = value_after(text, key)?;
    let end = v
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
        .unwrap_or(v.len());
    v[..end].parse::<f32>().ok().filter(|x| x.is_finite())
}

/// A string value without unescaping (brainsolpi's values are plain ASCII).
pub fn str_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let v = value_after(text, key)?.strip_prefix('"')?;
    let bytes = v.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(&v[..i]),
            _ => i += 1,
        }
    }
    None
}

pub fn bool_field(text: &str, key: &str) -> Option<bool> {
    let v = value_after(text, key)?;
    if v.starts_with("true") {
        Some(true)
    } else if v.starts_with("false") {
        Some(false)
    } else {
        None
    }
}
