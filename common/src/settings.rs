//! Réglages du bureau, choisis dans l'application Réglages et lus par le
//! compositeur (au démarrage, puis à chaque modification du fichier).
//! Fichier `~/.config/odysseus/reglages`, une ligne « clé = valeur » ; une clé
//! absente garde la valeur par défaut (ou la variable d'environnement d'origine).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Échelle de l'écran (1, 1,25, 1,5…)
    pub scale: Option<f64>,
    /// Mode de l'écran, « 3840x2160@60 »
    pub mode: Option<String>,
    /// Extinction de l'écran après tant de minutes sans activité (0 : jamais)
    pub screen_off_min: u32,
    /// Verrouiller la session quand l'écran s'éteint
    pub lock_on_idle: bool,
    /// Mise en veille du système après tant de minutes sans activité (0 : jamais)
    pub suspend_min: u32,
    /// Lumière de nuit : début et fin (« 21:00 »), température en kelvins
    pub night_from: Option<String>,
    pub night_to: Option<String>,
    pub night_temp: Option<u32>,
    /// Fond d'écran : chemin d'une image, « uni » pour aucun ; absent : l'ancien
    /// fichier wallpaper.jpg / .png du dossier de configuration
    pub wallpaper: Option<String>,
    /// Disposition du clavier : « auto » (Mac si un clavier Apple est branché),
    /// ou « disposition[:variante] », ex. « fr », « fr:mac », « us »
    pub keyboard: String,
    /// Répétition des touches maintenues : délai avant la première (ms), puis
    /// répétitions par seconde
    pub repeat_delay: u32,
    pub repeat_rate: u32,
    /// Dock : masquage (« intelligent » : quand une fenêtre passe dessous,
    /// « toujours », « jamais ») et taille des icônes (pixels logiques)
    pub dock_hide: String,
    pub dock_size: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scale: None,
            mode: None,
            screen_off_min: 10,
            lock_on_idle: true,
            suspend_min: 0,
            night_from: None,
            night_to: None,
            night_temp: None,
            wallpaper: None,
            keyboard: "auto".into(),
            repeat_delay: 300,
            repeat_rate: 25,
            dock_hide: "intelligent".into(),
            dock_size: 48,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    crate::theme::config_path().map(|p| p.with_file_name("reglages"))
}

impl Settings {
    pub fn load() -> Self {
        let text = path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let num = || value.parse::<u32>().ok();
            match key.trim() {
                "echelle" => s.scale = value.replace(',', ".").parse().ok(),
                "mode" => s.mode = Some(value.to_string()).filter(|v| !v.is_empty()),
                "extinction_min" => s.screen_off_min = num().unwrap_or(s.screen_off_min),
                "verrouillage" => s.lock_on_idle = value != "non",
                "veille_min" => s.suspend_min = num().unwrap_or(0),
                "nuit_debut" => s.night_from = Some(value.to_string()),
                "nuit_fin" => s.night_to = Some(value.to_string()),
                "nuit_temperature" => s.night_temp = num(),
                "fond" => s.wallpaper = Some(value.to_string()),
                "clavier" if !value.is_empty() => s.keyboard = value.to_string(),
                "repetition_delai" => s.repeat_delay = num().unwrap_or(300).clamp(150, 1500),
                "repetition_vitesse" => s.repeat_rate = num().unwrap_or(25).clamp(5, 60),
                "dock_masquage" if ["intelligent", "toujours", "jamais"].contains(&value) => {
                    s.dock_hide = value.to_string()
                }
                "dock_taille" => s.dock_size = num().unwrap_or(48).clamp(32, 72),
                // Les clés agent_* ont rejoint agent.json (common::agent)
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("# Réglages d'odysseus-desktop (application Réglages)\n");
        let mut line = |k: &str, v: String| out.push_str(&format!("{k} = {v}\n"));
        if let Some(scale) = self.scale {
            line("echelle", format!("{scale}"));
        }
        if let Some(mode) = &self.mode {
            line("mode", mode.clone());
        }
        line("extinction_min", self.screen_off_min.to_string());
        line(
            "verrouillage",
            if self.lock_on_idle { "oui" } else { "non" }.into(),
        );
        line("veille_min", self.suspend_min.to_string());
        if let Some(v) = &self.night_from {
            line("nuit_debut", v.clone());
        }
        if let Some(v) = &self.night_to {
            line("nuit_fin", v.clone());
        }
        if let Some(v) = self.night_temp {
            line("nuit_temperature", v.to_string());
        }
        if let Some(v) = &self.wallpaper {
            line("fond", v.clone());
        }
        line("clavier", self.keyboard.clone());
        line("repetition_delai", self.repeat_delay.to_string());
        line("repetition_vitesse", self.repeat_rate.to_string());
        line("dock_masquage", self.dock_hide.clone());
        line("dock_taille", self.dock_size.to_string());
        out
    }

    /// Enregistre (écriture puis renommage : un lecteur ne voit jamais un
    /// fichier à moitié écrit)
    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("HOME inconnu"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(tmp, path)
    }

    /// Disposition et variante XKB ; `apple` : un clavier Apple est branché
    pub fn keymap(&self, apple: bool) -> (String, String) {
        match self.keyboard.as_str() {
            "auto" | "" => {
                let layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "fr".into());
                let variant = if apple {
                    "mac".to_string()
                } else {
                    std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default()
                };
                (layout, variant)
            }
            fixed => match fixed.split_once(':') {
                Some((l, v)) => (l.to_string(), v.to_string()),
                None => (fixed.to_string(), String::new()),
            },
        }
    }
}

/// « 21:30 » → minutes depuis minuit
pub fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.trim().parse::<u32>().ok()? % 24 * 60 + m.trim().parse::<u32>().ok()? % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            scale: Some(1.25),
            mode: Some("2560x1440@60".into()),
            screen_off_min: 5,
            lock_on_idle: false,
            suspend_min: 60,
            night_from: Some("22:00".into()),
            night_to: Some("06:30".into()),
            night_temp: Some(3000),
            wallpaper: Some("uni".into()),
            keyboard: "fr:mac".into(),
            repeat_delay: 450,
            repeat_rate: 30,
            dock_hide: "toujours".into(),
            dock_size: 56,
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(s.keymap(false), ("fr".into(), "mac".into()));
    }
}
