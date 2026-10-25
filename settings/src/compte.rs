//! Réglages > Compte : photo (`~/.face`), nom affiché et mot de passe de
//! l'utilisateur (voir common::account). La photo se choisit parmi les images
//! récentes (Images, Téléchargements) ou se dépose depuis Fichiers ; elle est
//! recadrée en carré au centre. Le mot de passe se change avec `passwd`, dans
//! un terminal. Vignettes et photo sont préparées dans un fil (une photo HEIC
//! met une seconde ou deux à se décoder).

use std::path::{Path, PathBuf};

use common::{
    account,
    draw::Image,
    theme::{self, icon},
};
use smithay_client_toolkit::{
    reexports::calloop::channel::Sender,
    seat::keyboard::{KeyEvent, Keysym},
};

use crate::{Hit, Msg, ROW_H, Ui};

/// Images proposées
const PHOTOS: usize = 12;
const AVATAR: f32 = 96.0;

pub struct ComptePage {
    msgs: Sender<Msg>,
    pub avatar: Option<Image>,
    name: String,
    /// Le nom vient-il des Réglages (sinon du système) ?
    custom: bool,
    photos: Vec<(PathBuf, Option<Image>)>,
    /// Saisie du nom en cours
    editing: Option<String>,
    /// Photo en cours de préparation
    pub busy: bool,
}

impl ComptePage {
    pub fn load(msgs: Sender<Msg>) -> Self {
        let photos: Vec<(PathBuf, Option<Image>)> = account::recent_photos(PHOTOS)
            .into_iter()
            .map(|p| (p, None))
            .collect();
        // Vignettes carrées, dans un fil
        let paths: Vec<PathBuf> = photos.iter().map(|(p, _)| p.clone()).collect();
        let tx = msgs.clone();
        std::thread::spawn(move || {
            for path in paths {
                let thumb = square_thumb(&path, 160);
                if tx.send(Msg::Photo(path, thumb)).is_err() {
                    return;
                }
            }
        });
        Self {
            msgs,
            avatar: account::load_avatar(),
            name: account::display_name(),
            custom: account::custom_name().is_some(),
            photos,
            editing: None,
            busy: false,
        }
    }

    pub fn on_photo(&mut self, path: PathBuf, thumb: Option<Image>) {
        if let Some(slot) = self.photos.iter_mut().find(|(p, _)| *p == path) {
            slot.1 = thumb;
        }
    }

    /// Photo posée (ou échec) ; un message à afficher
    pub fn on_avatar(&mut self, result: Result<(), String>) -> String {
        self.busy = false;
        match result {
            Ok(()) => {
                self.avatar = account::load_avatar();
                "Photo enregistrée".into()
            }
            Err(e) => format!("Photo non enregistrée : {e}"),
        }
    }

    /// Fait d'une image la photo, dans un fil
    pub fn set_avatar(&mut self, source: &Path) {
        if self.busy {
            return;
        }
        self.busy = true;
        let (tx, source) = (self.msgs.clone(), source.to_path_buf());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Avatar(account::set_avatar(&source)));
        });
    }

    pub fn editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Frappe pendant la saisie du nom ; un message à afficher, s'il y a lieu
    pub fn key(&mut self, event: &KeyEvent) -> Option<String> {
        let text = self.editing.as_mut()?;
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.finish_name(true),
            Keysym::Escape => self.finish_name(false),
            Keysym::BackSpace => {
                text.pop();
                None
            }
            _ => {
                let t = event
                    .utf8
                    .as_ref()
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))?;
                if text.chars().count() < 60 {
                    text.push_str(t);
                }
                None
            }
        }
    }

    /// Fin de la saisie : enregistrer (vide : le nom du système revient) ou annuler
    fn finish_name(&mut self, keep: bool) -> Option<String> {
        let typed = self.editing.take()?;
        if !keep {
            return None;
        }
        let msg = match account::set_name(&typed) {
            Ok(()) if typed.trim().is_empty() => "Nom du système repris".to_string(),
            Ok(()) => "Nom enregistré".to_string(),
            Err(e) => format!("Nom non enregistré : {e}"),
        };
        self.name = account::display_name();
        self.custom = account::custom_name().is_some();
        Some(msg)
    }

    /// Clic sur un élément de la page ; un message à afficher, s'il y a lieu
    pub fn click(&mut self, hit: &Hit) -> Option<String> {
        // Un clic ailleurs termine la saisie du nom
        let mut msg = None;
        if self.editing.is_some() && hit != &Hit::CompteName {
            msg = self.finish_name(true);
        }
        let out = match hit {
            Hit::ComptePhoto(i) => {
                let path = self.photos.get(*i)?.0.clone();
                self.set_avatar(&path);
                None
            }
            Hit::CompteRemovePhoto => {
                let r = account::remove_avatar();
                self.avatar = None;
                Some(match r {
                    Ok(()) => "Photo retirée".into(),
                    Err(e) => format!("Photo non retirée : {e}"),
                })
            }
            Hit::CompteName => {
                if self.editing.is_none() {
                    self.editing = Some(self.name.clone());
                }
                None
            }
            Hit::ComptePassword => {
                // passwd demande l'ancien mot de passe puis le nouveau, deux fois
                let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
                let script = "echo 'Changer le mot de passe de session (verrou, sudo, connexion)'; \
                              echo; passwd; echo; echo 'Entrée pour fermer'; read _";
                match std::process::Command::new(term)
                    .args(["sh", "-c", script])
                    .spawn()
                {
                    Ok(mut child) => {
                        std::thread::spawn(move || child.wait());
                        None
                    }
                    Err(e) => Some(format!("Terminal introuvable : {e}")),
                }
            }
            _ => None,
        };
        out.or(msg)
    }

    pub fn draw(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Photo");
        let card_h = AVATAR + 40.0;
        ui.card(x, y, w, card_h);
        let (cx, cy) = (x + 20.0 + AVATAR / 2.0, y + card_h / 2.0);
        match self.avatar.as_ref() {
            Some(img) => ui.image(
                img,
                cx - AVATAR / 2.0,
                cy - AVATAR / 2.0,
                AVATAR,
                AVATAR,
                AVATAR / 2.0,
            ),
            None => {
                ui.circle(cx, cy, AVATAR / 2.0, p.accent);
                ui.centered(
                    &account::initials(&self.name),
                    cx,
                    cy - 20.0,
                    40.0,
                    32.0,
                    true,
                    p.on_accent,
                );
            }
        }
        let tx = x + 40.0 + AVATAR;
        let tw = (x + w - 20.0 - tx).max(120.0);
        let status = if self.busy {
            "Préparation de la photo…"
        } else if self.avatar.is_some() {
            "Sur l'écran de verrouillage et dans les Réglages"
        } else {
            "Choisissez une image ci-dessous, ou déposez-en une ici depuis Fichiers"
        };
        let status = ui.truncate(status, 13.0, tw);
        ui.text(&status, tx, y + 26.0, 20.0, 13.0, p.text_soft());
        if self.avatar.is_some() && !self.busy {
            ui.button(
                tx,
                y + 58.0,
                140.0,
                "Retirer la photo",
                false,
                Hit::CompteRemovePhoto,
            );
        }
        y += card_h + 14.0;

        // Images récentes, en vignettes rondes
        if !self.photos.is_empty() {
            y = ui.section(x, y, "Images récentes");
            let side = 72.0;
            let gap = 14.0;
            let cols = (((w + gap) / (side + gap)).floor() as usize).max(1);
            for (i, (_, thumb)) in self.photos.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let px = x + col as f32 * (side + gap);
                let py = y + row as f32 * (side + gap);
                let hit = Hit::ComptePhoto(i);
                if ui.hover == Some(hit.clone()) {
                    ui.circle(px + side / 2.0, py + side / 2.0, side / 2.0 + 3.0, p.accent);
                }
                match thumb {
                    Some(img) => ui.image(img, px, py, side, side, side / 2.0),
                    None => {
                        ui.circle(px + side / 2.0, py + side / 2.0, side / 2.0, p.ink(0.06));
                        ui.centered("…", px + side / 2.0, py, side, 16.0, false, p.text_dim);
                    }
                }
                ui.hit((px, py, side, side), hit);
            }
            let rows = self.photos.len().div_ceil(cols);
            y += rows as f32 * (side + gap) + 14.0;
        }

        // Nom affiché
        y = ui.section(x, y, "Nom");
        ui.card(x, y, w, ROW_H);
        match self.editing.as_ref() {
            Some(text) => {
                let shown = ui.truncate(text, 13.5, w - 200.0);
                ui.text(&shown, x + 18.0, y, ROW_H, 13.5, p.text);
                let tw = ui.measure(&shown, 13.5);
                ui.rect(x + 19.0 + tw, y + ROW_H / 2.0 - 9.0, 2.0, 18.0, p.accent);
                if text.is_empty() {
                    ui.text("Nom du système", x + 26.0, y, ROW_H, 13.5, p.ink(0.3));
                }
            }
            None => {
                let shown = ui.truncate(&self.name, 13.5, w - 200.0);
                ui.text(&shown, x + 18.0, y, ROW_H, 13.5, p.text);
            }
        }
        ui.button(
            x + w - 18.0 - 110.0,
            y + (ROW_H - 34.0) / 2.0,
            110.0,
            if self.editing.is_some() {
                "Valider"
            } else {
                "Modifier"
            },
            false,
            Hit::CompteName,
        );
        y += ROW_H + 8.0;
        let origin = if self.custom {
            format!(
                "Nom choisi ici ; effacé, le nom du système revient. Identifiant de connexion : {}.",
                account::login()
            )
        } else {
            format!(
                "Nom du système (/etc/passwd). Identifiant de connexion : {}, il ne change pas.",
                account::login()
            )
        };
        y = ui.note(x, y, w, &origin) + 20.0;

        // Mot de passe
        y = ui.section(x, y, "Mot de passe");
        ui.card(x, y, w, ROW_H);
        ui.icon(icon::LOCK, x + 30.0, y + ROW_H / 2.0, 15.0, p.text_dim);
        ui.text("Mot de passe de session", x + 50.0, y, ROW_H, 13.5, p.text);
        ui.button(
            x + w - 18.0 - 110.0,
            y + (ROW_H - 34.0) / 2.0,
            110.0,
            "Changer…",
            false,
            Hit::ComptePassword,
        );
        y += ROW_H + 8.0;
        ui.note(
            x,
            y,
            w,
            "Celui du verrou, de la connexion et de sudo ; il se change dans un terminal (passwd). Le trousseau a son propre mot de passe (Réglages > Trousseau).",
        )
    }
}

/// Vignette carrée (recadrée au centre), prémultipliée B G R A
fn square_thumb(path: &Path, side: u32) -> Option<Image> {
    let img = account::decode(path).ok()?;
    let s = img.width().min(img.height());
    let (x, y) = ((img.width() - s) / 2, (img.height() - s) / 2);
    let img = img.crop_imm(x, y, s, s).thumbnail(side, side).to_rgba8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        *px = [
            (px[2] as u32 * a / 255) as u8,
            (px[1] as u32 * a / 255) as u8,
            (px[0] as u32 * a / 255) as u8,
            a as u8,
        ];
    }
    Some(Image {
        width,
        height,
        data,
    })
}
