//! Parsing the brainsolpi history endpoint.
//!
//! `GET /api/history?span=N` answers with (brainsolpi/brainsolpi/server.py:93, history.py:13):
//! `{"t0":..,"dt":1,"cols":["pan","tilt","lvl","lux","pmw","off","gain"],"rows":[[21.4,42.4,0.969,956.8,15.19,2.6,1.482],...]}`
//! The last row is the newest second. Missing values come as `null` (or `NaN`).
//!
//! Meaning (brainsolpi/brainsolpi/engine.py:586-604): pan/tilt = panel angles in degrees,
//! lvl = light level seen by the tracker's sensors (0..1), lux = BH1750 light sensor,
//! pmw = estimated panel power in mW.

/// One sample from the tracker. `None` where the tracker has no value (e.g. no lux sensor).
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub pan: Option<f32>,
    pub tilt: Option<f32>,
    pub lvl: Option<f32>,
    pub lux: Option<f32>,
    pub pmw: Option<f32>,
}

/// Returns `Ok(None)` when the tracker has no rows yet (just started).
pub fn parse_history(body: &str) -> Result<Option<Sample>, ()> {
    let cols = section(body, "\"cols\"").ok_or(())?;
    let rows = section_until_end(body, "\"rows\"").ok_or(())?;

    // rows looks like `[a,b,..],[a,b,..]]}`; an empty history is `]}`.
    let rows = rows.trim_start();
    if rows.starts_with(']') {
        return Ok(None);
    }
    let start = rows.rfind('[').ok_or(())?;
    let row = &rows[start + 1..];
    let row = &row[..row.find(']').ok_or(())?];

    let mut s = Sample::default();
    for (name, value) in cols.split(',').zip(row.split(',')) {
        let name = name.trim().trim_matches('"');
        let value = value.trim().parse::<f32>().ok().filter(|v| v.is_finite());
        match name {
            "pan" => s.pan = value,
            "tilt" => s.tilt = value,
            "lvl" => s.lvl = value,
            "lux" => s.lux = value,
            "pmw" => s.pmw = value,
            _ => {}
        }
    }
    Ok(Some(s))
}

/// Contents of the array after `"key"`, up to the next `]`.
fn section<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = section_until_end(body, key)?;
    Some(&rest[..rest.find(']')?])
}

/// Everything after `"key" : [`, whitespace around `:` and `[` allowed.
fn section_until_end<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let i = body.find(key)?;
    let rest = body[i + key.len()..].trim_start().strip_prefix(':')?;
    rest.trim_start().strip_prefix('[')
}
