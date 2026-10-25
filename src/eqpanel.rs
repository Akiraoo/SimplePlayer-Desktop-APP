//! The equalizer window (title bar → 等化器): presets, the response curve, preamp and bands.
//! The edit is heard while a slider moves and saved to eq.json when it is let go.

use std::rc::Rc;

use slint::{Model, ModelRc, SharedString, VecModel};

use crate::eq::{self, Band, Kind, Preset};
use crate::{post, with_app, App, EqBandRow, EqDot, MainWindow};

// frequency and Q sliders are logarithmic: position 0..1
fn pos_to_freq(p: f32) -> f32 {
    eq::MIN_FREQ * (eq::MAX_FREQ / eq::MIN_FREQ).powf(p.clamp(0.0, 1.0))
}
fn freq_to_pos(f: f32) -> f32 {
    ((f / eq::MIN_FREQ).ln() / (eq::MAX_FREQ / eq::MIN_FREQ).ln()).clamp(0.0, 1.0)
}
fn pos_to_q(p: f32) -> f32 {
    eq::MIN_Q * (eq::MAX_Q / eq::MIN_Q).powf(p.clamp(0.0, 1.0))
}
fn q_to_pos(q: f32) -> f32 {
    ((q / eq::MIN_Q).ln() / (eq::MAX_Q / eq::MIN_Q).ln()).clamp(0.0, 1.0)
}
fn pos_to_gain(p: f32) -> f32 {
    let g = eq::MIN_GAIN + (eq::MAX_GAIN - eq::MIN_GAIN) * p.clamp(0.0, 1.0);
    (g * 10.0).round() / 10.0
}
fn gain_to_pos(g: f32) -> f32 {
    ((g - eq::MIN_GAIN) / (eq::MAX_GAIN - eq::MIN_GAIN)).clamp(0.0, 1.0)
}
fn pos_to_preamp(p: f32) -> f32 {
    let v = eq::MIN_PREAMP + (eq::MAX_PREAMP - eq::MIN_PREAMP) * p.clamp(0.0, 1.0);
    (v * 2.0).round() / 2.0
}
fn preamp_to_pos(v: f32) -> f32 {
    ((v - eq::MIN_PREAMP) / (eq::MAX_PREAMP - eq::MIN_PREAMP)).clamp(0.0, 1.0)
}

fn round_freq(f: f32) -> f32 {
    if f < 100.0 {
        f.round()
    } else if f < 1000.0 {
        (f / 5.0).round() * 5.0
    } else {
        (f / 50.0).round() * 50.0
    }
}

fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 {
        let s = format!("{:.2}", f / 1000.0);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        format!("{s} kHz")
    } else {
        format!("{} Hz", f.round() as i32)
    }
}

fn fmt_db(v: f32) -> String {
    format!("{v:+.1} dB")
}

fn band_row(b: &Band) -> EqBandRow {
    EqBandRow {
        kind: b.kind.ui_index(),
        freq: freq_to_pos(b.freq),
        gain: gain_to_pos(b.gain),
        q: q_to_pos(b.q),
        freq_text: fmt_freq(b.freq).into(),
        gain_text: fmt_db(b.gain).into(),
        q_text: format!("{:.2}", b.q).into(),
    }
}

/// The curve as path commands in a 1000 × 300 box (±15 dB), and the band markers.
fn curve(p: &Preset) -> (String, Vec<EqDot>) {
    const RANGE: f64 = 15.0;
    let y = |db: f64| 150.0 - db.clamp(-RANGE, RANGE) / RANGE * 150.0;
    let n = 200;
    let mut s = String::with_capacity(n * 14);
    for i in 0..=n {
        let pos = i as f64 / n as f64;
        let f = 20.0 * 1000f64.powf(pos);
        let cmd = if i == 0 { 'M' } else { 'L' };
        s.push_str(&format!("{cmd} {:.1} {:.1} ", pos * 1000.0, y(eq::response(p, f))));
    }
    let dots = p
        .bands
        .iter()
        .map(|b| EqDot {
            x: freq_to_pos(b.freq),
            y: (y(eq::response(p, b.freq as f64)) / 300.0) as f32,
        })
        .collect();
    (s, dots)
}

impl App {
    /// Shows the store and the edit in the equalizer window.
    pub(crate) fn push_eq(&mut self) {
        let Some(ui) = self.ui() else { return };
        let names: Vec<SharedString> = self
            .eq
            .presets
            .iter()
            .map(|p| SharedString::from(p.name.as_str()))
            .collect();
        ui.set_eq_enabled(self.eq.enabled);
        ui.set_eq_presets(ModelRc::new(VecModel::from(names)));
        ui.set_eq_current(self.eq.current as i32);
        ui.set_eq_max_bands(eq::MAX_BANDS as i32);
        self.push_eq_edit();
    }

    /// Only the edited preset (cheap enough to run on every slider move).
    fn push_eq_edit(&mut self) {
        let Some(ui) = self.ui() else { return };
        let p = &self.eq_edit;
        ui.set_eq_preamp(preamp_to_pos(p.preamp));
        ui.set_eq_preamp_text(fmt_db(p.preamp).into());
        let (path, dots) = curve(p);
        ui.set_eq_curve(path.into());
        ui.set_eq_dots(ModelRc::new(VecModel::from(dots)));
        // update the rows in place: replacing the model would rebuild the cards and
        // interrupt a slider that is being dragged
        let rows: Vec<EqBandRow> = p.bands.iter().map(band_row).collect();
        match &self.eq_model {
            Some(m) if m.row_count() == rows.len() => {
                for (i, r) in rows.into_iter().enumerate() {
                    m.set_row_data(i, r);
                }
            }
            _ => {
                let m = Rc::new(VecModel::from(rows));
                ui.set_eq_bands(ModelRc::from(m.clone()));
                self.eq_model = Some(m);
            }
        }
    }

    fn eq_message(&self, text: String) {
        if let Some(ui) = self.ui() {
            ui.set_eq_message(text.into());
        }
    }

    /// Hear the edit right away (nothing saved).
    fn eq_live(&mut self) {
        eq::apply(self.eq.enabled, &self.eq_edit);
        self.push_eq_edit();
    }

    /// Save the edit as the current preset.
    fn eq_commit(&mut self) {
        let mut p = self.eq_edit.clone().clean();
        p.name = self.eq.current().name.clone();
        let i = self.eq.current;
        self.eq.presets[i] = p.clone();
        self.eq_edit = p;
        self.eq.save();
        eq::apply(self.eq.enabled, &self.eq_edit);
        self.push_eq_edit();
    }

    fn eq_select(&mut self, i: usize) {
        if i >= self.eq.presets.len() {
            return;
        }
        self.eq.current = i;
        self.eq_edit = self.eq.current().clone();
        self.eq.save();
        eq::apply(self.eq.enabled, &self.eq_edit);
        self.push_eq();
    }

    fn eq_set_band(&mut self, i: usize, what: i32, pos: f32) -> bool {
        let Some(b) = self.eq_edit.bands.get_mut(i) else {
            return false;
        };
        match what {
            0 => b.freq = round_freq(pos_to_freq(pos)),
            1 => b.gain = pos_to_gain(pos),
            _ => b.q = (pos_to_q(pos) * 100.0).round() / 100.0,
        }
        true
    }

    fn eq_import(&mut self, text: &str, fallback: &str) {
        match eq::import(text, fallback) {
            Ok(list) => {
                let n = list.len();
                let first = self.eq.presets.len();
                for mut p in list {
                    p.name = self.eq.unique_name(&p.name, None);
                    self.eq.presets.push(p);
                }
                self.eq_select(first);
                self.eq_message(crate::tr!("匯入了 {} 組設定", "Imported {} preset(s)", n));
            }
            Err(e) => self.eq_message(crate::tr!("匯入失敗：{}", "Import failed: {}", e)),
        }
    }
}

/// Connects the equalizer window's callbacks.
pub(crate) fn wire(ui: &MainWindow) {
    ui.on_eq_opened(|| {
        post(|a| {
            a.eq_message(String::new());
            a.push_eq();
        });
    });
    ui.on_eq_toggled(|on| {
        with_app(|a| {
            a.eq.enabled = on;
            a.eq.save();
            eq::apply(on, &a.eq_edit);
            if let Some(ui) = a.ui() {
                ui.set_eq_enabled(on);
            }
        });
    });
    ui.on_eq_select(|i| {
        with_app(|a| {
            a.eq_message(String::new());
            a.eq_select(i.max(0) as usize);
        });
    });
    ui.on_eq_name_done(|mode, name| {
        with_app(|a| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            if mode == 0 {
                let name = a.eq.unique_name(&name, None);
                a.eq.presets.push(Preset::starter(name).clean());
                let last = a.eq.presets.len() - 1;
                a.eq_select(last);
            } else {
                let i = a.eq.current;
                let name = a.eq.unique_name(&name, Some(i));
                let p = Preset { name, ..a.eq.presets[i].clone() }.clean();
                a.eq.presets[i].name = p.name.clone();
                a.eq_edit.name = p.name;
                a.eq.save();
                a.push_eq();
            }
            if let Some(ui) = a.ui() {
                ui.set_eq_dialog(-1);
            }
        });
    });
    ui.on_eq_delete(|| {
        with_app(|a| {
            let i = a.eq.current;
            if i < a.eq.presets.len() {
                a.eq.presets.remove(i);
            }
            if a.eq.presets.is_empty() {
                a.eq.presets.push(Preset::starter(crate::tr!("預設", "Default")));
            }
            a.eq_select(i.min(a.eq.presets.len() - 1));
            if let Some(ui) = a.ui() {
                ui.set_eq_dialog(-1);
            }
        });
    });
    ui.on_eq_import(|| {
        // the file dialog runs its own message loop: keep the app state unborrowed meanwhile
        let picked = rfd::FileDialog::new()
            .set_title(crate::tr!("匯入等化器設定", "Import equalizer presets"))
            .add_filter(
                crate::tr!("等化器設定（Poweramp .json、AutoEq .txt）", "Equalizer presets (Poweramp .json, AutoEq .txt)"),
                &["json", "txt"],
            )
            .add_filter(crate::tr!("所有檔案", "All files"), &["*"])
            .pick_file();
        let Some(path) = picked else { return };
        let fallback = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| crate::tr!("匯入的設定", "Imported preset"));
        match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                with_app(|a| a.eq_import(&text, &fallback));
            }
            Err(e) => {
                with_app(|a| a.eq_message(crate::tr!("讀不到檔案：{}", "Can't read the file: {}", e)));
            }
        }
    });
    ui.on_eq_export(|| {
        let picked = rfd::FileDialog::new()
            .set_title(crate::tr!("匯出等化器設定", "Export equalizer presets"))
            .set_file_name("SimplePlayer-EQ.json")
            .add_filter("Poweramp JSON", &["json"])
            .save_file();
        let Some(path) = picked else { return };
        with_app(|a| {
            let text = eq::export(&a.eq.presets);
            let n = a.eq.presets.len();
            match std::fs::write(&path, text) {
                Ok(()) => a.eq_message(crate::tr!(
                    "已匯出 {} 組設定（Poweramp 格式）",
                    "Exported {} preset(s) (Poweramp format)",
                    n
                )),
                Err(e) => a.eq_message(crate::tr!("匯出失敗：{}", "Export failed: {}", e)),
            }
        });
    });
    ui.on_eq_preamp_moving(|pos| {
        with_app(|a| {
            a.eq_edit.preamp = pos_to_preamp(pos);
            a.eq_live();
        });
    });
    ui.on_eq_preamp_committed(|pos| {
        with_app(|a| {
            a.eq_edit.preamp = pos_to_preamp(pos);
            a.eq_commit();
        });
    });
    ui.on_eq_auto_preamp(|| {
        with_app(|a| {
            a.eq_edit.preamp = eq::auto_preamp(&a.eq_edit);
            a.eq_commit();
        });
    });
    ui.on_eq_band_kind(|i, k| {
        with_app(|a| {
            if let Some(b) = a.eq_edit.bands.get_mut(i.max(0) as usize) {
                b.kind = Kind::from_ui(k);
                a.eq_commit();
            }
        });
    });
    ui.on_eq_band_moving(|i, what, pos| {
        with_app(|a| {
            if a.eq_set_band(i.max(0) as usize, what, pos) {
                a.eq_live();
            }
        });
    });
    ui.on_eq_band_committed(|i, what, pos| {
        with_app(|a| {
            if a.eq_set_band(i.max(0) as usize, what, pos) {
                a.eq_commit();
            }
        });
    });
    ui.on_eq_band_delete(|i| {
        with_app(|a| {
            let i = i.max(0) as usize;
            if i < a.eq_edit.bands.len() {
                a.eq_edit.bands.remove(i);
                a.eq_commit();
            }
        });
    });
    ui.on_eq_band_add(|| {
        with_app(|a| {
            if a.eq_edit.bands.len() < eq::MAX_BANDS {
                a.eq_edit.bands.push(Band {
                    kind: Kind::Peak,
                    freq: 1000.0,
                    gain: 0.0,
                    q: 1.0,
                });
                a.eq_commit();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_mapping() {
        assert_eq!(round_freq(pos_to_freq(freq_to_pos(1000.0))), 1000.0);
        assert!((pos_to_q(q_to_pos(0.7)) - 0.7).abs() < 1e-4);
        assert_eq!(pos_to_gain(gain_to_pos(-4.0)), -4.0);
        assert_eq!(pos_to_preamp(preamp_to_pos(-4.5)), -4.5);
        assert_eq!(fmt_freq(1250.0), "1.25 kHz");
        assert_eq!(fmt_freq(10_000.0), "10 kHz");
        assert_eq!(fmt_freq(105.0), "105 Hz");
        assert_eq!(fmt_db(-3.0), "-3.0 dB");
        let (path, dots) = curve(&Preset::starter("x".into()));
        assert!(path.starts_with("M 0.0 150.0 "));
        assert_eq!(dots.len(), 3);
    }
}
