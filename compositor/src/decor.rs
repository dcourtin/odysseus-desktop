//! Habillage et réduction des fenêtres.
//!
//! - **Barre de titre** : dessinée par le compositeur au-dessus des fenêtres
//!   qui lui laissent la décoration (mode « côté serveur » : terminal, applis
//!   tierces). Les applis d'odysseus dessinent leurs propres boutons
//!   (décoration côté client) ; GTK la dessine aussi lui-même. La barre ne
//!   fait pas partie de la surface du client : la fenêtre est placée dessous,
//!   et agrandir, aimanter ou centrer en tiennent compte (`inset`).
//! - **Boutons**, dans le ton d'odysseus : trois ronds gris (fermer, réduire,
//!   agrandir) dont les symboles apparaissent au survol ; fermer prend la
//!   couleur d'accent. Alt+clic sur agrandir : plein écran. Double-clic sur
//!   la barre : agrandir ; glisser : déplacer.
//! - **Réduire** : la fenêtre quitte l'affichage et attend, avec son bureau et
//!   sa place, qu'on la rende (Dock, IPC « focus <id> », Super+Tab).

use std::{cell::RefCell, collections::HashMap, time::Instant};

use common::{
    draw::{Canvas, Text},
    theme::{self, icon},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::GlesRenderer,
        },
    },
    desktop::Window,
    input::pointer::{CursorImageStatus, Focus, GrabStartData},
    reexports::wayland_server::{Resource, backend::ObjectId},
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform},
};

use crate::{Odysseus, grabs::MoveSurfaceGrab, render::OdysseusElement};

/// Hauteur de la barre de titre (pixels logiques)
pub const TITLE_H: i32 = 30;
/// Centres des trois boutons depuis le bord gauche, rayon, zone de clic
const BUTTONS: [f64; 3] = [17.0, 37.0, 57.0];
const BUTTON_R: f64 = 6.0;
const BUTTON_HIT: f64 = 10.0;

/// Partie de la barre de titre
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TitlePart {
    Close,
    Minimize,
    Maximize,
    Bar,
}

/// Fenêtre réduite : son bureau (0 = bureau 1) et sa place
pub struct Minimized {
    pub window: Window,
    pub loc: Point<i32, Logical>,
    pub workspace: usize,
}

fn id_of(window: &Window) -> Option<ObjectId> {
    window.toplevel().map(|t| t.wl_surface().id())
}

impl Odysseus {
    /// Hauteur réservée au-dessus de la fenêtre pour sa barre de titre (0 sans barre)
    pub fn inset(&self, window: &Window) -> i32 {
        match id_of(window) {
            Some(id) if self.ssd.contains(&id) && !self.fullscreen.contains(&id) => TITLE_H,
            _ => 0,
        }
    }

    /// Fenêtres qui ont une barre de titre en ce moment (pour le rendu)
    pub fn titled(&self) -> std::collections::HashSet<ObjectId> {
        self.ssd
            .iter()
            .filter(|id| !self.fullscreen.contains(id))
            .cloned()
            .collect()
    }

    /// Zone donnée à une fenêtre (agrandie, aimantée) → place de sa surface, sous la barre
    pub fn client_rect(
        &self,
        window: &Window,
        area: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let t = self.inset(window);
        Rectangle::new(
            (area.loc.x, area.loc.y + t).into(),
            (area.size.w, (area.size.h - t).max(1)).into(),
        )
    }

    /// Fenêtre (barre comprise) à sa place actuelle
    pub fn frame_rect(&self, window: &Window, loc: Point<i32, Logical>) -> Rectangle<i32, Logical> {
        let t = self.inset(window);
        let size = window.geometry().size;
        Rectangle::new((loc.x, loc.y - t).into(), (size.w, size.h + t).into())
    }

    /// Barre de titre sous le pointeur (et la partie visée), si aucune fenêtre ne la couvre
    pub fn titlebar_under(&self, pos: Point<f64, Logical>) -> Option<(Window, TitlePart)> {
        for window in self.space.elements().rev() {
            let loc = self.space.element_location(window)?;
            let t = self.inset(window);
            let size = window.geometry().size;
            if t > 0 {
                let bar = Rectangle::new((loc.x, loc.y - t).into(), (size.w, t).into()).to_f64();
                if bar.contains(pos) {
                    let (x, y) = (pos.x - bar.loc.x, pos.y - bar.loc.y - t as f64 / 2.0);
                    let part = [TitlePart::Close, TitlePart::Minimize, TitlePart::Maximize]
                        .into_iter()
                        .zip(BUTTONS)
                        .find(|(_, cx)| (x - cx).hypot(y) <= BUTTON_HIT)
                        .map_or(TitlePart::Bar, |(p, _)| p);
                    return Some((window.clone(), part));
                }
            }
            // Fenêtre au-dessus qui couvre ce point : rien dessous
            if Rectangle::new(loc, size).to_f64().contains(pos) {
                return None;
            }
        }
        None
    }

    /// Clic gauche sur une barre de titre
    pub fn titlebar_click(&mut self, window: Window, part: TitlePart, serial: Serial, alt: bool) {
        self.focus_window(&window);
        match part {
            TitlePart::Close => {
                if let Some(t) = window.toplevel() {
                    t.send_close();
                }
            }
            TitlePart::Minimize => self.minimize(&window),
            TitlePart::Maximize if alt => {
                let full = id_of(&window).is_some_and(|id| self.fullscreen.contains(&id));
                self.set_fullscreen(&window, !full);
            }
            TitlePart::Maximize => self.toggle_maximize(&window),
            TitlePart::Bar => {
                let id = id_of(&window);
                let double = self.last_title_click.as_ref().is_some_and(|(last, at)| {
                    Some(last) == id.as_ref() && at.elapsed().as_millis() < 400
                });
                if double {
                    self.last_title_click = None;
                    self.toggle_maximize(&window);
                    return;
                }
                self.last_title_click = id.map(|id| (id, Instant::now()));
                // Glisser la barre : déplacer la fenêtre
                let pointer = self.seat.get_pointer().unwrap();
                let Some(initial_window_location) = self.space.element_location(&window) else {
                    return;
                };
                let grab = MoveSurfaceGrab {
                    start_data: GrabStartData {
                        focus: None,
                        button: 0x110,
                        location: pointer.current_location(),
                    },
                    window,
                    initial_window_location,
                };
                pointer.set_grab(self, grab, serial, Focus::Clear);
            }
        }
    }

    /// Survol d'une barre de titre (les symboles des boutons apparaissent) ;
    /// le curseur y reprend sa forme ordinaire
    pub fn update_title_hover(&mut self, pos: Point<f64, Logical>, nothing_under: bool) {
        let hover = if nothing_under && !self.seat.get_pointer().unwrap().is_grabbed() {
            self.titlebar_under(pos)
                .and_then(|(w, part)| id_of(&w).map(|id| (id, part)))
        } else {
            None
        };
        if hover.is_some() && matches!(self.cursor_status, CursorImageStatus::Surface(_)) {
            self.cursor_status = CursorImageStatus::default_named();
        }
        self.title_hover = hover;
    }

    // --- Réduire ------------------------------------------------------------------

    /// Réduit une fenêtre : elle disparaît en fondu et attend qu'on la rende
    pub fn minimize(&mut self, window: &Window) {
        let Some(loc) = self.space.element_location(window) else {
            return;
        };
        if let Some(context) = self.renderer_context.as_ref()
            && let Some(closing) = crate::closing::Closing::capture(window, loc, context)
        {
            self.closing.push(closing);
        }
        self.space.unmap_elem(window);
        self.minimized.push(Minimized {
            window: window.clone(),
            loc,
            workspace: self.workspaces.active,
        });
        // Le clavier passe à la fenêtre du dessus
        let top = self.space.elements().last().cloned();
        match top {
            Some(top) => self.focus_window(&top),
            None => {
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
            }
        }
        crate::ipc::broadcast_window_event(self);
    }

    /// Rend une fenêtre réduite, sur son bureau ; faux si elle ne l'était pas
    pub fn unminimize(&mut self, window: &Window) -> bool {
        let Some(i) = self.minimized.iter().position(|m| &m.window == window) else {
            return false;
        };
        let m = self.minimized.remove(i);
        if m.workspace != self.workspaces.active {
            self.switch_workspace(m.workspace);
        }
        if let Some(id) = id_of(&m.window) {
            self.opening.insert(id, Instant::now());
        }
        self.space.map_element(m.window.clone(), m.loc, true);
        self.focus_window(&m.window);
        crate::ipc::broadcast_window_event(self);
        true
    }

    /// Réduit la fenêtre active (Super+M)
    pub fn minimize_focused(&mut self) {
        if let Some(window) = self.focused_window() {
            self.minimize(&window);
        }
    }

    /// Oublie les fenêtres réduites que leur client a fermées
    pub fn refresh_minimized(&mut self) {
        let before = self.minimized.len();
        self.minimized.retain(|m| m.window.alive());
        if self.minimized.len() != before {
            crate::ipc::broadcast_window_event(self);
        }
    }
}

// --- Dessin de la barre --------------------------------------------------------

/// Ce qui change l'image d'une barre
#[derive(PartialEq, Clone)]
struct BarKey {
    w: u32,
    h: u32,
    title: String,
    focused: bool,
    hover: Option<TitlePart>,
    theme: &'static str,
}

thread_local! {
    /// Barres déjà dessinées : on ne redessine (et ne renvoie au GPU) que ce qui change
    static BARS: RefCell<HashMap<ObjectId, (BarKey, MemoryRenderBuffer)>> = RefCell::new(HashMap::new());
}

struct Fonts {
    bold: Text,
    icons: Text,
}

thread_local! {
    static FONTS: Option<Fonts> = Text::load(theme::FONT_BOLD)
        .ok()
        .zip(Text::load(theme::FONT_ICONS).ok())
        .map(|(bold, icons)| Fonts { bold, icons });
}

/// Barre de titre d'une fenêtre, posée sur `rect` (pixels logiques)
pub fn titlebar_element(
    renderer: &mut GlesRenderer,
    window: &Window,
    rect: Rectangle<f64, Logical>,
    scale: f64,
    focused: bool,
    hover: Option<TitlePart>,
    alpha: f32,
) -> Option<OdysseusElement> {
    let id = id_of(window)?;
    let (_, title) = crate::agent::app_and_title(window);
    let p = theme::palette();
    let key = BarKey {
        w: (rect.size.w * scale).round().max(1.0) as u32,
        h: (rect.size.h * scale).round().max(1.0) as u32,
        title,
        focused,
        hover,
        theme: p.id,
    };
    let (key_w, key_h) = (key.w, key.h);
    let buffer = BARS.with(|bars| {
        let mut bars = bars.borrow_mut();
        if bars.len() > 64 {
            bars.clear();
        }
        match bars.get(&id) {
            Some((k, b)) if *k == key => Some(b.clone()),
            _ => {
                let b = draw_bar(&key, scale as f32)?;
                bars.insert(id, (key.clone(), b.clone()));
                Some(b)
            }
        }
    })?;
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        rect.loc.to_physical(scale),
        &buffer,
        Some(alpha),
        // Le tampon est en pixels de l'écran : tout le tampon, ramené à la taille logique
        Some(Rectangle::new(
            (0.0, 0.0).into(),
            (key_w as f64, key_h as f64).into(),
        )),
        Some(
            (
                rect.size.w.round() as i32,
                rect.size.h.round().max(1.0) as i32,
            )
                .into(),
        ),
        Kind::Unspecified,
    )
    .ok()
    .map(OdysseusElement::CursorTheme)
}

fn draw_bar(key: &BarKey, s: f32) -> Option<MemoryRenderBuffer> {
    let p = theme::palette();
    let (w, h) = (key.w, key.h);
    let mut data = vec![0u8; (w * h * 4) as usize];
    let mut c = Canvas {
        buf: &mut data,
        width: w,
        height: h,
    };
    let (wf, hf) = (w as f32, h as f32);
    let r = theme::WINDOW_RADIUS * s;
    // Coins du haut arrondis, ceux du bas cachés sous la fenêtre
    c.fill_rounded(0.0, 0.0, wf, hf + 2.0 * r, r, p.sidebar);
    let b = p.window_border();
    c.stroke_rounded(0.0, 0.0, wf, hf + 2.0 * r, r, s, b);
    c.fill_rounded(0.0, hf - s, wf, s, 0.0, p.ink(0.06));
    FONTS.with(|fonts| {
        let fonts = fonts.as_ref()?;
        let cy = hf / 2.0;
        let over_buttons = matches!(
            key.hover,
            Some(TitlePart::Close | TitlePart::Minimize | TitlePart::Maximize)
        );
        for (part, (cx, glyph)) in [TitlePart::Close, TitlePart::Minimize, TitlePart::Maximize]
            .into_iter()
            .zip(
                BUTTONS
                    .iter()
                    .zip([icon::CLOSE, icon::MINUS, icon::MAXIMIZE_2]),
            )
        {
            let cx = *cx as f32 * s;
            let hovered = key.hover == Some(part);
            let fill = if hovered && part == TitlePart::Close {
                p.accent
            } else if hovered {
                p.ink(0.30)
            } else if key.focused {
                p.ink(0.20)
            } else {
                p.ink(0.12)
            };
            c.fill_circle(cx, cy, BUTTON_R as f32 * s, fill);
            if over_buttons {
                let color = if hovered && part == TitlePart::Close {
                    p.on_accent
                } else {
                    p.text
                };
                fonts.icons.icon(&mut c, glyph, cx, cy, 9.0 * s, color);
            }
        }
        // Titre centré, tronqué entre les boutons et le bord
        let px = 12.5 * s;
        let room = wf - 2.0 * (BUTTONS[2] as f32 + 16.0) * s;
        if room > 20.0 * s && !key.title.is_empty() {
            let title = fonts.bold.truncate(&key.title, px, room);
            let color = if key.focused {
                p.text_soft()
            } else {
                p.text_dim
            };
            fonts
                .bold
                .draw_centered(&mut c, &title, wf / 2.0, 0.0, hf, px, color);
        }
        Some(())
    })?;
    Some(MemoryRenderBuffer::from_slice(
        &data,
        Fourcc::Argb8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    ))
}
