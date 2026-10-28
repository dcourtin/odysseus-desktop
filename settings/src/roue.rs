//! Réglages > Roue : les huit places de la roue (odysseus-roue), choisies
//! parmi les entrées proposées par les applis, les actions des applis et les
//! applications, ou des dossiers qui en tiennent huit autres (second anneau).
//! Le statut a toujours une place : on le déplace, on ne le retire pas.
//! Chaque changement est enregistré aussitôt (`~/.config/odysseus/roue`) ;
//! la roue le lit à chaque ouverture.

use std::collections::HashMap;

use common::{
    apps::{self, App},
    draw::Image,
    roue::{self, Folder, Item, SLOTS, Slot, Wheel},
    theme::{self, icon},
};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use crate::{Hit, Ui};

const R_WHEEL: f32 = 128.0;
const R_PLACES: f32 = 90.0;
const R_PLACE: f32 = 26.0;
const LIST_ROW: f32 = 44.0;

pub struct RouePage {
    apps: Vec<App>,
    catalog: Vec<Item>,
    wheel: Wheel,
    /// Place choisie : ce qu'on ajoute y va
    selected: usize,
    /// Dossier ouvert (sa place dans la roue) : les places montrées sont les siennes
    folder: Option<usize>,
    /// Renommage du dossier choisi en cours : son nom d'avant (Échap le rend)
    renaming: Option<String>,
    /// Icônes chargées, par nom d'icône
    icons: HashMap<String, Option<Image>>,
}

impl RouePage {
    pub fn load() -> Self {
        let apps = apps::load();
        let catalog = roue::catalog(&apps);
        let wheel = roue::load(&apps);
        let mut icons = HashMap::new();
        for it in &catalog {
            icons
                .entry(it.icon.clone())
                .or_insert_with(|| common::icons::load(&it.icon, 64));
        }
        let selected = wheel.iter().position(Option::is_none).unwrap_or(0);
        Self {
            apps,
            catalog,
            wheel,
            selected,
            folder: None,
            renaming: None,
            icons,
        }
    }

    fn item(&self, slot: &Slot) -> Option<&Item> {
        self.catalog.iter().find(|it| &it.slot == slot)
    }

    /// Places montrées : celles de la roue, ou du dossier ouvert
    fn places(&self) -> &[Option<Slot>; SLOTS] {
        match self.folder.and_then(|f| self.wheel[f].as_ref()?.folder()) {
            Some(folder) => &folder.places,
            None => &self.wheel,
        }
    }

    fn places_mut(&mut self) -> &mut [Option<Slot>; SLOTS] {
        match self.folder {
            Some(f) if self.wheel[f].as_ref().is_some_and(|s| s.folder().is_some()) => {
                &mut self.wheel[f]
                    .as_mut()
                    .and_then(Slot::folder_mut)
                    .expect("dossier vérifié")
                    .places
            }
            _ => &mut self.wheel,
        }
    }

    /// Dossier à la place choisie de la roue
    fn selected_folder_mut(&mut self) -> Option<&mut Folder> {
        if self.folder.is_some() {
            return None;
        }
        self.wheel[self.selected].as_mut()?.folder_mut()
    }

    /// Où est déjà cet élément : (dossier, place)
    fn place_of(&self, slot: &Slot) -> Option<(Option<usize>, usize)> {
        if let Some(i) = self.wheel.iter().position(|p| p.as_ref() == Some(slot)) {
            return Some((None, i));
        }
        self.wheel.iter().enumerate().find_map(|(f, p)| {
            let folder = p.as_ref()?.folder()?;
            let j = folder
                .places
                .iter()
                .position(|q| q.as_ref() == Some(slot))?;
            Some((Some(f), j))
        })
    }

    /// « Place 3 », « Artcraft · 2 »
    fn place_label(&self, (folder, i): (Option<usize>, usize)) -> String {
        match folder.and_then(|f| self.wheel[f].as_ref()?.folder()) {
            Some(f) => format!("{} · {}", f.name, i + 1),
            None => format!("Place {}", i + 1),
        }
    }

    fn folder_name(&self) -> String {
        let taken = |n: &str| {
            self.wheel
                .iter()
                .flatten()
                .any(|s| s.folder().is_some_and(|f| f.name == n))
        };
        (1..)
            .map(|k| {
                if k == 1 {
                    "Dossier".to_string()
                } else {
                    format!("Dossier {k}")
                }
            })
            .find(|n| !taken(n))
            .unwrap_or_default()
    }

    /// Renommage en cours (la page prend alors le clavier)
    pub fn renaming(&self) -> bool {
        self.renaming.is_some()
    }

    /// Frappe pendant le renommage ; un message à afficher, s'il y a lieu
    pub fn key(&mut self, event: &KeyEvent) -> Option<String> {
        self.renaming.as_ref()?;
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.finish_rename(true),
            Keysym::Escape => self.finish_rename(false),
            Keysym::BackSpace => {
                self.selected_folder_mut()?.name.pop();
                None
            }
            _ => {
                let t = event
                    .utf8
                    .as_ref()
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))?
                    .clone();
                let folder = self.selected_folder_mut()?;
                if folder.name.chars().count() < 40 {
                    folder.name.push_str(&t);
                }
                None
            }
        }
    }

    fn save(&self) -> Option<String> {
        roue::save(&self.wheel)
            .err()
            .map(|e| format!("Roue non enregistrée : {e}"))
    }

    /// Fin du renommage : garder le nom tapé (s'il n'est pas vide) ou rendre l'ancien
    fn finish_rename(&mut self, keep: bool) -> Option<String> {
        let before = self.renaming.take()?;
        let folder = self.selected_folder_mut()?;
        let typed = folder.name.trim().to_string();
        folder.name = if keep && !typed.is_empty() {
            typed
        } else {
            before
        };
        self.save()
    }

    /// Clic sur un élément de la page ; un message à afficher, s'il y a lieu
    pub fn click(&mut self, hit: &Hit) -> Option<String> {
        // Un clic ailleurs termine le renommage
        let msg = if hit != &Hit::RoueRename {
            self.finish_rename(true)
        } else {
            None
        };
        let out = match hit {
            Hit::RoueSlot(i) => {
                let is_folder = self.folder.is_none()
                    && self.wheel[*i]
                        .as_ref()
                        .is_some_and(|s| s.folder().is_some());
                if is_folder && self.selected == *i {
                    // Second clic sur un dossier : l'ouvrir
                    self.open_folder();
                } else {
                    self.selected = *i;
                }
                None
            }
            Hit::RoueAdd(ci) => {
                let slot = self.catalog.get(*ci)?.slot.clone();
                if slot == Slot::Statut && self.folder.is_some() {
                    return Some("Le statut va dans la roue, pas dans un dossier".into());
                }
                // Le statut ne se perd pas : il prend la place libérée par ce
                // qu'on ajoute, sinon la suivante libre
                let selected = self.selected;
                let displaces_status =
                    self.places()[selected] == Some(Slot::Statut) && slot != Slot::Statut;
                if displaces_status {
                    let home = match self.place_of(&slot) {
                        Some((None, i)) => Some(i),
                        _ => (1..SLOTS)
                            .map(|d| (selected + d) % SLOTS)
                            .find(|&i| self.wheel[i].is_none()),
                    };
                    let Some(home) = home else {
                        return Some(
                            "La roue est pleine : libérez une place pour le statut".into(),
                        );
                    };
                    self.wheel[home] = Some(Slot::Statut);
                    if let Some((Some(f), j)) = self.place_of(&slot)
                        && let Some(folder) = self.wheel[f].as_mut().and_then(Slot::folder_mut)
                    {
                        folder.places[j] = None;
                    }
                    self.wheel[selected] = Some(slot);
                    return self.save();
                }
                // Une seule place par élément : il quitte l'ancienne
                match self.place_of(&slot) {
                    Some((None, i)) => self.wheel[i] = None,
                    Some((Some(f), j)) => {
                        if let Some(folder) = self.wheel[f].as_mut().and_then(Slot::folder_mut) {
                            folder.places[j] = None;
                        }
                    }
                    None => {}
                }
                let selected = self.selected;
                self.places_mut()[selected] = Some(slot);
                // Place suivante libre, pour enchaîner les ajouts
                if let Some(next) = (1..SLOTS)
                    .map(|d| (self.selected + d) % SLOTS)
                    .find(|&i| self.places()[i].is_none())
                {
                    self.selected = next;
                }
                self.save()
            }
            Hit::RoueRemove => {
                let selected = self.selected;
                if self.places()[selected] == Some(Slot::Statut) {
                    return Some(
                        "Le statut reste dans la roue : déplacez-le avec les flèches".into(),
                    );
                }
                self.places_mut()[selected] = None;
                self.save()
            }
            Hit::RoueMove(d) => {
                let to = (self.selected as i32 + *d as i32).rem_euclid(SLOTS as i32) as usize;
                let from = self.selected;
                self.places_mut().swap(from, to);
                self.selected = to;
                self.save()
            }
            Hit::RoueFolder => {
                if self.folder.is_some()
                    || self.wheel[self.selected]
                        .as_ref()
                        .is_some_and(|s| s.folder().is_some() || *s == Slot::Statut)
                {
                    return msg;
                }
                // Nouveau dossier à la place choisie ; ce qu'elle tenait y entre
                let mut folder = Folder::new(&self.folder_name());
                folder.places[0] = self.wheel[self.selected].take();
                self.wheel[self.selected] = Some(Slot::Dossier(Box::new(folder)));
                let saved = self.save();
                // Et on le nomme tout de suite
                self.renaming = self
                    .selected_folder_mut()
                    .map(|f| std::mem::take(&mut f.name));
                saved
            }
            Hit::RoueOpen => {
                self.open_folder();
                None
            }
            Hit::RoueBack => {
                self.selected = self.folder.take().unwrap_or(self.selected);
                None
            }
            Hit::RoueRename => {
                if self.renaming.is_none() {
                    let name = self
                        .selected_folder_mut()
                        .map(|f| std::mem::take(&mut f.name));
                    self.renaming = name;
                }
                None
            }
            Hit::RoueReset => {
                self.wheel = roue::default(&self.apps);
                roue::with_status(&mut self.wheel);
                self.selected = 0;
                self.folder = None;
                self.save()
                    .or_else(|| Some("Roue remise aux favoris du Dock".into()))
            }
            _ => None,
        };
        out.or(msg)
    }

    fn open_folder(&mut self) {
        if self.folder.is_none()
            && self.wheel[self.selected]
                .as_ref()
                .is_some_and(|s| s.folder().is_some())
        {
            self.folder = Some(self.selected);
            self.selected = self.places().iter().position(Option::is_none).unwrap_or(0);
        }
    }

    fn draw_icon(&self, ui: &mut Ui, it: &Item, cx: f32, cy: f32, size: f32) {
        if it.slot == Slot::Statut {
            return Self::draw_status(ui, cx, cy, size);
        }
        match self.icons.get(&it.icon).and_then(Option::as_ref) {
            Some(img) => ui.image(img, cx - size / 2.0, cy - size / 2.0, size, size, 0.0),
            None => {
                let p = theme::palette();
                ui.circle(cx, cy, size / 2.0, p.accent.with_alpha(0.25));
                let initial: String = it.name.chars().take(1).collect::<String>().to_uppercase();
                ui.centered(
                    &initial,
                    cx,
                    cy - size / 2.0,
                    size,
                    size * 0.45,
                    true,
                    p.text,
                );
            }
        }
    }

    /// Statut : pastille liserée d'accent, icône des panneaux
    fn draw_status(ui: &mut Ui, cx: f32, cy: f32, size: f32) {
        let p = theme::palette();
        let r = size * 0.3;
        ui.rounded(
            cx - size / 2.0 - 1.5,
            cy - size / 2.0 - 1.5,
            size + 3.0,
            size + 3.0,
            r + 1.5,
            p.accent.with_alpha(0.55),
        );
        ui.rounded(
            cx - size / 2.0,
            cy - size / 2.0,
            size,
            size,
            r,
            p.panel.mix(p.accent, 0.18),
        );
        ui.icon(icon::CONTROLS, cx, cy, size * 0.5, p.text);
    }

    /// Dossier : pastille d'accent et icône de dossier
    fn draw_folder(ui: &mut Ui, cx: f32, cy: f32, size: f32) {
        let p = theme::palette();
        ui.rounded(
            cx - size / 2.0,
            cy - size / 2.0,
            size,
            size,
            size * 0.3,
            p.accent.with_alpha(0.2),
        );
        ui.icon(icon::FOLDER, cx, cy, size * 0.5, p.accent);
    }

    pub fn draw(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let open = self.folder.and_then(|f| self.wheel[f].as_ref()?.folder());
        let mut y = ui.section(x, y, open.map_or("Places", |_| "Places du dossier"));
        let card_h = 2.0 * R_WHEEL + 32.0;
        ui.card(x, y, w, card_h);
        // La roue, comme elle s'ouvre (place 1 en haut, sens des aiguilles d'une
        // montre), ou les places du dossier ouvert, dossier au centre
        let (cx, cy) = (x + 16.0 + R_WHEEL, y + 16.0 + R_WHEEL);
        ui.circle(cx, cy, R_WHEEL, p.ink(0.05));
        if open.is_some() {
            Self::draw_folder(ui, cx, cy, 44.0);
        } else {
            ui.circle(cx, cy, 30.0, p.ink(0.05));
        }
        let places = self.places();
        for (i, place) in places.iter().enumerate() {
            let a = (-90.0 + i as f32 * 360.0 / SLOTS as f32).to_radians();
            let (px, py) = (cx + R_PLACES * a.cos(), cy + R_PLACES * a.sin());
            let hit = Hit::RoueSlot(i);
            if i == self.selected {
                ui.circle(px, py, R_PLACE + 3.0, p.accent);
                ui.circle(px, py, R_PLACE, p.content);
            } else if ui.hover == Some(hit.clone()) {
                ui.circle(px, py, R_PLACE, p.tile_hover());
            }
            match place.as_ref() {
                Some(Slot::Dossier(_)) => Self::draw_folder(ui, px, py, 34.0),
                Some(s) => match self.item(s) {
                    Some(it) => self.draw_icon(ui, it, px, py, 32.0),
                    None => ui.icon(icon::PLUS, px, py, 14.0, p.ink(0.35)),
                },
                None => ui.icon(icon::PLUS, px, py, 14.0, p.ink(0.35)),
            }
            ui.hit(
                (px - R_PLACE, py - R_PLACE, 2.0 * R_PLACE, 2.0 * R_PLACE),
                hit,
            );
        }

        // La place choisie
        let dx = x + 2.0 * R_WHEEL + 56.0;
        let dw = (x + w - 20.0 - dx).max(120.0);
        let mut dy = y + 28.0;
        let heading = match open {
            Some(f) => format!("{} · place {}", f.name, self.selected + 1),
            None => format!("Place {}", self.selected + 1),
        };
        let heading = ui.truncate(&heading, 12.0, dw);
        ui.text(&heading, dx, dy, 18.0, 12.0, p.text_dim);
        dy += 22.0;
        let current = places[self.selected].as_ref();
        match current {
            Some(Slot::Dossier(f)) => {
                if self.renaming.is_some() {
                    // Saisie du nom, avec son curseur
                    let typed = ui.truncate(&f.name, 16.0, dw - 12.0);
                    ui.bold_text(&typed, dx, dy, 24.0, 16.0, p.text);
                    let tw = ui.measure(&typed, 16.0);
                    ui.rect(dx + tw + 2.0, dy + 3.0, 2.0, 18.0, p.accent);
                    if f.name.is_empty() {
                        ui.text("Nom du dossier", dx + 10.0, dy, 24.0, 16.0, p.ink(0.3));
                    }
                } else {
                    let name = ui.truncate(&f.name, 16.0, dw);
                    ui.bold_text(&name, dx, dy, 24.0, 16.0, p.text);
                }
                dy += 26.0;
                let n = f.filled().count();
                let kind = match n {
                    0 => "Dossier vide".to_string(),
                    1 => "Dossier · 1 place".to_string(),
                    n => format!("Dossier · {n} places"),
                };
                ui.text(&kind, dx, dy, 18.0, 12.5, p.text_dim);
            }
            Some(slot) => match self.item(slot) {
                Some(it) => {
                    let name = ui.truncate(&it.name, 16.0, dw);
                    ui.bold_text(&name, dx, dy, 24.0, 16.0, p.text);
                    dy += 26.0;
                    let kind = match &it.slot {
                        Slot::Action(..) => format!("Action de {}", it.detail),
                        Slot::Entree(_) => "Proposée par une appli".to_string(),
                        Slot::Statut => it.detail.clone(),
                        _ => "Application".to_string(),
                    };
                    let kind = ui.truncate(&kind, 12.5, dw);
                    ui.text(&kind, dx, dy, 18.0, 12.5, p.text_dim);
                }
                None => {
                    ui.bold_text("Introuvable", dx, dy, 24.0, 16.0, p.text);
                    dy += 26.0;
                    let t = ui.truncate(&slot.to_text(), 12.5, dw);
                    ui.text(&t, dx, dy, 18.0, 12.5, p.text_dim);
                }
            },
            None => {
                ui.bold_text("Vide", dx, dy, 24.0, 16.0, p.text_dim);
                dy += 26.0;
                ui.text(
                    "Choisissez ce qu'elle lance ci-dessous",
                    dx,
                    dy,
                    18.0,
                    12.5,
                    p.text_dim,
                );
            }
        }
        dy += 40.0;
        ui.button(dx, dy, 40.0, "←", false, Hit::RoueMove(-1));
        ui.button(dx + 48.0, dy, 40.0, "→", false, Hit::RoueMove(1));
        let is_status = current == Some(&Slot::Statut);
        if current.is_some() && !is_status {
            ui.button(dx + 96.0, dy, 96.0, "Retirer", false, Hit::RoueRemove);
        }
        dy += 42.0;
        let is_folder = current.is_some_and(|s| s.folder().is_some());
        if open.is_some() {
            ui.button(dx, dy, 150.0, "Retour à la roue", false, Hit::RoueBack);
        } else if is_folder {
            ui.button(dx, dy, 88.0, "Ouvrir", true, Hit::RoueOpen);
            ui.button(dx + 96.0, dy, 104.0, "Renommer", false, Hit::RoueRename);
        } else if !is_status {
            ui.button(dx, dy, 150.0, "En faire un dossier", false, Hit::RoueFolder);
        }
        dy += 50.0;
        let tip = if is_status {
            "Le statut a toujours une place dans la roue : les flèches le déplacent. Il s'ouvre en anneau, et chaque entrée peut en ouvrir un autre (le menu d'une icône d'appli)."
        } else if open.is_some() {
            "Ces places s'ouvrent en second anneau, autour de la roue, quand on passe sur le dossier."
        } else if is_folder {
            "Un dossier ouvre un second anneau autour de la roue. Ouvrez-le pour choisir ses places."
        } else {
            "Les flèches déplacent la place autour de la roue. Une appli déjà ouverte revient au premier plan."
        };
        ui.note(dx, dy, dw, tip);
        y += card_h + 14.0;
        y = ui.note(
            x,
            y,
            w,
            "Ouvrir la roue : Super+R, clic du milieu sur le bureau, ou Super + clic du milieu n'importe où. Au clavier, les chiffres 1 à 8 ; Échap referme le dossier ouvert, puis la roue.",
        ) + 10.0;
        ui.button(
            x,
            y,
            220.0,
            "Reprendre les favoris du Dock",
            false,
            Hit::RoueReset,
        );
        y += 34.0 + 28.0;

        // Ce qu'on peut y mettre, par groupe
        let groups: [(&str, fn(&Slot) -> bool); 4] = [
            ("Bureau", |s| matches!(s, Slot::Statut)),
            ("Proposées par les applis", |s| {
                matches!(s, Slot::Entree(_))
            }),
            ("Actions des applis", |s| matches!(s, Slot::Action(..))),
            ("Applications", |s| matches!(s, Slot::App(_))),
        ];
        for (title, keep) in groups {
            let rows: Vec<(usize, &Item)> = self
                .catalog
                .iter()
                .enumerate()
                .filter(|(_, it)| keep(&it.slot))
                .collect();
            if rows.is_empty() {
                continue;
            }
            y = ui.section(x, y, title);
            ui.card(x, y, w, rows.len() as f32 * LIST_ROW);
            for (k, (ci, it)) in rows.into_iter().enumerate() {
                let ry = y + k as f32 * LIST_ROW;
                let hit = Hit::RoueAdd(ci);
                if k > 0 {
                    ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
                }
                let hovered = ui.hover == Some(hit.clone());
                if hovered {
                    ui.rounded(x + 4.0, ry + 3.0, w - 8.0, LIST_ROW - 6.0, 9.0, p.ink(0.04));
                }
                self.draw_icon(ui, it, x + 30.0, ry + LIST_ROW / 2.0, 24.0);
                let right = 120.0;
                let name = ui.truncate(&it.name, 13.5, (w - 70.0 - right) * 0.5);
                ui.text(&name, x + 54.0, ry, LIST_ROW, 13.5, p.text);
                let nw = ui.measure(&name, 13.5);
                if !it.detail.is_empty() {
                    let room = w - 54.0 - nw - 16.0 - right;
                    let detail = ui.truncate(&it.detail, 12.0, room.max(0.0));
                    ui.text(
                        &detail,
                        x + 54.0 + nw + 12.0,
                        ry,
                        LIST_ROW,
                        12.0,
                        p.text_dim,
                    );
                }
                match self.place_of(&it.slot) {
                    Some(place) => {
                        let label = self.place_label(place);
                        let pw = ui.measure(&label, 11.0) + 16.0;
                        ui.pill(x + w - 16.0 - pw, ry + (LIST_ROW - 20.0) / 2.0, &label);
                    }
                    None if hovered => {
                        let label = match open {
                            Some(f) => format!("Ajouter à {} · {}", f.name, self.selected + 1),
                            None => format!("Ajouter à la place {}", self.selected + 1),
                        };
                        let lw = ui.measure(&label, 12.5);
                        ui.text(&label, x + w - 16.0 - lw, ry, LIST_ROW, 12.5, p.accent);
                    }
                    None => {}
                }
                ui.hit((x, ry, w, LIST_ROW), hit);
            }
            y += self.catalog.iter().filter(|it| keep(&it.slot)).count() as f32 * LIST_ROW + 28.0;
        }
        ui.note(
            x,
            y,
            w,
            "Pour les applis : déposer un fichier .desktop (Name, Exec, Icon, Comment) dans ~/.local/share/odysseus/roue ou /usr/share/odysseus/roue, ou lancer « odysseus-roue proposer --id ID --nom NOM --commande CMD ». Une proposition attend ici que vous la placiez ; les actions d'une appli viennent de son fichier .desktop.",
        )
    }
}
