//! Boutons des fenêtres (fermer, réduire, agrandir), dans le ton d'odysseus :
//! trois ronds gris en haut à gauche, dont les symboles apparaissent quand on
//! les survole ; fermer prend la couleur d'accent. Dessinés de la même façon
//! par le compositeur (barre de titre des applis tierces) et par les applis
//! d'odysseus (dans leur barre latérale ou leur barre d'outils).

use crate::{
    draw::{Canvas, Text},
    theme::{self, icon},
};

/// Centres des boutons depuis le bord gauche de la fenêtre (pixels logiques)
pub const CENTERS: [f32; 3] = [17.0, 37.0, 57.0];
pub const RADIUS: f32 = 6.0;
/// Rayon de la zone de clic, un peu plus large que le rond
pub const HIT: f32 = 10.0;
/// Largeur occupée, marge comprise : ce qu'une appli laisse libre à gauche
pub const WIDTH: f32 = 76.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Close,
    Minimize,
    Maximize,
}

const ALL: [Button; 3] = [Button::Close, Button::Minimize, Button::Maximize];

/// Bouton sous (x, y), relatifs au bord gauche de la fenêtre et au centre
/// vertical de la rangée (pixels logiques)
pub fn hit(x: f32, dy: f32) -> Option<Button> {
    ALL.into_iter()
        .zip(CENTERS)
        .find(|(_, cx)| (x - cx).hypot(dy) <= HIT)
        .map(|(b, _)| b)
}

/// La rangée de boutons (pour savoir si on la survole)
pub fn over_row(x: f32, dy: f32) -> bool {
    (CENTERS[0] - HIT..=CENTERS[2] + HIT).contains(&x) && dy.abs() <= HIT
}

/// Dessine les trois boutons ; `x0` : bord gauche de la fenêtre, `cy` : centre
/// de la rangée (pixels du tampon), `s` : échelle. `row_hover` : la rangée est
/// survolée (symboles visibles), `hover` : le bouton survolé.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    c: &mut Canvas,
    icons: &Text,
    x0: f32,
    cy: f32,
    s: f32,
    focused: bool,
    row_hover: bool,
    hover: Option<Button>,
) {
    let p = theme::palette();
    for (button, (cx, glyph)) in
        ALL.into_iter().zip(
            CENTERS
                .iter()
                .zip([icon::CLOSE, icon::MINUS, icon::MAXIMIZE_2]),
        )
    {
        let cx = x0 + cx * s;
        let hovered = hover == Some(button);
        let fill = if hovered && button == Button::Close {
            p.accent
        } else if hovered {
            p.ink(0.30)
        } else if focused || row_hover {
            p.ink(0.20)
        } else {
            p.ink(0.12)
        };
        c.fill_circle(cx, cy, RADIUS * s, fill);
        if row_hover || hover.is_some() {
            let color = if hovered && button == Button::Close {
                p.on_accent
            } else {
                p.text
            };
            icons.icon(c, glyph, cx, cy, 9.0 * s, color);
        }
    }
}

/// État des boutons d'une fenêtre d'appli : à brancher sur sa configuration
/// (`configure`), ses mouvements de pointeur (`motion`) et ses clics (`press`)
#[derive(Default)]
pub struct Chrome {
    pub focused: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    row_hover: bool,
    hover: Option<Button>,
}

impl Chrome {
    /// États de la fenêtre, d'après sa configuration
    pub fn configure(&mut self, focused: bool, maximized: bool, fullscreen: bool) {
        self.focused = focused;
        self.maximized = maximized;
        self.fullscreen = fullscreen;
    }

    /// Pointeur en (x, y) logiques ; `cy` : centre vertical de la rangée.
    /// Vrai si l'affichage des boutons change (redessiner).
    pub fn motion(&mut self, x: f32, y: f32, cy: f32) -> bool {
        let (row, hover) = (over_row(x, y - cy), hit(x, y - cy));
        let changed = (row, hover) != (self.row_hover, self.hover);
        self.row_hover = row;
        self.hover = hover;
        changed
    }

    /// Le pointeur quitte la fenêtre ; vrai s'il faut redessiner
    pub fn leave(&mut self) -> bool {
        let changed = self.row_hover || self.hover.is_some();
        self.row_hover = false;
        self.hover = None;
        changed
    }

    /// Bouton cliqué en (x, y) logiques
    pub fn press(&self, x: f32, y: f32, cy: f32) -> Option<Button> {
        hit(x, y - cy)
    }

    /// Dessine la rangée ; `cy` en pixels logiques, `s` : échelle
    pub fn draw(&self, c: &mut Canvas, icons: &Text, cy: f32, s: f32) {
        draw(
            c,
            icons,
            0.0,
            cy * s,
            s,
            self.focused,
            self.row_hover,
            self.hover,
        );
    }
}
