//! Contenu d'un dossier, tri, historique de navigation.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Recherche dans les sous-dossiers : dossier de l'élément, relatif au
    /// dossier affiché
    pub place: Option<String>,
}

/// Colonne de tri
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Modified,
}

/// Tri de la liste : colonne et sens, mémorisés d'une fois sur l'autre
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
        }
    }
}

impl Sort {
    fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/odysseus/fichiers-tri"))
    }

    /// Réglage enregistré : « nom », « taille » ou « date », suivi de « desc »
    pub fn load() -> Self {
        let text = Self::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .unwrap_or_default();
        let mut words = text.split_whitespace();
        let key = match words.next() {
            Some("taille") => SortKey::Size,
            Some("date") => SortKey::Modified,
            _ => SortKey::Name,
        };
        Self {
            key,
            descending: words.next() == Some("desc"),
        }
    }

    fn save(&self) {
        let Some(file) = Self::file() else {
            return;
        };
        let key = match self.key {
            SortKey::Name => "nom",
            SortKey::Size => "taille",
            SortKey::Modified => "date",
        };
        let dir = if self.descending { " desc" } else { "" };
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(file, format!("{key}{dir}\n"));
    }
}

pub struct Browser {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Sélection multiple (Ctrl+clic, Maj+clic, Ctrl+A) : chemins choisis ;
    /// vide, la sélection se résume à `selected`
    pub picked: HashSet<PathBuf>,
    /// Point de départ d'une sélection par plage (Maj)
    pub anchor: usize,
    /// Première ligne affichée
    pub scroll: usize,
    pub show_hidden: bool,
    pub sort: Sort,
    /// Message d'erreur ou d'information pour la barre d'état
    pub message: Option<String>,
    /// Recherche : seuls les noms qui contiennent ce texte (sans casse ni
    /// accents) ; `deep` : la liste vient d'une recherche dans les
    /// sous-dossiers (search.rs), elle n'est plus relue depuis le dossier
    pub filter: String,
    pub deep: bool,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
        let mut b = Self {
            cwd: start,
            entries: Vec::new(),
            selected: 0,
            picked: HashSet::new(),
            anchor: 0,
            scroll: 0,
            show_hidden: false,
            sort: Sort::load(),
            message: None,
            filter: String::new(),
            deep: false,
            back: Vec::new(),
            forward: Vec::new(),
        };
        b.reload();
        b
    }

    /// Relit le dossier courant ; la sélection reste sur le même élément.
    pub fn reload(&mut self) {
        self.message = None;
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        if self.deep {
            // Résultats d'une recherche : ceux qui ont disparu s'en vont
            self.entries.retain(|e| e.path.symlink_metadata().is_ok());
            self.selected = self.selected.min(self.entries.len().saturating_sub(1));
            self.normalize();
            return;
        }
        self.entries.clear();
        let filter = fold(&self.filter);
        match std::fs::read_dir(&self.cwd) {
            Ok(list) => {
                for item in list.flatten() {
                    let name = item.file_name().to_string_lossy().into_owned();
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    if !filter.is_empty() && !fold(&name).contains(&filter) {
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
                        place: None,
                    });
                }
            }
            Err(e) => {
                self.message = Some(format!("Impossible de lire {} : {e}", self.cwd.display()))
            }
        }
        self.sort_entries();
        if let Some(i) = selected.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
            self.selected = i;
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.normalize();
    }

    /// Dossiers d'abord (comme le Finder), puis la colonne choisie ; à égalité,
    /// ordre alphabétique sans casse. Les dossiers n'ont pas de taille : par nom.
    pub fn sort_entries(&mut self) {
        let sort = self.sort;
        self.entries.sort_by(|a, b| {
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let order = match sort.key {
                SortKey::Size if !a.is_dir => a.size.cmp(&b.size).then_with(by_name),
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
                _ => by_name(),
            };
            let order = if sort.descending {
                order.reverse()
            } else {
                order
            };
            b.is_dir.cmp(&a.is_dir).then(order)
        });
    }

    /// Clic sur un en-tête : trie par cette colonne, ou inverse le sens si elle
    /// l'est déjà. Taille et date commencent par les plus grands / récents.
    pub fn sort_by(&mut self, key: SortKey) {
        self.sort = if self.sort.key == key {
            Sort {
                key,
                descending: !self.sort.descending,
            }
        } else {
            Sort {
                key,
                descending: key != SortKey::Name,
            }
        };
        self.sort.save();
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        self.sort_entries();
        if let Some(i) = selected.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
            self.selected = i;
        }
    }

    fn go(&mut self, path: PathBuf) {
        self.filter.clear();
        self.deep = false;
        self.cwd = path;
        self.selected = 0;
        self.anchor = 0;
        self.picked.clear();
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

    /// L'élément `i` fait-il partie de la sélection ?
    pub fn is_picked(&self, i: usize) -> bool {
        if self.picked.is_empty() {
            i == self.selected
        } else {
            self.entries
                .get(i)
                .is_some_and(|e| self.picked.contains(&e.path))
        }
    }

    /// Éléments sélectionnés, dans l'ordre de la liste
    pub fn selection(&self) -> Vec<&Entry> {
        (0..self.entries.len())
            .filter(|&i| self.is_picked(i))
            .map(|i| &self.entries[i])
            .collect()
    }

    pub fn multiple(&self) -> bool {
        self.picked.len() > 1
    }

    /// Sélection simple
    pub fn select(&mut self, i: usize) {
        self.selected = i;
        self.anchor = i;
        self.picked.clear();
    }

    /// Ctrl+clic : ajoute ou retire un élément
    pub fn toggle(&mut self, i: usize) {
        let Some(path) = self.entries.get(i).map(|e| e.path.clone()) else {
            return;
        };
        if self.picked.is_empty()
            && let Some(e) = self.entries.get(self.selected)
        {
            self.picked.insert(e.path.clone());
        }
        if !self.picked.remove(&path) {
            self.picked.insert(path);
        }
        self.selected = i;
        self.anchor = i;
        self.normalize();
    }

    /// Maj+clic, Maj+flèches : de l'ancre jusqu'à `i`
    pub fn extend_to(&mut self, i: usize) {
        let i = i.min(self.entries.len().saturating_sub(1));
        let (a, b) = (self.anchor.min(i), self.anchor.max(i));
        self.picked = self.entries[a..=b.min(self.entries.len().saturating_sub(1))]
            .iter()
            .map(|e| e.path.clone())
            .collect();
        self.selected = i;
        self.normalize();
    }

    /// Sélection fixée d'un bloc (zone de sélection), curseur sur `cursor`
    pub fn pick_set(&mut self, set: HashSet<PathBuf>, cursor: usize) {
        self.picked = set;
        self.selected = cursor.min(self.entries.len().saturating_sub(1));
        self.normalize();
    }

    pub fn select_all(&mut self) {
        self.picked = self.entries.iter().map(|e| e.path.clone()).collect();
        self.normalize();
    }

    /// Un seul élément choisi : sélection simple ; le curseur reste sur un
    /// élément choisi
    fn normalize(&mut self) {
        self.picked
            .retain(|p| self.entries.iter().any(|e| &e.path == p));
        if self.picked.len() == 1 {
            let only = self.picked.drain().next();
            if let Some(i) = only.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
                self.selected = i;
            }
        } else if !self.picked.is_empty()
            && !self.is_picked(self.selected)
            && let Some(i) = (0..self.entries.len()).find(|&i| self.is_picked(i))
        {
            self.selected = i;
        }
    }

    /// Recherche à la frappe : première entrée qui commence par `prefix`.
    pub fn jump_to(&mut self, prefix: &str) {
        let p = prefix.to_lowercase();
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.name.to_lowercase().starts_with(&p))
        {
            self.select(i);
        }
    }
}

/// Texte comparable pour la recherche : minuscules, sans accents
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' | 'ì' => 'i',
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'ÿ' => 'y',
            other => other,
        })
        .collect()
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
