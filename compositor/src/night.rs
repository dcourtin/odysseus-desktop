//! Lumière de nuit : l'écran tire vers l'orangé le soir, par la table de
//! couleurs (gamma) du CRTC — aucun programme supplémentaire, aucun coût de rendu.
//!
//! Trois modes : désactivé, activé, automatique (horaires et température des
//! Réglages, ou `ODYSSEUS_NIGHT_FROM` / `_TO` / `_TEMP` ; 21:00 → 07:00 et
//! 3400 K par défaut). Le changement est progressif.
//! Le mode est gardé dans ~/.local/state/odysseus/nightlight (lu par le centre
//! de contrôle) ; il se change par IPC : `nightlight on|off|auto|toggle`.

use chrono::{Local, Timelike};

/// Température « neutre » (aucune correction)
pub const NEUTRAL: f32 = 6500.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    On,
    Auto,
}

pub struct NightLight {
    pub mode: Mode,
    /// Température affichée, qui rejoint la cible pas à pas
    pub current: f32,
    /// Dernière température appliquée au matériel (None : à appliquer)
    pub applied: Option<f32>,
    temperature: f32,
    from_min: u32,
    to_min: u32,
}

fn state_file() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".local/state/odysseus/nightlight"))
}

/// « 21:30 » → minutes depuis minuit
fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.trim().parse::<u32>().ok()? % 24 * 60 + m.trim().parse::<u32>().ok()? % 60)
}

impl NightLight {
    pub fn load(settings: &common::settings::Settings) -> Self {
        let mode = match state_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .as_deref()
            .map(str::trim)
        {
            Some("on") => Mode::On,
            Some("auto") => Mode::Auto,
            _ => Mode::Off,
        };
        let mut night = Self {
            mode,
            current: NEUTRAL,
            applied: None,
            temperature: 3400.0,
            from_min: 21 * 60,
            to_min: 7 * 60,
        };
        night.configure(settings);
        night.current = night.target();
        night
    }

    /// Horaires et température : réglages, sinon variables d'environnement,
    /// sinon 21:00 → 07:00 et 3400 K
    pub fn configure(&mut self, settings: &common::settings::Settings) {
        let env = |k: &str| std::env::var(k).ok();
        self.temperature = settings
            .night_temp
            .map(|t| t as f32)
            .or_else(|| env("ODYSSEUS_NIGHT_TEMP").and_then(|v| v.parse().ok()))
            .unwrap_or(3400.0)
            .clamp(1500.0, NEUTRAL);
        let hm = |set: &Option<String>, var: &str, default: u32| {
            set.clone()
                .or_else(|| env(var))
                .and_then(|v| parse_hm(&v))
                .unwrap_or(default)
        };
        self.from_min = hm(&settings.night_from, "ODYSSEUS_NIGHT_FROM", 21 * 60);
        self.to_min = hm(&settings.night_to, "ODYSSEUS_NIGHT_TO", 7 * 60);
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if let Some(path) = state_file() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let text = match mode {
                Mode::Off => "off",
                Mode::On => "on",
                Mode::Auto => "auto",
            };
            let _ = std::fs::write(path, format!("{text}\n"));
        }
    }

    /// Le soir, entre les deux heures (la plage peut passer minuit)
    fn is_night(&self) -> bool {
        let now = Local::now();
        let m = now.hour() * 60 + now.minute();
        if self.from_min <= self.to_min {
            m >= self.from_min && m < self.to_min
        } else {
            m >= self.from_min || m < self.to_min
        }
    }

    pub fn target(&self) -> f32 {
        match self.mode {
            Mode::Off => NEUTRAL,
            Mode::On => self.temperature,
            Mode::Auto if self.is_night() => self.temperature,
            Mode::Auto => NEUTRAL,
        }
    }

    /// Avance d'une image vers la cible (environ 1,5 s pour un changement complet) ;
    /// renvoie la température à appliquer si elle a assez changé.
    pub fn step(&mut self) -> Option<f32> {
        let target = self.target();
        self.current += (target - self.current).clamp(-50.0, 50.0);
        let due = match self.applied {
            None => true,
            Some(t) => (t - self.current).abs() >= 10.0 || (self.current == target && t != target),
        };
        due.then_some(self.current)
    }
}

/// Facteurs rouge, vert, bleu d'un blanc à `kelvin` (approximation de Tanner
/// Helland), rapportés au blanc neutre : 6500 K donne (1, 1, 1).
pub fn whitepoint(kelvin: f32) -> [f32; 3] {
    fn raw(kelvin: f32) -> [f32; 3] {
        let t = kelvin / 100.0;
        let r = if t <= 66.0 {
            1.0
        } else {
            329.698_73 * (t - 60.0).powf(-0.133_204_76) / 255.0
        };
        let g = if t <= 66.0 {
            (99.470_8 * t.ln() - 161.119_57) / 255.0
        } else {
            288.122_17 * (t - 60.0).powf(-0.075_514_85) / 255.0
        };
        let b = if t >= 66.0 {
            1.0
        } else if t <= 19.0 {
            0.0
        } else {
            (138.517_73 * (t - 10.0).ln() - 305.044_8) / 255.0
        };
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }
    let w = raw(kelvin);
    let n = raw(NEUTRAL);
    [
        (w[0] / n[0]).min(1.0),
        (w[1] / n[1]).min(1.0),
        (w[2] / n[2]).min(1.0),
    ]
}

/// Tables gamma (rouge, vert, bleu) de `size` entrées pour une température.
pub fn ramps(kelvin: f32, size: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let [r, g, b] = whitepoint(kelvin);
    let ramp = |k: f32| -> Vec<u16> {
        (0..size)
            .map(|i| {
                let v = i as f32 / (size.max(2) - 1) as f32;
                (v * k * 65535.0).round() as u16
            })
            .collect()
    };
    (ramp(r), ramp(g), ramp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutre_et_chaud() {
        let [r, g, b] = whitepoint(NEUTRAL);
        assert!((r - 1.0).abs() < 1e-3 && (g - 1.0).abs() < 1e-3 && (b - 1.0).abs() < 1e-3);
        let [r, g, b] = whitepoint(3400.0);
        assert!(r > 0.99 && g < 0.9 && b < g);
        assert_eq!(parse_hm("21:30"), Some(21 * 60 + 30));
    }
}
