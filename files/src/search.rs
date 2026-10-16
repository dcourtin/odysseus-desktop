//! Recherche (Ctrl+F) : le dossier affiché se filtre à la frappe ; « Sous-
//! dossiers » cherche aussi dans toute l'arborescence, dans un fil, les
//! résultats arrivant au fur et à mesure.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use crate::{
    Files,
    field::{Field, Outcome},
    model::{Entry, fold},
};

/// Au-delà, la recherche s'arrête (et le dit)
const MAX_RESULTS: usize = 2000;

pub struct Search {
    pub field: Field,
    /// Dossier où la recherche a commencé : en changer la ferme
    pub dir: PathBuf,
    pub deep: bool,
    /// Le clavier va au champ (sinon à la liste)
    pub focused: bool,
    /// Recherche dans les sous-dossiers en cours, ou arrêtée au maximum
    pub busy: bool,
    pub capped: bool,
    /// Numéro de la recherche en cours : un fil dont le numéro n'est plus le
    /// bon s'arrête
    generation: Arc<AtomicU64>,
}

/// Lot de résultats envoyé par le fil
pub struct Found {
    generation: u64,
    entries: Vec<Entry>,
    done: bool,
    capped: bool,
}

impl Files {
    /// Ctrl+F : ouvre la recherche, ou y remet le clavier
    pub fn start_search(&mut self) {
        match self.search.as_mut() {
            Some(s) => {
                s.focused = true;
                let len = s.field.text.len();
                s.field.select(0, len);
            }
            None => {
                self.search = Some(Search {
                    field: Field::default(),
                    dir: self.browser.cwd.clone(),
                    deep: false,
                    focused: true,
                    busy: false,
                    capped: false,
                    generation: Arc::new(AtomicU64::new(0)),
                });
            }
        }
        self.menu = None;
        self.goto = None;
        self.draw();
    }

    /// Clavier quand le champ de recherche l'a ; faux pour ce qui revient à
    /// la liste (flèches, pages)
    pub fn search_key(&mut self, event: &KeyEvent) -> bool {
        let (ctrl, shift) = (self.modifiers.ctrl, self.modifiers.shift);
        let Some(s) = self.search.as_mut().filter(|s| s.focused) else {
            return false;
        };
        if matches!(
            event.keysym,
            Keysym::Up | Keysym::Down | Keysym::Prior | Keysym::Next
        ) {
            return false;
        }
        match s.field.key(event, ctrl, shift) {
            Outcome::Changed => self.run_search(),
            // Entrée : le clavier passe aux résultats (Entrée renomme, Espace ouvre)
            Outcome::Submit => {
                s.focused = false;
                self.draw();
            }
            Outcome::Cancel => self.close_search(),
            Outcome::Moved => self.draw(),
            // Ctrl+F, Ctrl+A… : comme d'habitude
            Outcome::Ignored => return false,
        }
        true
    }

    pub fn close_search(&mut self) {
        if let Some(s) = self.search.take() {
            s.generation.fetch_add(1, Ordering::Relaxed);
        }
        self.browser.filter.clear();
        self.browser.deep = false;
        self.browser.reload();
        self.navigated();
    }

    /// « Ce dossier » / « Sous-dossiers »
    pub fn set_scope(&mut self, deep: bool) {
        if let Some(s) = self.search.as_mut()
            && s.deep != deep
        {
            s.deep = deep;
            self.run_search();
        }
    }

    /// Relance la recherche avec le texte du champ
    pub fn run_search(&mut self) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        let generation = s.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let query = fold(s.field.text.trim());
        s.capped = false;
        self.browser.scroll = 0;
        if !s.deep || query.is_empty() {
            s.busy = false;
            self.browser.deep = false;
            self.browser.filter = query;
            self.browser.reload();
            self.browser.select(0);
            return self.draw();
        }
        s.busy = true;
        self.browser.deep = true;
        self.browser.filter = query.clone();
        self.browser.entries.clear();
        self.browser.select(0);
        let (root, hidden) = (self.browser.cwd.clone(), self.browser.show_hidden);
        let (current, tx) = (s.generation.clone(), self.found.clone());
        std::thread::spawn(move || walk(&root, &query, hidden, generation, &current, &tx));
        self.draw();
    }

    /// Le dossier affiché a changé : la recherche se ferme
    pub fn left_search_dir(&mut self) {
        if self
            .search
            .as_ref()
            .is_some_and(|s| s.dir != self.browser.cwd)
            && let Some(s) = self.search.take()
        {
            s.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Lot de résultats reçu du fil
    pub fn found(&mut self, found: Found) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        if found.generation != s.generation.load(Ordering::Relaxed) {
            return;
        }
        if found.done {
            s.busy = false;
            s.capped = found.capped;
        }
        let first = self.browser.entries.is_empty();
        let selected = self
            .browser
            .entries
            .get(self.browser.selected)
            .map(|e| e.path.clone());
        self.browser.entries.extend(found.entries);
        self.browser.sort_entries();
        // La sélection reste sur le même élément pendant que la liste grandit
        if let Some(i) =
            selected.and_then(|p| self.browser.entries.iter().position(|e| e.path == p))
            && !first
        {
            self.browser.selected = i;
        }
        self.draw();
    }
}

/// Parcours en largeur depuis `root`, sans suivre les liens ; envoie les
/// résultats par lots
fn walk(
    root: &Path,
    query: &str,
    hidden: bool,
    generation: u64,
    current: &AtomicU64,
    tx: &smithay_client_toolkit::reexports::calloop::channel::Sender<Found>,
) {
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut batch = Vec::new();
    let mut total = 0;
    let mut sent = Instant::now();
    let mut capped = false;
    'outer: while let Some(dir) = queue.pop_front() {
        let Ok(list) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in list.flatten() {
            if current.load(Ordering::Relaxed) != generation {
                return;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let path = item.path();
            let Ok(kind) = item.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // Pseudo-systèmes de fichiers : rien à y chercher
                if !matches!(path.to_str(), Some("/proc" | "/sys" | "/dev" | "/run")) {
                    queue.push_back(path.clone());
                }
            }
            if !fold(&name).contains(query) {
                continue;
            }
            let meta = std::fs::metadata(&path).ok();
            let place = dir
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_default();
            batch.push(Entry {
                name,
                is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                size: meta.as_ref().map_or(0, |m| m.len()),
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                place: Some(place),
                path,
            });
            total += 1;
            if total >= MAX_RESULTS {
                capped = true;
                break 'outer;
            }
            if batch.len() >= 200 || sent.elapsed() > Duration::from_millis(150) {
                let entries = std::mem::take(&mut batch);
                if tx
                    .send(Found {
                        generation,
                        entries,
                        done: false,
                        capped: false,
                    })
                    .is_err()
                {
                    return;
                }
                sent = Instant::now();
            }
        }
    }
    let _ = tx.send(Found {
        generation,
        entries: batch,
        done: true,
        capped,
    });
}
