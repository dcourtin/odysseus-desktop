//! Aimantation des fenêtres, à la manière de macOS / Windows : glisser une
//! fenêtre contre un bord propose une zone (aperçu translucide) ; relâcher
//! l'y range. Bord gauche ou droit : moitié ; haut : plein écran utile ;
//! coins : quart. Une fenêtre rangée reprend sa taille quand on la décroche.
//! Clavier : Super+← / → (moitiés), Super+↑ (agrandir), Super+↓ (restaurer).

use std::time::Instant;

use smithay::{
    desktop::Window,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::State, wayland_server::Resource,
    },
    utils::{Logical, Point, Rectangle},
};

use crate::{
    Odysseus,
    anim::{Motion, SnapPreview},
};

/// Distance au bord qui déclenche l'aimantation (pixels logiques)
const EDGE: f64 = 14.0;
/// Hauteur des zones de coin, le long des bords gauche et droit
const CORNER: f64 = 140.0;
/// Zones de coin, en fraction de l'écran : un bord touché dans ces zones donne un quart
const CORNER_W: f64 = 0.15;
const CORNER_H: f64 = 0.20;
/// Écart entre les fenêtres rangées et autour d'elles
const GAP: i32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Odysseus {
    /// Zone visée par le pointeur pendant un déplacement de fenêtre
    pub fn snap_zone_at(&self, pos: Point<f64, Logical>) -> Option<Zone> {
        let out = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))?
            .to_f64();
        let area = self.usable_area()?.to_f64();
        // Bords touchés (le bas ne sert qu'aux coins)
        let left = pos.x <= out.loc.x + EDGE;
        let right = pos.x >= out.loc.x + out.size.w - EDGE;
        let top = pos.y <= area.loc.y + EDGE;
        let bottom = pos.y >= out.loc.y + out.size.h - EDGE;
        // Zones de coin, généreuses : un des deux bords du coin suffit
        let corner_w = (out.size.w * CORNER_W).max(CORNER);
        let corner_h = (area.size.h * CORNER_H).max(CORNER);
        let near_left = pos.x <= out.loc.x + corner_w;
        let near_right = pos.x >= out.loc.x + out.size.w - corner_w;
        let near_top = pos.y <= area.loc.y + corner_h;
        let near_bottom = pos.y >= out.loc.y + out.size.h - corner_h;
        let corner = |side: bool, end: bool, near_side: bool, near_end: bool| {
            (side || end) && near_side && near_end
        };
        Some(if corner(left, top, near_left, near_top) {
            Zone::TopLeft
        } else if corner(right, top, near_right, near_top) {
            Zone::TopRight
        } else if corner(left, bottom, near_left, near_bottom) {
            Zone::BottomLeft
        } else if corner(right, bottom, near_right, near_bottom) {
            Zone::BottomRight
        } else if left {
            Zone::Left
        } else if right {
            Zone::Right
        } else if top {
            Zone::Top
        } else {
            return None;
        })
    }

    /// Rectangle d'une zone dans la surface utile (hors barre), avec marges
    pub fn zone_rect(&self, zone: Zone) -> Option<Rectangle<i32, Logical>> {
        let a = self.usable_area()?;
        let (x, y, w, h) = (
            a.loc.x + GAP,
            a.loc.y + GAP,
            a.size.w - 2 * GAP,
            a.size.h - 2 * GAP,
        );
        let (hw, hh) = ((w - GAP) / 2, (h - GAP) / 2);
        let (rx, by) = (x + hw + GAP, y + hh + GAP);
        let (rw, bh) = (w - hw - GAP, h - hh - GAP);
        Some(match zone {
            Zone::Top => Rectangle::new((x, y).into(), (w, h).into()),
            Zone::Left => Rectangle::new((x, y).into(), (hw, h).into()),
            Zone::Right => Rectangle::new((rx, y).into(), (rw, h).into()),
            Zone::TopLeft => Rectangle::new((x, y).into(), (hw, hh).into()),
            Zone::TopRight => Rectangle::new((rx, y).into(), (rw, hh).into()),
            Zone::BottomLeft => Rectangle::new((x, by).into(), (hw, bh).into()),
            Zone::BottomRight => Rectangle::new((rx, by).into(), (rw, bh).into()),
        })
    }

    /// Pendant le déplacement : met à jour l'aperçu (et son animation).
    pub fn update_snap_preview(&mut self, window: &Window, pos: Point<f64, Logical>) {
        let zone = self.snap_zone_at(pos).and_then(|z| self.zone_rect(z));
        match (zone, self.snap_preview) {
            (Some(zone), Some(p)) if p.zone == zone => {}
            (Some(zone), current) => {
                // Part de l'aperçu affiché, ou de la fenêtre elle-même
                let from = current.map(|p| p.current().0).unwrap_or_else(|| {
                    let loc = self.space.element_location(window).unwrap_or_default();
                    Rectangle::new(loc, window.geometry().size).to_f64()
                });
                self.snap_preview = Some(SnapPreview {
                    zone,
                    from,
                    start: Instant::now(),
                });
            }
            (None, _) => self.snap_preview = None,
        }
    }

    /// Fin du déplacement : range la fenêtre dans la zone visée, s'il y en a une.
    pub fn finish_snap(&mut self, window: &Window, pos: Point<f64, Logical>) {
        self.snap_preview = None;
        if let Some(zone) = self.snap_zone_at(pos) {
            self.snap_window(window, zone);
        }
    }

    /// Range une fenêtre dans une zone (animation depuis sa place actuelle).
    pub fn snap_window(&mut self, window: &Window, zone: Zone) {
        let Some(rect) = self.zone_rect(zone) else {
            return;
        };
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        let Some(loc) = self.space.element_location(window) else {
            return;
        };
        let id = toplevel.wl_surface().id();
        let current = Rectangle::new(loc, window.geometry().size);
        // Taille d'avant l'aimantation, gardée pour le décrochage
        self.restore.entry(id.clone()).or_insert(current);
        self.fullscreen.remove(&id);
        let from = self.visual_rect(window, loc);
        toplevel.with_pending_state(|s| {
            s.states.unset(State::Fullscreen);
            for state in [
                State::TiledLeft,
                State::TiledRight,
                State::TiledTop,
                State::TiledBottom,
            ] {
                s.states.unset(state);
            }
            if zone == Zone::Top {
                s.states.set(State::Maximized);
            } else {
                s.states.unset(State::Maximized);
                // Bords collés à d'autres fenêtres : le client retire ses ombres et coins
                let tiled: &[State] = match zone {
                    Zone::Left => &[State::TiledRight],
                    Zone::Right => &[State::TiledLeft],
                    Zone::TopLeft => &[State::TiledRight, State::TiledBottom],
                    Zone::TopRight => &[State::TiledLeft, State::TiledBottom],
                    Zone::BottomLeft => &[State::TiledRight, State::TiledTop],
                    Zone::BottomRight => &[State::TiledLeft, State::TiledTop],
                    Zone::Top => &[],
                };
                for state in tiled {
                    s.states.set(*state);
                }
            }
            s.size = Some(rect.size);
        });
        toplevel.send_pending_configure();
        self.space.map_element(window.clone(), rect.loc, true);
        self.motions.insert(
            id,
            Motion {
                from,
                to: rect.to_f64(),
                start: Instant::now(),
            },
        );
    }

    /// Décroche une fenêtre rangée au début d'un déplacement : elle reprend sa
    /// taille, placée pour rester sous le pointeur au même endroit relatif.
    /// Renvoie la nouvelle position de la fenêtre.
    pub fn unsnap_for_move(
        &mut self,
        window: &Window,
        pointer: Point<f64, Logical>,
    ) -> Option<Point<i32, Logical>> {
        let toplevel = window.toplevel()?.clone();
        let id = toplevel.wl_surface().id();
        let saved = self.restore.remove(&id)?;
        self.fullscreen.remove(&id);
        let loc = self.space.element_location(window)?;
        let size = window.geometry().size;
        let ratio = ((pointer.x - loc.x as f64) / size.w.max(1) as f64).clamp(0.0, 1.0);
        let new_loc: Point<i32, Logical> = (
            (pointer.x - ratio * saved.size.w as f64).round() as i32,
            (pointer.y - 12.0) as i32,
        )
            .into();
        let from = self.visual_rect(window, loc);
        toplevel.with_pending_state(|s| {
            for state in [
                State::Maximized,
                State::Fullscreen,
                State::TiledLeft,
                State::TiledRight,
                State::TiledTop,
                State::TiledBottom,
            ] {
                s.states.unset(state);
            }
            s.size = Some(saved.size);
        });
        toplevel.send_pending_configure();
        self.space.map_element(window.clone(), new_loc, true);
        self.motions.insert(
            id,
            Motion {
                from,
                to: Rectangle::new(new_loc, saved.size).to_f64(),
                start: Instant::now(),
            },
        );
        Some(new_loc)
    }

    /// Rectangle où la fenêtre est affichée en ce moment (animation comprise)
    pub fn visual_rect(
        &self,
        window: &Window,
        loc: Point<i32, Logical>,
    ) -> Rectangle<f64, Logical> {
        window
            .toplevel()
            .and_then(|t| self.motions.get(&t.wl_surface().id()))
            .filter(|m| !m.done())
            .map(|m| m.current())
            .unwrap_or_else(|| Rectangle::new(loc, window.geometry().size).to_f64())
    }
}
