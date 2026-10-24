//! Réglages > Roue : les huit places de la roue (odysseus-roue), choisies
//! parmi les entrées proposées par les applis, les actions des applis et les
//! applications. Chaque changement est enregistré aussitôt
//! (`~/.config/odysseus/roue`) ; la roue le lit à chaque ouverture.

use std::collections::HashMap;

use common::{
    apps::{self, App},
    draw::Image,
    roue::{self, Item, SLOTS, Slot, Wheel},
    theme::{self, icon},
};

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
            icons,
        }
    }

    fn item(&self, slot: &Slot) -> Option<&Item> {
        self.catalog.iter().find(|it| &it.slot == slot)
    }

    /// Place qui tient déjà cet élément
    fn place_of(&self, slot: &Slot) -> Option<usize> {
        self.wheel.iter().position(|p| p.as_ref() == Some(slot))
    }

    fn save(&self) -> Option<String> {
        roue::save(&self.wheel)
            .err()
            .map(|e| format!("Roue non enregistrée : {e}"))
    }

    /// Clic sur un élément de la page ; un message à afficher, s'il y a lieu
    pub fn click(&mut self, hit: &Hit) -> Option<String> {
        match hit {
            Hit::RoueSlot(i) => {
                self.selected = *i;
                None
            }
            Hit::RoueAdd(ci) => {
                let slot = self.catalog.get(*ci)?.slot.clone();
                // Une seule place par élément : il quitte l'ancienne
                if let Some(old) = self.place_of(&slot) {
                    self.wheel[old] = None;
                }
                self.wheel[self.selected] = Some(slot);
                // Place suivante libre, pour enchaîner les ajouts
                if let Some(next) = (1..SLOTS)
                    .map(|d| (self.selected + d) % SLOTS)
                    .find(|&i| self.wheel[i].is_none())
                {
                    self.selected = next;
                }
                self.save()
            }
            Hit::RoueRemove => {
                self.wheel[self.selected] = None;
                self.save()
            }
            Hit::RoueMove(d) => {
                let to = (self.selected as i32 + *d as i32).rem_euclid(SLOTS as i32) as usize;
                self.wheel.swap(self.selected, to);
                self.selected = to;
                self.save()
            }
            Hit::RoueReset => {
                self.wheel = roue::default(&self.apps);
                self.selected = 0;
                self.save()
                    .or_else(|| Some("Roue remise aux favoris du Dock".into()))
            }
            _ => None,
        }
    }

    fn draw_icon(&self, ui: &mut Ui, it: &Item, cx: f32, cy: f32, size: f32) {
        match self.icons.get(&it.icon).and_then(Option::as_ref) {
            Some(img) => ui.image(img, cx - size / 2.0, cy - size / 2.0, size, size, 0.0),
            None => {
                let p = theme::palette();
                ui.circle(cx, cy, size / 2.0, p.accent.with_alpha(0.25));
                let initial: String = it.name.chars().take(1).collect::<String>().to_uppercase();
                ui.centered(&initial, cx, cy - size / 2.0, size, size * 0.45, true, p.text);
            }
        }
    }

    pub fn draw(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Places");
        let card_h = 2.0 * R_WHEEL + 32.0;
        ui.card(x, y, w, card_h);
        // La roue, comme elle s'ouvre (place 1 en haut, sens des aiguilles d'une montre)
        let (cx, cy) = (x + 16.0 + R_WHEEL, y + 16.0 + R_WHEEL);
        ui.circle(cx, cy, R_WHEEL, p.ink(0.05));
        ui.circle(cx, cy, 30.0, p.ink(0.05));
        for i in 0..SLOTS {
            let a = (-90.0 + i as f32 * 360.0 / SLOTS as f32).to_radians();
            let (px, py) = (cx + R_PLACES * a.cos(), cy + R_PLACES * a.sin());
            let hit = Hit::RoueSlot(i);
            if i == self.selected {
                ui.circle(px, py, R_PLACE + 3.0, p.accent);
                ui.circle(px, py, R_PLACE, p.content);
            } else if ui.hover == Some(hit.clone()) {
                ui.circle(px, py, R_PLACE, p.tile_hover());
            }
            match self.wheel[i].as_ref().and_then(|s| self.item(s)) {
                Some(it) => self.draw_icon(ui, it, px, py, 32.0),
                None => ui.icon(icon::PLUS, px, py, 14.0, p.ink(0.35)),
            }
            ui.hit((px - R_PLACE, py - R_PLACE, 2.0 * R_PLACE, 2.0 * R_PLACE), hit);
        }

        // La place choisie
        let dx = x + 2.0 * R_WHEEL + 56.0;
        let dw = (x + w - 20.0 - dx).max(120.0);
        let mut dy = y + 28.0;
        ui.text(
            &format!("Place {}", self.selected + 1),
            dx,
            dy,
            18.0,
            12.0,
            p.text_dim,
        );
        dy += 22.0;
        let current = self.wheel[self.selected].as_ref();
        match current.map(|s| (s, self.item(s))) {
            Some((_, Some(it))) => {
                let name = ui.truncate(&it.name, 16.0, dw);
                ui.bold_text(&name, dx, dy, 24.0, 16.0, p.text);
                dy += 26.0;
                let kind = match &it.slot {
                    Slot::App(_) => "Application".to_string(),
                    Slot::Action(..) => format!("Action de {}", it.detail),
                    Slot::Entree(_) => "Proposée par une appli".to_string(),
                };
                let kind = ui.truncate(&kind, 12.5, dw);
                ui.text(&kind, dx, dy, 18.0, 12.5, p.text_dim);
            }
            Some((slot, None)) => {
                ui.bold_text("Introuvable", dx, dy, 24.0, 16.0, p.text);
                dy += 26.0;
                let t = ui.truncate(&slot.to_text(), 12.5, dw);
                ui.text(&t, dx, dy, 18.0, 12.5, p.text_dim);
            }
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
        if current.is_some() {
            ui.button(dx + 96.0, dy, 96.0, "Retirer", false, Hit::RoueRemove);
        }
        dy += 52.0;
        ui.note(
            dx,
            dy,
            dw,
            "Les flèches déplacent la place autour de la roue. Une appli déjà ouverte revient au premier plan.",
        );
        y += card_h + 14.0;
        y = ui.note(
            x,
            y,
            w,
            "Ouvrir la roue : Super+R, clic du milieu sur le bureau, ou Super + clic du milieu n'importe où. Au clavier, les chiffres 1 à 8 ; Échap la ferme.",
        ) + 10.0;
        ui.button(x, y, 220.0, "Reprendre les favoris du Dock", false, Hit::RoueReset);
        y += 34.0 + 28.0;

        // Ce qu'on peut y mettre, par groupe
        let groups: [(&str, fn(&Slot) -> bool); 3] = [
            ("Proposées par les applis", |s| matches!(s, Slot::Entree(_))),
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
                    ui.text(&detail, x + 54.0 + nw + 12.0, ry, LIST_ROW, 12.0, p.text_dim);
                }
                match self.place_of(&it.slot) {
                    Some(place) => {
                        let label = format!("Place {}", place + 1);
                        let pw = ui.measure(&label, 11.0) + 16.0;
                        ui.pill(x + w - 16.0 - pw, ry + (LIST_ROW - 20.0) / 2.0, &label);
                    }
                    None if hovered => {
                        let label = format!("Ajouter à la place {}", self.selected + 1);
                        let lw = ui.measure(&label, 12.5);
                        ui.text(&label, x + w - 16.0 - lw, ry, LIST_ROW, 12.5, p.accent);
                    }
                    None => {}
                }
                ui.hit((x, ry, w, LIST_ROW), hit);
            }
            y += self
                .catalog
                .iter()
                .filter(|it| keep(&it.slot))
                .count() as f32
                * LIST_ROW
                + 28.0;
        }
        ui.note(
            x,
            y,
            w,
            "Pour les applis : déposer un fichier .desktop (Name, Exec, Icon, Comment) dans ~/.local/share/odysseus/roue ou /usr/share/odysseus/roue, ou lancer « odysseus-roue proposer --id ID --nom NOM --commande CMD ». Une proposition attend ici que vous la placiez ; les actions d'une appli viennent de son fichier .desktop.",
        )
    }
}
