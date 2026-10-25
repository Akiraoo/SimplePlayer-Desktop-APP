//! Parametric equalizer: the presets, the filter maths (RBJ "Audio EQ Cookbook" biquads),
//! preset import / export, and the processor the audio output runs the samples through.
//!
//! Same model and file formats as the Android app: presets are kept, imported and exported
//! in Poweramp's JSON format (AutoEq / Equalizer APO `ParametricEQ.txt` can be imported too),
//! so a preset made in Poweramp or on the phone works here unchanged.
//!
//! Settings live in `eq.json` next to `config.json`. The output thread reads them through
//! [`Processor`], which picks up a change on its next buffer. Off or flat = samples pass
//! through untouched (bit-perfect stays bit-perfect).

use std::f64::consts::PI;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

pub const MAX_BANDS: usize = 10;
pub const MIN_FREQ: f32 = 20.0;
pub const MAX_FREQ: f32 = 20_000.0;
pub const MIN_GAIN: f32 = -15.0;
pub const MAX_GAIN: f32 = 15.0;
pub const MIN_Q: f32 = 0.1;
pub const MAX_Q: f32 = 10.0;
pub const MIN_PREAMP: f32 = -20.0;
pub const MAX_PREAMP: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LowShelf,
    Peak,
    HighShelf,
}

impl Kind {
    /// Index used by the UI (and Poweramp's own numbering is 0 / 1 / 2 = low / high / peak).
    pub fn ui_index(self) -> i32 {
        match self {
            Kind::LowShelf => 0,
            Kind::Peak => 1,
            Kind::HighShelf => 2,
        }
    }

    pub fn from_ui(i: i32) -> Kind {
        match i {
            0 => Kind::LowShelf,
            2 => Kind::HighShelf,
            _ => Kind::Peak,
        }
    }

    fn poweramp(self) -> i64 {
        match self {
            Kind::LowShelf => 0,
            Kind::HighShelf => 1,
            Kind::Peak => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub kind: Kind,
    /// Hz
    pub freq: f32,
    /// dB
    pub gain: f32,
    pub q: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    /// dB, applied before the filters (negative = headroom against clipping)
    pub preamp: f32,
    pub bands: Vec<Band>,
}

impl Preset {
    /// True when it would not change the sound at all.
    pub fn is_flat(&self) -> bool {
        self.preamp == 0.0 && self.bands.iter().all(|b| b.gain == 0.0)
    }

    /// Values kept inside the limits, at most [`MAX_BANDS`] bands, a usable name.
    pub fn clean(mut self) -> Preset {
        let name: String = self.name.trim().chars().take(40).collect();
        self.name = if name.is_empty() {
            crate::tr!("未命名", "Untitled")
        } else {
            name
        };
        self.preamp = finite(self.preamp, 0.0).clamp(MIN_PREAMP, MAX_PREAMP);
        self.bands.truncate(MAX_BANDS);
        for b in &mut self.bands {
            b.freq = finite(b.freq, 1000.0).clamp(MIN_FREQ, MAX_FREQ);
            b.gain = finite(b.gain, 0.0).clamp(MIN_GAIN, MAX_GAIN);
            b.q = finite(b.q, 1.0).clamp(MIN_Q, MAX_Q);
        }
        self
    }

    /// A starting point: three bands at 0 dB to drag around.
    pub fn starter(name: String) -> Preset {
        Preset {
            name,
            preamp: 0.0,
            bands: vec![
                Band { kind: Kind::LowShelf, freq: 105.0, gain: 0.0, q: 0.7 },
                Band { kind: Kind::Peak, freq: 1000.0, gain: 0.0, q: 1.0 },
                Band { kind: Kind::HighShelf, freq: 10_000.0, gain: 0.0, q: 0.7 },
            ],
        }
    }
}

fn finite(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/* ---------------- filter maths ---------------- */

/// Normalised biquad coefficients b0, b1, b2, a1, a2 (a0 = 1).
pub fn coefficients(b: &Band, rate: u32) -> [f64; 5] {
    let rate = rate.max(8000) as f64;
    // keep the centre frequency below Nyquist (a 20 kHz shelf at 44.1 kHz is fine,
    // a 20 kHz peak at 32 kHz would not be)
    let f = (b.freq as f64).min(rate * 0.45);
    let a = 10f64.powf(b.gain as f64 / 40.0);
    let w0 = 2.0 * PI * f / rate;
    let cw = w0.cos();
    let alpha = w0.sin() / (2.0 * b.q as f64);
    let (b0, b1, b2, a0, a1, a2) = match b.kind {
        Kind::Peak => (
            1.0 + alpha * a,
            -2.0 * cw,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cw,
            1.0 - alpha / a,
        ),
        Kind::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cw + s),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                a * ((a + 1.0) - (a - 1.0) * cw - s),
                (a + 1.0) + (a - 1.0) * cw + s,
                -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                (a + 1.0) + (a - 1.0) * cw - s,
            )
        }
        Kind::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cw + s),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                a * ((a + 1.0) + (a - 1.0) * cw - s),
                (a + 1.0) - (a - 1.0) * cw + s,
                2.0 * ((a - 1.0) - (a + 1.0) * cw),
                (a + 1.0) - (a - 1.0) * cw - s,
            )
        }
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Gain in dB of the whole preset (preamp included) at `freq` Hz.
pub fn response(p: &Preset, freq: f64) -> f64 {
    const RATE: u32 = 48_000;
    let w = 2.0 * PI * freq / RATE as f64;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let mut db = p.preamp as f64;
    for b in &p.bands {
        let c = coefficients(b, RATE);
        // H(e^jw) = (b0 + b1 e^-jw + b2 e^-2jw) / (1 + a1 e^-jw + a2 e^-2jw)
        let nr = c[0] + c[1] * c1 + c[2] * c2;
        let ni = -(c[1] * s1 + c[2] * s2);
        let dr = 1.0 + c[3] * c1 + c[4] * c2;
        let di = -(c[3] * s1 + c[4] * s2);
        let mag2 = (nr * nr + ni * ni) / (dr * dr + di * di);
        db += 10.0 * mag2.max(1e-12).log10();
    }
    db
}

/// Preamp that keeps the loudest point of the curve at 0 dB (never above 0).
pub fn auto_preamp(p: &Preset) -> f32 {
    let flat = Preset { preamp: 0.0, ..p.clone() };
    let mut peak: f64 = 0.0;
    let mut f = 20.0;
    while f <= 20_000.0 {
        peak = peak.max(response(&flat, f));
        f *= 1.02;
    }
    // round down to 0.5 dB steps
    ((-(peak * 2.0).ceil() / 2.0) as f32).max(MIN_PREAMP)
}

/* ---------------- file formats ---------------- */

fn num(v: &Value, key: &str) -> Option<f32> {
    v.get(key).and_then(|x| {
        x.as_f64()
            .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
            .map(|f| f as f32)
    })
}

/// One preset in Poweramp's format:
/// `{name, preamp, parametric, bands: [{type, channels, frequency, q, gain, color}]}`
/// (type 0 = low shelf, 1 = high shelf, anything else = peak). The Android app's own
/// format (`bands: [{type: "PEAK", freq, gain, q}]`) reads the same way.
fn preset_from_json(o: &Value, fallback_name: &str) -> Option<Preset> {
    let arr = o.get("bands")?.as_array()?;
    let parametric = o.get("parametric").and_then(Value::as_bool).unwrap_or(true);
    let bands: Vec<Band> = arr
        .iter()
        .filter_map(|b| {
            let kind = match b.get("type") {
                Some(Value::String(s)) => match s.to_ascii_uppercase().as_str() {
                    "LOW_SHELF" => Kind::LowShelf,
                    "HIGH_SHELF" => Kind::HighShelf,
                    "PEAK" => Kind::Peak,
                    _ => return None,
                },
                _ if !parametric => Kind::Peak,
                Some(v) => match v.as_i64() {
                    Some(0) => Kind::LowShelf,
                    Some(1) => Kind::HighShelf,
                    _ => Kind::Peak,
                },
                None => Kind::Peak,
            };
            Some(Band {
                kind,
                freq: num(b, "frequency").or_else(|| num(b, "freq")).unwrap_or(1000.0),
                gain: num(b, "gain").unwrap_or(0.0),
                q: num(b, "q").unwrap_or(1.41),
            })
        })
        .collect();
    if bands.is_empty() {
        return None;
    }
    let name = o
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(fallback_name);
    Some(Preset {
        name: name.to_string(),
        preamp: num(o, "preamp").unwrap_or(0.0),
        bands,
    })
}

/// Preset in Poweramp's format (so it can go back into Poweramp or the Android app).
fn preset_to_json(p: &Preset) -> Value {
    json!({
        "name": p.name,
        "preamp": round(p.preamp, 10.0),
        "parametric": true,
        "bands": p.bands.iter().map(|b| json!({
            "type": b.kind.poweramp(),
            "channels": 0,
            "frequency": b.freq.round() as i64,
            "q": round(b.q, 100.0),
            "gain": round(b.gain, 10.0),
            "color": 0,
        })).collect::<Vec<_>>(),
    })
}

/// Rounds to the given step for the file (so 0.1 dB doesn't turn into 0.10000000149).
fn round(v: f32, scale: f64) -> f64 {
    (v as f64 * scale).round() / scale
}

/// Reads presets from a file's text: Poweramp JSON (one preset or an array) or AutoEq /
/// Equalizer APO `ParametricEQ.txt`. `fallback_name` names presets without a name.
pub fn import(text: &str, fallback_name: &str) -> Result<Vec<Preset>, String> {
    let t = text.trim().trim_start_matches('\u{feff}').trim();
    let found = if t.starts_with('[') || t.starts_with('{') {
        let v: Value = serde_json::from_str(t).map_err(|e| e.to_string())?;
        let list = match &v {
            Value::Array(a) => a.clone(),
            // our own eq.json: {enabled, current, presets: [...]}
            Value::Object(o) if o.contains_key("presets") => {
                o["presets"].as_array().cloned().unwrap_or_default()
            }
            _ => vec![v.clone()],
        };
        list.iter()
            .filter_map(|o| preset_from_json(o, fallback_name))
            .collect()
    } else {
        auto_eq(t, fallback_name)
    };
    if found.is_empty() {
        return Err(crate::tr!(
            "檔案裡沒有看得懂的等化器設定",
            "No equalizer settings found in the file"
        ));
    }
    Ok(found.into_iter().map(Preset::clean).collect())
}

/// AutoEq / Equalizer APO "ParametricEQ.txt":
///   Preamp: -6.2 dB
///   Filter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70
///   Filter 2: ON PK Fc 220 Hz Gain -2.1 dB Q 1.20
fn auto_eq(t: &str, fallback_name: &str) -> Vec<Preset> {
    let mut preamp = 0.0;
    let mut bands = Vec::new();
    for line in t.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let lower: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
        if lower.first().map(String::as_str) == Some("preamp:") {
            if let Some(v) = words.get(1).and_then(|w| w.parse::<f32>().ok()) {
                preamp = v;
            }
            continue;
        }
        if !lower.first().is_some_and(|w| w.starts_with("filter")) {
            continue;
        }
        // "Filter 1: ON PK Fc 220 Hz Gain -2.1 dB Q 1.20" / "Filter: ON PK …"
        let Some(on) = lower.iter().position(|w| w == "on" || w == "off") else {
            continue;
        };
        if lower[on] == "off" {
            continue;
        }
        let kind = match lower.get(on + 1).map(String::as_str) {
            Some("pk" | "peq" | "peak") => Kind::Peak,
            Some("ls" | "lsc" | "lsq") => Kind::LowShelf,
            Some("hs" | "hsc" | "hsq") => Kind::HighShelf,
            _ => continue,
        };
        let after = |key: &str| -> Option<f32> {
            let i = lower.iter().position(|w| w == key)?;
            words.get(i + 1)?.parse().ok()
        };
        let (Some(freq), Some(gain)) = (after("fc"), after("gain")) else {
            continue;
        };
        let q = after("q").unwrap_or(if kind == Kind::Peak { 1.41 } else { 0.7 });
        bands.push(Band { kind, freq, gain, q });
    }
    if bands.is_empty() {
        Vec::new()
    } else {
        vec![Preset {
            name: fallback_name.to_string(),
            preamp,
            bands,
        }]
    }
}

/// Export in Poweramp's format.
pub fn export(presets: &[Preset]) -> String {
    let arr: Vec<Value> = presets.iter().map(preset_to_json).collect();
    serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "[]".into())
}

/* ---------------- store (eq.json) ---------------- */

/// Saved presets, which one is in use and the on/off switch.
pub struct Store {
    pub enabled: bool,
    pub presets: Vec<Preset>,
    pub current: usize,
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("eq.json")
}

impl Store {
    pub fn load() -> Store {
        let mut st = Store {
            enabled: false,
            presets: Vec::new(),
            current: 0,
        };
        let parsed = fs::read_to_string(store_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let mut current_name = String::new();
        if let Some(v) = parsed {
            st.enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            current_name = v
                .get("current")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(arr) = v.get("presets").and_then(Value::as_array) {
                st.presets = arr
                    .iter()
                    .filter_map(|o| preset_from_json(o, ""))
                    .map(Preset::clean)
                    .collect();
            }
        }
        if st.presets.is_empty() {
            st.presets.push(Preset::starter(crate::tr!("預設", "Default")));
        }
        st.current = st
            .presets
            .iter()
            .position(|p| p.name == current_name)
            .unwrap_or(0);
        st
    }

    pub fn save(&self) {
        let v = json!({
            "enabled": self.enabled,
            "current": self.current().name,
            "presets": self.presets.iter().map(preset_to_json).collect::<Vec<_>>(),
        });
        let path = store_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&v) {
            let _ = fs::write(path, text);
        }
    }

    pub fn current(&self) -> &Preset {
        &self.presets[self.current.min(self.presets.len() - 1)]
    }

    /// `name`, or "name (2)", "name (3)"… if another preset (not `except`) has it.
    pub fn unique_name(&self, name: &str, except: Option<usize>) -> String {
        let taken = |n: &str| {
            self.presets
                .iter()
                .enumerate()
                .any(|(i, p)| Some(i) != except && p.name == n)
        };
        if !taken(name) {
            return name.to_string();
        }
        let mut n = 2;
        while taken(&format!("{name} ({n})")) {
            n += 1;
        }
        format!("{name} ({n})")
    }
}

/* ---------------- what the audio output hears ---------------- */

/// The settings the audio output uses right now; `VERSION` changes with every update.
static LIVE: Mutex<Option<Arc<(bool, Preset)>>> = Mutex::new(None);
static VERSION: AtomicU64 = AtomicU64::new(1);

/// Loads the saved settings for the audio output (once; later calls do nothing).
pub fn init() {
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    if live.is_none() {
        let st = Store::load();
        *live = Some(Arc::new((st.enabled, st.current().clone())));
        VERSION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Makes the output use these settings (heard within one buffer; nothing is saved).
pub fn apply(enabled: bool, preset: &Preset) {
    if let Ok(mut live) = LIVE.lock() {
        *live = Some(Arc::new((enabled, preset.clone())));
    }
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// Runs the samples of the output through the equalizer (owned by the output side).
pub struct Processor {
    seen: u64,
    rate: u32,
    bypass: bool,
    preamp: f64,
    /// b0 b1 b2 a1 a2 per band
    coeffs: Vec<[f64; 5]>,
    /// z1 z2 per band, left then right
    state: Vec<[f64; 2]>,
}

impl Default for Processor {
    fn default() -> Self {
        Processor {
            seen: 0,
            rate: 0,
            bypass: true,
            preamp: 1.0,
            coeffs: Vec::new(),
            state: Vec::new(),
        }
    }
}

impl Processor {
    /// Picks up new settings / a new sample rate. Returns true when the samples are changed
    /// (false = pass-through).
    pub fn prepare(&mut self, rate: u32) -> bool {
        let version = VERSION.load(Ordering::Relaxed);
        if version == self.seen && rate == self.rate {
            return !self.bypass;
        }
        // the UI thread only holds this for a moment; try again next buffer if it does
        let Ok(live) = LIVE.try_lock() else {
            return !self.bypass;
        };
        let Some(cur) = live.clone() else {
            self.seen = version;
            self.bypass = true;
            return false;
        };
        drop(live);
        let (enabled, preset) = &*cur;
        let was_bypassed = self.bypass;
        let old_bands = self.coeffs.len();
        self.seen = version;
        self.rate = rate;
        self.bypass = !*enabled || preset.is_flat();
        self.preamp = 10f64.powf(preset.preamp as f64 / 20.0);
        self.coeffs = preset.bands.iter().map(|b| coefficients(b, rate)).collect();
        // keep the filter memory while bands are only being tweaked (no click while a
        // slider is dragged); start clean when the layout changes or it was off
        if was_bypassed || old_bands != self.coeffs.len() || self.state.len() != self.coeffs.len() * 2
        {
            self.reset();
        }
        !self.bypass
    }

    /// Clears the filter memory (after a seek or a new song).
    pub fn reset(&mut self) {
        self.state = vec![[0.0; 2]; self.coeffs.len() * 2];
    }

    /// One stereo frame (call only after `prepare` returned true).
    #[inline]
    pub fn frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        let n = self.coeffs.len();
        let mut out = [l as f64 * self.preamp, r as f64 * self.preamp];
        for (ch, x) in out.iter_mut().enumerate() {
            let z = &mut self.state[ch * n..(ch + 1) * n];
            for (c, s) in self.coeffs.iter().zip(z.iter_mut()) {
                // transposed direct form II
                let y = c[0] * *x + s[0];
                s[0] = c[1] * *x - c[3] * y + s[1];
                s[1] = c[2] * *x - c[4] * y;
                *x = y;
            }
        }
        (out[0] as f32, out[1] as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(freq: f32, gain: f32, q: f32) -> Band {
        Band { kind: Kind::Peak, freq, gain, q }
    }

    #[test]
    fn response_matches_gain() {
        let p = Preset {
            name: "t".into(),
            preamp: -3.0,
            bands: vec![peak(1000.0, 6.0, 1.0)],
        };
        assert!((response(&p, 1000.0) - 3.0).abs() < 0.05);
        assert!(response(&p, 20.0).abs() - 3.0 < 0.1);
        assert_eq!(auto_preamp(&Preset { preamp: 0.0, ..p }), -6.0);
    }

    #[test]
    fn shelves() {
        let lo = Preset {
            name: "t".into(),
            preamp: 0.0,
            bands: vec![Band { kind: Kind::LowShelf, freq: 100.0, gain: 6.0, q: 0.7 }],
        };
        assert!((response(&lo, 20.0) - 6.0).abs() < 0.3);
        assert!(response(&lo, 10_000.0).abs() < 0.1);
        let hi = Preset {
            bands: vec![Band { kind: Kind::HighShelf, freq: 8000.0, gain: -4.0, q: 0.7 }],
            ..lo
        };
        assert!((response(&hi, 19_000.0) + 4.0).abs() < 0.4);
        assert!(response(&hi, 100.0).abs() < 0.1);
    }

    #[test]
    fn processor_sine_gain() {
        // +6 dB peak at 1 kHz: a 1 kHz sine comes out twice as loud
        let p = Preset {
            name: "t".into(),
            preamp: 0.0,
            bands: vec![peak(1000.0, 6.0206, 1.0)],
        };
        apply(true, &p);
        let mut pr = Processor::default();
        assert!(pr.prepare(48_000));
        let mut peak_out: f32 = 0.0;
        for i in 0..48_000 {
            let x = (2.0 * PI * 1000.0 * i as f64 / 48_000.0).sin() as f32 * 0.25;
            let (l, r) = pr.frame(x, x);
            assert_eq!(l, r);
            if i > 24_000 {
                peak_out = peak_out.max(l.abs());
            }
        }
        assert!((peak_out - 0.5).abs() < 0.005, "{peak_out}");
        apply(false, &p);
        assert!(!pr.prepare(48_000));
    }

    #[test]
    fn poweramp_roundtrip() {
        let text = r#"[{"name":"TE-V1R","preamp":-4.5,"parametric":true,"bands":[
            {"type":0,"channels":0,"frequency":105,"q":0.7,"gain":2.5,"color":0},
            {"type":2,"channels":0,"frequency":6200,"q":3,"gain":-4,"color":0},
            {"type":1,"channels":0,"frequency":10000,"q":0.7,"gain":-3,"color":0}]}]"#;
        let list = import(text, "x").unwrap();
        assert_eq!(list.len(), 1);
        let p = &list[0];
        assert_eq!(p.name, "TE-V1R");
        assert_eq!(p.preamp, -4.5);
        assert_eq!(p.bands[0].kind, Kind::LowShelf);
        assert_eq!(p.bands[1].kind, Kind::Peak);
        assert_eq!(p.bands[2].kind, Kind::HighShelf);
        let again = import(&export(&list), "x").unwrap();
        assert_eq!(again, list);
    }

    #[test]
    fn android_own_format() {
        let text = r#"{"name":"A","preamp":-2,"bands":[{"type":"HIGH_SHELF","freq":9000,"gain":-3,"q":0.7}]}"#;
        let p = &import(text, "x").unwrap()[0];
        assert_eq!(p.bands[0].kind, Kind::HighShelf);
        assert_eq!(p.bands[0].freq, 9000.0);
    }

    #[test]
    fn autoeq_text() {
        let text = "Preamp: -6.2 dB\r\nFilter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70\r\n\
                    Filter 2: ON PK Fc 220 Hz Gain -2.1 dB Q 1.20\r\nFilter 3: OFF PK Fc 1000 Hz Gain 3 dB Q 1\r\n\
                    Filter 4: ON HSC Fc 10000 Hz Gain -3 dB";
        let p = &import(text, "My IEM").unwrap()[0];
        assert_eq!(p.name, "My IEM");
        assert_eq!(p.preamp, -6.2);
        assert_eq!(p.bands.len(), 3);
        assert_eq!(p.bands[0].kind, Kind::LowShelf);
        assert_eq!(p.bands[1].gain, -2.1);
        assert_eq!(p.bands[2].q, 0.7);
        assert!(import("hello", "x").is_err());
    }
}
