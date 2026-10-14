//! Contenu d'un dossier, tri, historique de navigation.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub struct Browser {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Première ligne affichée
    pub scroll: usize,
    pub show_hidden: bool,
    /// Message d'erreur ou d'information pour la barre d'état
    pub message: Option<String>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
        let mut b = Self {
            cwd: start,
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            show_hidden: false,
            message: None,
            back: Vec::new(),
            forward: Vec::new(),
        };
        b.reload();
        b
    }

    /// Relit le dossier courant ; dossiers d'abord, puis ordre alphabétique sans casse.
    pub fn reload(&mut self) {
        self.message = None;
        self.entries.clear();
        match std::fs::read_dir(&self.cwd) {
            Ok(list) => {
                for item in list.flatten() {
                    let name = item.file_name().to_string_lossy().into_owned();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    let path = item.path();
                    // Les liens symboliques sont suivis (dossier lié = dossier)
                    let meta = std::fs::metadata(&path).or_else(|_| item.metadata());
                    let (is_dir, size, modified) = match meta {
                        Ok(m) => (m.is_dir(), m.len(), m.modified().ok()),
                        Err(_) => (false, 0, None),
                    };
                    self.entries.push(Entry {
                        name,
                        path,
                        is_dir,
                        size,
                        modified,
                    });
                }
            }
            Err(e) => {
                self.message = Some(format!("Impossible de lire {} : {e}", self.cwd.display()))
            }
        }
        self.entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    fn go(&mut self, path: PathBuf) {
        self.cwd = path;
        self.selected = 0;
        self.scroll = 0;
        self.reload();
    }

    pub fn open_dir(&mut self, path: &Path) {
        if path == self.cwd {
            return;
        }
        let previous = std::mem::replace(&mut self.cwd, path.to_path_buf());
        self.back.push(previous);
        self.forward.clear();
        let target = self.cwd.clone();
        self.go(target);
    }

    pub fn parent(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            let child = self.cwd.clone();
            self.open_dir(&parent);
            // Resélectionner le dossier dont on vient
            if let Some(i) = self.entries.iter().position(|e| e.path == child) {
                self.selected = i;
            }
        }
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn back(&mut self) {
        if let Some(path) = self.back.pop() {
            let current = std::mem::replace(&mut self.cwd, path.clone());
            self.forward.push(current);
            self.go(path);
        }
    }

    pub fn forward(&mut self) {
        if let Some(path) = self.forward.pop() {
            let current = std::mem::replace(&mut self.cwd, path.clone());
            self.back.push(current);
            self.go(path);
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload();
    }

    /// Recherche à la frappe : première entrée qui commence par `prefix`.
    pub fn jump_to(&mut self, prefix: &str) {
        let p = prefix.to_lowercase();
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.name.to_lowercase().starts_with(&p))
        {
            self.selected = i;
        }
    }
}

/// Taille lisible, à la française : 12 octets, 3,4 Ko, 1,2 Go.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["octets", "Ko", "Mo", "Go", "To"];
    if bytes < 1000 {
        return format!("{bytes} {}", if bytes == 1 { "octet" } else { "octets" });
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let text = if value < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{} {}", text.replace('.', ","), UNITS[unit])
}

/// Date lisible : « aujourd'hui 14:32 », « hier 09:10 » ou « 12 oct. 2026 ».
pub fn format_date(time: Option<SystemTime>) -> String {
    use chrono::{DateTime, Datelike, Local};
    const MOIS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    let Some(time) = time else {
        return "—".into();
    };
    let date: DateTime<Local> = time.into();
    let today = Local::now().date_naive();
    match (today - date.date_naive()).num_days() {
        0 => format!("aujourd'hui {}", date.format("%H:%M")),
        1 => format!("hier {}", date.format("%H:%M")),
        _ => format!(
            "{} {} {}",
            date.day(),
            MOIS[date.month0() as usize],
            date.year()
        ),
    }
}

/// Fichier qu'on sait ouvrir dans l'éditeur de texte.
pub fn is_text(path: &Path, size: u64) -> bool {
    const TEXT: &[&str] = &[
        "txt", "md", "rs", "toml", "sh", "conf", "cfg", "ini", "log", "json", "yaml", "yml", "c",
        "h", "cpp", "py", "desktop", "service", "lock", "html", "css", "js", "xml", "patch",
        "diff",
    ];
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => TEXT.contains(&ext.to_lowercase().as_str()),
        // Sans extension : petit fichier probablement textuel (README, Makefile…)
        None => size < 512 * 1024,
    }
}
