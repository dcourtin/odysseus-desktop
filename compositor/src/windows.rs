//! Gestion des fenêtres : focus, agrandir / plein écran (et restauration),
//! redimensionnement par les bords ou au Mod+clic droit, passage d'une fenêtre à l'autre.

use smithay::{
    desktop::Window,
    input::pointer::{Focus, GrabStartData},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::State, wayland_server::Resource,
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial},
};

use crate::{
    Odysseus,
    grabs::{ResizeSurfaceGrab, resize_grab::ResizeEdge},
};

/// Bande de saisie autour d'une fenêtre, à l'extérieur, et longueur des coins
/// le long des côtés (pixels logiques)
const EDGE_GRAB: f64 = 10.0;
const EDGE_CORNER: f64 = 24.0;

/// État demandé par un client avant que sa fenêtre soit placée
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialState {
    Maximized,
    Fullscreen,
}

impl Odysseus {
    /// Fenêtre pas encore placée qui demande d'être agrandie ou en plein écran :
    /// on le note, et on configure tout de suite la bonne taille.
    pub fn request_initial_state(
        &mut self,
        toplevel: &smithay::wayland::shell::xdg::ToplevelSurface,
        wanted: Option<InitialState>,
    ) {
        let id = toplevel.wl_surface().id();
        let area = match wanted {
            Some(InitialState::Maximized) => self.usable_area(),
            Some(InitialState::Fullscreen) => self
                .space
                .outputs()
                .next()
                .and_then(|o| self.space.output_geometry(o)),
            None => None,
        };
        match (wanted, area) {
            (Some(state), Some(area)) => {
                self.initial_state.insert(id, state);
                toplevel.with_pending_state(|s| {
                    s.states.set(if state == InitialState::Maximized {
                        State::Maximized
                    } else {
                        State::Fullscreen
                    });
                    s.size = Some(area.size);
                });
            }
            _ => {
                self.initial_state.remove(&id);
                toplevel.with_pending_state(|s| {
                    s.states.unset(State::Maximized);
                    s.states.unset(State::Fullscreen);
                });
            }
        }
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    /// Au placement d'une fenêtre qui avait demandé un état : prise de toute la
    /// zone, avec pour restauration la taille par défaut, centrée.
    pub fn apply_initial_state(&mut self, window: &Window, state: InitialState) -> bool {
        let area = match state {
            InitialState::Maximized => self.usable_area(),
            InitialState::Fullscreen => self
                .space
                .outputs()
                .next()
                .and_then(|o| self.space.output_geometry(o)),
        };
        let (Some(area), Some(usable), Some(toplevel)) =
            (area, self.usable_area(), window.toplevel())
        else {
            return false;
        };
        let size = smithay::utils::Size::from((usable.size.w * 55 / 100, usable.size.h * 60 / 100));
        let loc = (
            usable.loc.x + (usable.size.w - size.w) / 2,
            usable.loc.y + (usable.size.h - size.h) / 2,
        );
        self.restore
            .insert(toplevel.wl_surface().id(), Rectangle::new(loc.into(), size));
        if state == InitialState::Fullscreen {
            self.fullscreen.insert(toplevel.wl_surface().id());
        }
        self.space.map_element(window.clone(), area.loc, true);
        true
    }

    /// La fenêtre du dessus est en plein écran : la barre (couche « top ») s'efface.
    pub fn top_is_fullscreen(&self) -> bool {
        self.space
            .elements()
            .last()
            .and_then(|w| w.toplevel())
            .is_some_and(|t| self.fullscreen.contains(&t.wl_surface().id()))
    }

    /// Fenêtre qui a le focus clavier.
    pub fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &focus))
            .cloned()
    }

    /// Passe la fenêtre au premier plan et lui donne le clavier.
    pub fn focus_window(&mut self, window: &Window) {
        self.space.raise_element(window, true);
        // Écran verrouillé : le clavier reste au verrou
        if self.is_locked() {
            return;
        }
        let surface = window.toplevel().map(|t| t.wl_surface().clone());
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        self.space.elements().for_each(|w| {
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        });
    }

    /// Mod+Tab : la fenêtre du dessous passe au premier plan (cycle).
    pub fn cycle_windows(&mut self) {
        let bottom = self.space.elements().next().cloned();
        if self.space.elements().count() > 1
            && let Some(window) = bottom
        {
            self.focus_window(&window);
        }
    }

    /// Agrandir à la zone utilisable (hors barre), ou restaurer.
    pub fn toggle_maximize(&mut self, window: &Window) {
        if self.restore_window(window) {
            return;
        }
        if let Some(area) = self.usable_area() {
            self.fill_area(window, area, State::Maximized);
        }
    }

    /// Plein écran (toute la sortie, barre comprise) ou restauration.
    pub fn set_fullscreen(&mut self, window: &Window, fullscreen: bool) {
        if !fullscreen {
            self.restore_window(window);
            return;
        }
        let area = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o));
        if let Some(area) = area {
            self.restore_window(window);
            self.fill_area(window, area, State::Fullscreen);
        }
    }

    pub fn is_maximized_or_fullscreen(&self, window: &Window) -> bool {
        window
            .toplevel()
            .is_some_and(|t| self.restore.contains_key(&t.wl_surface().id()))
    }

    fn fill_area(&mut self, window: &Window, area: Rectangle<i32, Logical>, state: State) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let Some(loc) = self.space.element_location(window) else {
            return;
        };
        self.restore.insert(
            toplevel.wl_surface().id(),
            Rectangle::new(loc, window.geometry().size),
        );
        if state == State::Fullscreen {
            self.fullscreen.insert(toplevel.wl_surface().id());
        }
        let from = self.visual_rect(window, loc);
        toplevel.with_pending_state(|s| {
            s.states.set(state);
            s.size = Some(area.size);
        });
        toplevel.send_pending_configure();
        self.space.map_element(window.clone(), area.loc, true);
        self.animate_to(window, from, area);
    }

    /// Lance le trajet animé d'une fenêtre vers `to`.
    pub fn animate_to(
        &mut self,
        window: &Window,
        from: Rectangle<f64, Logical>,
        to: Rectangle<i32, Logical>,
    ) {
        if let Some(t) = window.toplevel() {
            self.motions.insert(
                t.wl_surface().id(),
                crate::anim::Motion {
                    from,
                    to: to.to_f64(),
                    start: std::time::Instant::now(),
                },
            );
        }
    }

    /// Rend à la fenêtre sa taille et sa place d'avant ; faux si elle n'était ni agrandie ni en plein écran.
    pub fn restore_window(&mut self, window: &Window) -> bool {
        let Some(toplevel) = window.toplevel().cloned() else {
            return false;
        };
        let Some(rect) = self.restore.remove(&toplevel.wl_surface().id()) else {
            return false;
        };
        self.fullscreen.remove(&toplevel.wl_surface().id());
        let from = self
            .space
            .element_location(window)
            .map(|loc| self.visual_rect(window, loc))
            .unwrap_or_else(|| rect.to_f64());
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
            s.size = Some(rect.size);
        });
        toplevel.send_pending_configure();
        self.space.map_element(window.clone(), rect.loc, true);
        self.animate_to(window, from, rect);
        true
    }

    /// Mod+clic droit glissé : redimensionner depuis le coin le plus proche du pointeur.
    pub fn start_mod_resize(&mut self, window: Window, button: u32, serial: Serial) {
        let Some(loc) = self.space.element_location(&window) else {
            return;
        };
        let rect = Rectangle::new(loc, window.geometry().size);
        let pos = self.seat.get_pointer().unwrap().current_location();
        let center = rect.to_f64().loc + rect.to_f64().size.downscale(2.0);
        let horizontal = if pos.x < center.x {
            ResizeEdge::LEFT
        } else {
            ResizeEdge::RIGHT
        };
        let vertical = if pos.y < center.y {
            ResizeEdge::TOP
        } else {
            ResizeEdge::BOTTOM
        };
        self.start_resize(window, horizontal | vertical, button, serial);
    }

    /// Fenêtre dont un bord (ou un coin) est sous le pointeur, juste à
    /// l'extérieur : pas sur une fenêtre, ni sur une fenêtre agrandie
    pub fn edge_under(&self, pos: Point<f64, Logical>) -> Option<(Window, ResizeEdge)> {
        if self.space.element_under(pos).is_some() || self.top_is_fullscreen() {
            return None;
        }
        // Du dessus vers le dessous : la première fenêtre dont la bande contient le pointeur
        self.space.elements().rev().find_map(|window| {
            if self.is_maximized_or_fullscreen(window) {
                return None;
            }
            let loc = self.space.element_location(window)?;
            let r = Rectangle::new(loc, window.geometry().size).to_f64();
            let (left, top) = (r.loc.x, r.loc.y);
            let (right, bottom) = (left + r.size.w, top + r.size.h);
            let inside_band = pos.x >= left - EDGE_GRAB
                && pos.x < right + EDGE_GRAB
                && pos.y >= top - EDGE_GRAB
                && pos.y < bottom + EDGE_GRAB;
            if !inside_band {
                return None;
            }
            // Près d'un coin (moins de EDGE_CORNER), les deux côtés ; sinon un seul
            let mut edges = ResizeEdge::empty();
            if pos.x < left + EDGE_CORNER {
                edges |= ResizeEdge::LEFT;
            } else if pos.x >= right - EDGE_CORNER {
                edges |= ResizeEdge::RIGHT;
            }
            if pos.y < top + EDGE_CORNER {
                edges |= ResizeEdge::TOP;
            } else if pos.y >= bottom - EDGE_CORNER {
                edges |= ResizeEdge::BOTTOM;
            }
            (!edges.is_empty()).then(|| (window.clone(), edges))
        })
    }

    /// Redimensionne `window` par ses bords `edges`, tant que le bouton est tenu
    pub fn start_resize(&mut self, window: Window, edges: ResizeEdge, button: u32, serial: Serial) {
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        if self.is_maximized_or_fullscreen(&window) {
            self.restore_window(&window);
        }
        let Some(loc) = self.space.element_location(&window) else {
            return;
        };
        let rect = Rectangle::new(loc, window.geometry().size);
        let pointer = self.seat.get_pointer().unwrap();
        let pos = pointer.current_location();

        toplevel.with_pending_state(|s| {
            s.states.set(State::Resizing);
        });
        toplevel.send_pending_configure();

        let start_data = GrabStartData {
            focus: None,
            button,
            location: pos,
        };
        let grab = ResizeSurfaceGrab::start(start_data, window, edges, rect);
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }
}
