//! Vue en icônes (Ctrl+1 ; Ctrl+2 revient à la liste) : grille de tuiles,
//! vignettes des images décodées dans un fil, en commençant par celles qui
//! sont à l'écran.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

use common::{
    draw::{Canvas, Image, Text},
    theme,
};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::{Hit, edit::Rename, info, model::Browser};

/// Tuile (pixels logiques) : icône de ICON de côté, nom sur deux lignes
pub const TILE_W: f32 = 112.0;
pub const TILE_H: f32 = 124.0;
const ICON: f32 = 64.0;
/// Vignettes gardées en mémoire au plus (≈ 64 Ko chacune en 128 px)
const CACHE: usize = 600;

/// Élément affiché et son rectangle (x, y, largeur, hauteur ; pixels logiques)
pub type ItemRect = (usize, (f64, f64, f64, f64));

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    List,
    Icons,
}

impl View {
    fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".config/odysseus/fichiers-vue"))
    }

    /// Réglage enregistré : « icones » ou « liste »
    pub fn load() -> Self {
        match Self::file().and_then(|f| std::fs::read_to_string(f).ok()) {
            Some(t) if t.trim() == "icones" => View::Icons,
            _ => View::List,
        }
    }

    pub fn save(self) {
        let Some(file) = Self::file() else {
            return;
        };
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = match self {
            View::List => "liste\n",
            View::Icons => "icones\n",
        };
        let _ = std::fs::write(file, text);
    }
}

/// Liste de travail du fil des vignettes : remplacée à chaque dessin par les
/// images visibles qui n'en ont pas encore
type Work = Arc<(Mutex<Vec<PathBuf>>, Condvar)>;

pub struct Thumbs {
    cache: HashMap<PathBuf, Option<Image>>,
    work: Work,
}

impl Thumbs {
    /// Démarre le fil ; chaque vignette faite revient par `tx`
    pub fn new(tx: Sender<(PathBuf, Option<Image>)>) -> Self {
        let work: Work = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let shared = work.clone();
        std::thread::spawn(move || {
            let (list, ready) = &*shared;
            loop {
                let path = {
                    let mut list = list.lock().unwrap();
                    while list.is_empty() {
                        list = ready.wait(list).unwrap();
                    }
                    list.remove(0)
                };
                let side = (ICON * 2.0) as u32;
                let image = info::thumbnail(&path, side, side);
                if tx.send((path, image)).is_err() {
                    return;
                }
            }
        });
        Self {
            cache: HashMap::new(),
            work,
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Image> {
        self.cache.get(path)?.as_ref()
    }

    /// Images affichées sans vignette : à faire, dans cet ordre
    pub fn want(&self, missing: Vec<PathBuf>) {
        let (list, ready) = &*self.work;
        let mut list = list.lock().unwrap();
        *list = missing
            .into_iter()
            .filter(|p| !self.cache.contains_key(p))
            .collect();
        if !list.is_empty() {
            ready.notify_one();
        }
    }

    pub fn store(&mut self, path: PathBuf, image: Option<Image>) {
        if self.cache.len() >= CACHE {
            self.cache.clear();
        }
        self.cache.insert(path, image);
    }

    pub fn known(&self, path: &Path) -> bool {
        self.cache.contains_key(path)
    }
}

/// Ce qu'il faut pour dessiner la grille (le tampon emprunte déjà `Files`)
pub struct Tiles<'a> {
    pub regular: &'a Text,
    pub bold: &'a Text,
    pub browser: &'a Browser,
    pub thumbs: &'a Thumbs,
    pub rename: Option<&'a Rename>,
    pub drop_target: Option<&'a PathBuf>,
}

/// Colonnes de la grille pour une largeur de liste (pixels logiques)
pub fn columns(list_w: f32) -> usize {
    ((list_w / TILE_W).floor() as usize).max(1)
}

impl Tiles<'_> {
    /// Dessine les tuiles visibles dans [x0, right] × [top, bottom] (pixels
    /// du tampon). Renvoie les rectangles des tuiles (pixels logiques) et les
    /// images qui attendent leur vignette.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        c: &mut Canvas,
        x0: f32,
        top: f32,
        right: f32,
        bottom: f32,
        s: f32,
        hit: &mut dyn FnMut(f32, f32, f32, f32, Hit),
    ) -> (Vec<ItemRect>, Vec<PathBuf>) {
        let p = theme::palette();
        let b = self.browser;
        let cols = columns((right - x0) / s - 32.0);
        let (tw, th) = (TILE_W * s, TILE_H * s);
        // Grille centrée dans la largeur disponible
        let left = x0 + ((right - x0) - cols as f32 * tw) / 2.0;
        let rows = (((bottom - top) / th).ceil() as usize).max(1);
        let mut rects = Vec::new();
        let mut missing = Vec::new();
        let end = b.entries.len().min(b.scroll + rows * cols);
        for idx in b.scroll..end {
            let e = &b.entries[idx];
            let k = idx - b.scroll;
            let (tx, ty) = (left + (k % cols) as f32 * tw, top + (k / cols) as f32 * th);
            if ty >= bottom {
                break;
            }
            let picked = b.is_picked(idx);
            let target = e.is_dir && self.drop_target == Some(&e.path);
            // Fond de l'icône quand l'élément est choisi, ou cible d'un dépôt
            let (ix, iy, isz) = (tx + (tw - ICON * s) / 2.0, ty + 8.0 * s, ICON * s);
            if picked || target {
                let a = if target { 0.45 } else { 0.22 };
                c.fill_rounded(
                    ix - 6.0 * s,
                    iy - 6.0 * s,
                    isz + 12.0 * s,
                    isz + 12.0 * s,
                    10.0 * s,
                    p.accent.with_alpha(a),
                );
            }
            if e.is_dir {
                crate::draw_folder_icon(c, ix + 4.0 * s, iy + 10.0 * s, isz - 8.0 * s, p.accent);
            } else if let Some(img) = self.thumbs.get(&e.path) {
                // Vignette ajustée dans le carré, proportions gardées
                let r = (isz / img.width as f32).min(isz / img.height as f32);
                let (w, h) = (img.width as f32 * r, img.height as f32 * r);
                c.image(
                    img,
                    ix + (isz - w) / 2.0,
                    iy + (isz - h) / 2.0,
                    w,
                    h,
                    4.0 * s,
                );
            } else {
                if info::is_image(&e.path) && !self.thumbs.known(&e.path) {
                    missing.push(e.path.clone());
                }
                let fw = isz * 0.62;
                crate::draw_file_icon(c, ix + (isz - fw) / 2.0, iy + 4.0 * s, fw, p.text_dim);
                // Extension en capitales sur la feuille
                if let Some(ext) = e
                    .path
                    .extension()
                    .map(|x| x.to_string_lossy().to_uppercase())
                    && ext.len() <= 4
                {
                    let ew = self.bold.measure(&ext, 10.0 * s);
                    self.bold.draw_in(
                        c,
                        &ext,
                        ix + (isz - ew) / 2.0,
                        iy + isz * 0.62,
                        14.0 * s,
                        10.0 * s,
                        p.content,
                    );
                }
            }
            // Nom : deux lignes au plus, centrées ; pastille quand l'élément est choisi
            let ny = iy + isz + 10.0 * s;
            if let Some(r) = self.rename.filter(|r| r.path == e.path) {
                crate::Files::draw_rename_field(
                    self.regular,
                    r,
                    c,
                    tx + 10.0 * s,
                    ny - 6.0 * s,
                    tw - 12.0 * s,
                    28.0 * s,
                    s,
                );
            } else {
                let px = 12.5 * s;
                let lines = self.regular.wrap(&e.name, px, tw - 12.0 * s, 2);
                let lh = 16.0 * s;
                for (i, line) in lines.iter().enumerate() {
                    let lw = self.regular.measure(line, px);
                    let lx = tx + (tw - lw) / 2.0;
                    let ly = ny + i as f32 * lh;
                    if picked {
                        c.fill_rounded(
                            lx - 5.0 * s,
                            ly,
                            lw + 10.0 * s,
                            lh,
                            5.0 * s,
                            p.accent.with_alpha(0.85),
                        );
                    }
                    let color = if picked { p.on_accent } else { p.text };
                    self.regular.draw_in(c, line, lx, ly, lh, px, color);
                }
            }
            hit(tx, ty, tw, th, Hit::Row(idx));
            rects.push((
                idx,
                (
                    (tx / s) as f64,
                    (ty / s) as f64,
                    (tw / s) as f64,
                    (th / s) as f64,
                ),
            ));
        }
        (rects, missing)
    }
}
