//! Fichiers à la souris : renommer sur place, sélection par zone (glisser
//! depuis un espace vide), menu du clic droit, nouveau dossier.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba},
};
use smithay_client_toolkit::seat::keyboard::KeyEvent;

use crate::{
    Files, ROW_H,
    field::{Field, Outcome},
};

/// Nom en cours de modification, dans sa ligne
pub struct Rename {
    pub path: PathBuf,
    pub field: Field,
}

/// Rectangle de sélection en cours (pixels logiques de la fenêtre)
pub struct Band {
    pub start: (f64, f64),
    pub now: (f64, f64),
    /// Sélection d'avant (Ctrl : la zone s'y ajoute)
    pub base: HashSet<PathBuf>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum MenuItem {
    Open,
    Rename,
    CopyPath,
    AskAgent,
    Trash,
    Restore,
    Extract,
    NewFolder,
    Terminal,
    ToggleHidden,
    Duplicate,
    Compress,
    Copy,
    Cut,
    Paste,
}

pub struct Menu {
    /// Coin haut gauche (pixels logiques)
    pub x: f64,
    pub y: f64,
    pub items: Vec<(&'static str, MenuItem)>,
    pub hover: Option<usize>,
}

pub const MENU_W: f64 = 230.0;
pub const MENU_ROW: f64 = 30.0;

impl Menu {
    pub fn height(&self) -> f64 {
        self.items.len() as f64 * MENU_ROW + 12.0
    }

    pub fn item_at(&self, x: f64, y: f64) -> Option<usize> {
        if x < self.x || x >= self.x + MENU_W || y < self.y + 6.0 {
            return None;
        }
        let i = ((y - self.y - 6.0) / MENU_ROW).floor() as usize;
        (i < self.items.len()).then_some(i)
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + MENU_W && y >= self.y && y < self.y + self.height()
    }
}

/// Nom libre dans `dir` : « Nouveau dossier », puis « Nouveau dossier 2 »…
fn free_name(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(base);
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{base} {n}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

impl Files {
    // --- Renommer ---------------------------------------------------------------

    /// Commence à renommer l'élément sélectionné (un seul) : le nom sans son
    /// extension est sélectionné, prêt à être remplacé
    pub fn start_rename(&mut self) {
        if self.browser.multiple() || self.in_trash() {
            return;
        }
        let Some(e) = self.browser.entries.get(self.browser.selected) else {
            return;
        };
        let text = e.name.clone();
        let stem_end = if e.is_dir {
            text.len()
        } else {
            text.rfind('.').filter(|i| *i > 0).unwrap_or(text.len())
        };
        let mut field = Field::new(&text);
        field.select(0, stem_end);
        self.rename = Some(Rename {
            path: e.path.clone(),
            field,
        });
        self.menu = None;
        self.follow_selection();
        self.draw();
    }

    /// Valide le nouveau nom (rien si inchangé ou vide)
    pub fn commit_rename(&mut self) {
        let Some(r) = self.rename.take() else {
            return;
        };
        let name = r.field.text.trim();
        let old = r
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() || name == old {
            return self.draw();
        }
        if name.contains('/') || name == "." || name == ".." {
            self.browser.message = Some(format!("« {name} » : nom impossible"));
            return self.draw();
        }
        let Some(dir) = r.path.parent() else {
            return;
        };
        let target = dir.join(name);
        if target.exists() {
            self.browser.message = Some(format!("« {name} » existe déjà ici"));
            return self.draw();
        }
        match std::fs::rename(&r.path, &target) {
            Ok(()) => {
                self.browser.reload();
                if let Some(i) = self.browser.entries.iter().position(|e| e.path == target) {
                    self.browser.select(i);
                }
            }
            Err(e) => self.browser.message = Some(format!("Renommage impossible : {e}")),
        }
        self.navigated();
    }

    pub fn cancel_rename(&mut self) {
        if self.rename.take().is_some() {
            self.draw();
        }
    }

    /// Clavier pendant le renommage ; vrai si la touche a servi
    pub fn rename_key(&mut self, event: &KeyEvent) -> bool {
        let (ctrl, shift) = (self.modifiers.ctrl, self.modifiers.shift);
        let Some(r) = self.rename.as_mut() else {
            return false;
        };
        match r.field.key(event, ctrl, shift) {
            Outcome::Submit => self.commit_rename(),
            Outcome::Cancel => self.cancel_rename(),
            _ => self.draw(),
        }
        true
    }

    /// Champ de renommage dessiné à la place du nom (pixels du tampon)
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rename_field(
        font: &Text,
        r: &Rename,
        c: &mut Canvas,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        s: f32,
    ) {
        let pad = 8.0 * s;
        r.field
            .draw(font, c, x - pad, y + 3.0 * s, w + pad, h - 6.0 * s, s, "");
    }

    // --- Nouveau dossier ----------------------------------------------------------

    pub fn new_folder(&mut self) {
        if self.in_trash() {
            return;
        }
        let path = free_name(&self.browser.cwd, "Nouveau dossier");
        match std::fs::create_dir(&path) {
            Ok(()) => {
                self.browser.reload();
                if let Some(i) = self.browser.entries.iter().position(|e| e.path == path) {
                    self.browser.select(i);
                }
                self.start_rename();
            }
            Err(e) => {
                self.browser.message = Some(format!("Dossier non créé : {e}"));
                self.draw();
            }
        }
    }

    // --- Sélection par zone -----------------------------------------------------

    pub fn start_band(&mut self, x: f64, y: f64) {
        let base = if self.modifiers.ctrl {
            self.browser
                .selection()
                .iter()
                .map(|e| e.path.clone())
                .collect()
        } else {
            HashSet::new()
        };
        self.band = Some(Band {
            start: (x, y),
            now: (x, y),
            base,
        });
    }

    /// La zone s'étend : les lignes qu'elle touche sont sélectionnées
    pub fn move_band(&mut self, x: f64, y: f64) {
        let Some(band) = self.band.as_mut() else {
            return;
        };
        band.now = (x, y);
        let (y0, y1) = (band.start.1.min(y), band.start.1.max(y));
        let top = self.rows_top;
        let first = self.browser.scroll as f64;
        let row = |yy: f64| first + ((yy - top) / ROW_H as f64).floor();
        let n = self.browser.entries.len();
        let (a, b) = (row(y0).max(0.0) as usize, row(y1).max(-1.0));
        let mut set = band.base.clone();
        if b >= 0.0 {
            for i in a..=(b as usize).min(n.saturating_sub(1)) {
                if let Some(e) = self.browser.entries.get(i) {
                    set.insert(e.path.clone());
                }
            }
        }
        let cursor = if y >= band.start.1 {
            b.max(0.0) as usize
        } else {
            a
        };
        self.browser.pick_set(set, cursor.min(n.saturating_sub(1)));
        self.draw();
    }

    pub fn end_band(&mut self) -> bool {
        self.band.take().is_some()
    }

    // --- Menu du clic droit -------------------------------------------------------

    /// Menu d'un élément (la sélection), ou du dossier (clic dans le vide)
    pub fn open_menu(&mut self, x: f64, y: f64, on_item: bool) {
        let trash = self.in_trash();
        let mut items: Vec<(&'static str, MenuItem)> = Vec::new();
        if on_item {
            let multiple = self.browser.multiple();
            let sel = self.browser.selection();
            let archive = sel.iter().any(|e| crate::archive::kind(&e.path).is_some());
            items.push(("Ouvrir", MenuItem::Open));
            if archive {
                items.push(("Décompresser ici", MenuItem::Extract));
            }
            if !multiple && !trash {
                items.push(("Renommer", MenuItem::Rename));
            }
            if !trash {
                items.push(("Copier", MenuItem::Copy));
                items.push(("Couper", MenuItem::Cut));
                items.push(("Dupliquer", MenuItem::Duplicate));
                items.push(("Compresser", MenuItem::Compress));
            }
            items.push((
                if multiple {
                    "Copier les chemins"
                } else {
                    "Copier le chemin"
                },
                MenuItem::CopyPath,
            ));
            items.push(("Demander à l'Assistant", MenuItem::AskAgent));
            items.push(if trash {
                ("Remettre à sa place", MenuItem::Restore)
            } else {
                ("Mettre à la corbeille", MenuItem::Trash)
            });
        } else {
            if !trash {
                items.push(("Nouveau dossier", MenuItem::NewFolder));
                if self.can_paste() {
                    items.push(("Coller", MenuItem::Paste));
                }
            }
            items.push(("Terminal ici", MenuItem::Terminal));
            items.push((
                if self.browser.show_hidden {
                    "Masquer les fichiers cachés"
                } else {
                    "Afficher les fichiers cachés"
                },
                MenuItem::ToggleHidden,
            ));
        }
        let mut menu = Menu {
            x,
            y,
            items,
            hover: None,
        };
        // Reste dans la fenêtre
        menu.x = menu.x.min(self.width as f64 - MENU_W - 4.0).max(4.0);
        menu.y = menu
            .y
            .min(self.height as f64 - menu.height() - 4.0)
            .max(4.0);
        self.menu = Some(menu);
        self.draw();
    }

    pub fn menu_action(&mut self, item: MenuItem) {
        self.menu = None;
        match item {
            MenuItem::Open => self.open_selected(),
            MenuItem::Rename => self.start_rename(),
            MenuItem::CopyPath => self.copy_path(),
            MenuItem::AskAgent => self.ask_agent(),
            MenuItem::Trash => self.trash_selected(),
            MenuItem::Restore => self.restore_selected(),
            MenuItem::Extract => self.extract_selected(),
            MenuItem::NewFolder => self.new_folder(),
            MenuItem::Terminal => {
                let dir = self.browser.cwd.clone();
                self.open_terminal(&dir);
            }
            MenuItem::Duplicate => self.duplicate(),
            MenuItem::Compress => self.compress(),
            MenuItem::Copy => self.clip_selection(false),
            MenuItem::Cut => self.clip_selection(true),
            MenuItem::Paste => self.paste(),
            MenuItem::ToggleHidden => {
                self.browser.toggle_hidden();
                self.navigated();
            }
        }
        self.draw();
    }

    pub fn draw_menu(font: &Text, m: &Menu, c: &mut Canvas, s: f32) {
        let p = theme::palette();
        let (x, y, w, h) = (
            m.x as f32 * s,
            m.y as f32 * s,
            MENU_W as f32 * s,
            m.height() as f32 * s,
        );
        c.fill_rounded(
            x + 2.0 * s,
            y + 4.0 * s,
            w,
            h,
            10.0 * s,
            Rgba::hex(0x000000, 0.25),
        );
        c.fill_rounded(x, y, w, h, 10.0 * s, p.panel);
        c.stroke_rounded(x, y, w, h, 10.0 * s, s, p.panel_border());
        for (i, (label, item)) in m.items.iter().enumerate() {
            let ry = y + (6.0 + i as f32 * MENU_ROW as f32) * s;
            let rh = MENU_ROW as f32 * s;
            let hovered = m.hover == Some(i);
            if hovered {
                c.fill_rounded(
                    x + 4.0 * s,
                    ry,
                    w - 8.0 * s,
                    rh,
                    6.0 * s,
                    p.accent.with_alpha(0.9),
                );
            }
            let color = if hovered {
                p.on_accent
            } else if matches!(item, MenuItem::Trash) {
                p.danger
            } else {
                p.text
            };
            font.draw_in(c, label, x + 14.0 * s, ry, rh, 13.5 * s, color);
        }
    }

    /// Rectangle de sélection (pixels du tampon)
    pub fn draw_band(b: &Band, c: &mut Canvas, s: f32) {
        let p = theme::palette();
        let (x0, y0) = (b.start.0.min(b.now.0) as f32, b.start.1.min(b.now.1) as f32);
        let (x1, y1) = (b.start.0.max(b.now.0) as f32, b.start.1.max(b.now.1) as f32);
        let (w, h) = ((x1 - x0) * s, (y1 - y0) * s);
        if w < 1.0 || h < 1.0 {
            return;
        }
        c.fill_rounded(x0 * s, y0 * s, w, h, 3.0 * s, p.accent.with_alpha(0.15));
        c.stroke_rounded(x0 * s, y0 * s, w, h, 3.0 * s, s, p.accent.with_alpha(0.8));
    }
}
