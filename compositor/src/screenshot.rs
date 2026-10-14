//! Captures d'écran, à la manière de macOS :
//! - Impr. écran ou Super+Maj+S : sélection d'une zone (l'écran s'assombrit,
//!   on trace un rectangle, ses dimensions s'affichent ; Échap annule) ;
//!   Espace bascule en mode fenêtre (la fenêtre survolée est surlignée, un
//!   clic la capture avec son ombre, sur fond transparent) ;
//! - Maj+Impr. écran : l'écran entier, aussitôt.
//!
//! La capture est enregistrée dans ~/Images (« Capture d’écran AAAA-MM-JJ à
//! HH.MM.SS.png »), copiée dans le presse-papiers (image/png) et annoncée par
//! une notification avec vignette (un clic l'ouvre dans Aperçu). Un bref
//! éclair blanc confirme la prise.

use std::{sync::Arc, time::Instant};

use smithay::{
    desktop::Window,
    input::keyboard::Keysym,
    utils::{Logical, Point, Rectangle},
};

use crate::Odysseus;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Area,
    Window,
}

/// Sélection en cours (avant la capture)
pub struct Selection {
    pub mode: Mode,
    /// Coin de départ du rectangle (bouton appuyé)
    pub anchor: Option<Point<f64, Logical>>,
    pub pointer: Point<f64, Logical>,
}

impl Selection {
    /// Rectangle tracé, en pixels logiques
    pub fn rect(&self) -> Option<Rectangle<i32, Logical>> {
        let a = self.anchor?;
        let p = self.pointer;
        let (x0, y0) = (a.x.min(p.x), a.y.min(p.y));
        let (x1, y1) = (a.x.max(p.x), a.y.max(p.y));
        Some(Rectangle::new(
            (x0.round() as i32, y0.round() as i32).into(),
            ((x1 - x0).round() as i32, (y1 - y0).round() as i32).into(),
        ))
    }
}

/// Capture à faire à la prochaine image (par le backend)
pub enum Request {
    Full,
    Area(Rectangle<i32, Logical>),
    Window(Window),
}

/// Données mises dans le presse-papiers par le compositeur
pub type ClipboardData = Arc<Vec<u8>>;

impl Odysseus {
    pub fn start_selection(&mut self) {
        let pointer = self.seat.get_pointer().unwrap().current_location();
        self.selection = Some(Selection {
            mode: Mode::Area,
            anchor: None,
            pointer,
        });
    }

    pub fn capture_full(&mut self) {
        self.capture_request = Some(Request::Full);
    }

    /// Touche pendant la sélection ; vrai si elle est consommée.
    pub fn selection_key(&mut self, sym: Keysym) -> bool {
        let Some(sel) = self.selection.as_mut() else {
            return false;
        };
        match sym {
            Keysym::Escape => self.selection = None,
            Keysym::space => {
                sel.mode = match sel.mode {
                    Mode::Area => Mode::Window,
                    Mode::Window => Mode::Area,
                };
                sel.anchor = None;
            }
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(rect) = sel.rect() {
                    self.selection = None;
                    self.capture_request = Some(Request::Area(rect));
                }
            }
            _ => {}
        }
        true
    }

    pub fn selection_motion(&mut self, pos: Point<f64, Logical>) {
        if let Some(sel) = self.selection.as_mut() {
            sel.pointer = pos;
        }
    }

    /// Bouton pendant la sélection : début / fin du tracé, ou clic sur une fenêtre.
    pub fn selection_button(&mut self, pressed: bool) {
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        match (sel.mode, pressed) {
            (Mode::Area, true) => sel.anchor = Some(sel.pointer),
            (Mode::Area, false) => {
                let rect = sel.rect();
                self.selection = None;
                match rect {
                    // Un simple clic sans tracé : rien
                    Some(r) if r.size.w > 3 && r.size.h > 3 => {
                        self.capture_request = Some(Request::Area(r));
                    }
                    _ => {}
                }
            }
            (Mode::Window, true) => {
                let pos = sel.pointer;
                let window = self.space.element_under(pos).map(|(w, _)| w.clone());
                self.selection = None;
                if let Some(window) = window {
                    self.capture_request = Some(Request::Window(window));
                }
            }
            (Mode::Window, false) => {}
        }
    }

    /// Fenêtre surlignée en mode fenêtre
    pub fn selection_window_rect(&self) -> Option<Rectangle<i32, Logical>> {
        let sel = self.selection.as_ref()?;
        if sel.mode != Mode::Window {
            return None;
        }
        let (window, loc) = self.space.element_under(sel.pointer)?;
        Some(Rectangle::new(loc, window.geometry().size))
    }

    /// Après l'enregistrement : presse-papiers, notification, éclair.
    pub fn capture_done(&mut self, path: std::path::PathBuf, png: Vec<u8>) {
        tracing::info!("capture : {}", path.display());
        self.flash = Some(Instant::now());
        smithay::wayland::selection::data_device::set_data_device_selection(
            &self.display_handle,
            &self.seat,
            vec!["image/png".into()],
            Arc::new(png),
        );
        let p = path.to_string_lossy().replace('\'', "'\\''");
        let cmd = format!(
            "--send 'Capture d’écran' 'Enregistrée dans Images et copiée dans le presse-papiers' --image '{p}' --open '{p}'"
        );
        self.spawn(&self.sibling_command(&format!("odysseus-notify {cmd}")));
    }
}

/// Nom de fichier à la manière de macOS
pub fn file_name() -> String {
    chrono::Local::now()
        .format("Capture d’écran %Y-%m-%d à %H.%M.%S.png")
        .to_string()
}
