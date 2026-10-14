//! Dessin d'une notification : carte arrondie, icône, titre, heure, texte,
//! jauge éventuelle et boutons d'action. Partagé par les bannières et le
//! centre de notifications. Dimensions en pixels logiques (× échelle).

use chrono::Local;
use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba, icon},
};

use crate::model::Notification;

pub const CARD_W: f32 = 344.0;
const PAD: f32 = 12.0;
const ICON: f32 = 36.0;
const GAP: f32 = 10.0;
const LINE: f32 = 18.0;
const BUTTONS_H: f32 = 32.0;
const PROGRESS_H: f32 = 18.0;
const CLOSE_R: f32 = 10.0;

pub struct Fonts {
    pub regular: Text,
    pub bold: Text,
    pub icons: Text,
}

/// Zones cliquables d'une carte dessinée, en pixels logiques de la surface
pub struct Hit {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Bouton de fermeture (visible au survol)
    pub close: (f32, f32),
    /// Boutons d'action : clé et zone horizontale [x0, x1] sur la rangée du bas
    pub buttons: Vec<(String, f32, f32)>,
    pub buttons_y: f32,
}

impl Hit {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn on_close(&self, x: f32, y: f32) -> bool {
        let (cx, cy) = self.close;
        (x - cx).powi(2) + (y - cy).powi(2) <= (CLOSE_R + 3.0).powi(2)
    }

    pub fn button_at(&self, x: f32, y: f32) -> Option<&str> {
        if y < self.buttons_y || y >= self.y + self.h {
            return None;
        }
        self.buttons
            .iter()
            .find(|(_, x0, x1)| x >= *x0 && x < *x1)
            .map(|(k, _, _)| k.as_str())
    }
}

fn title(n: &Notification) -> &str {
    if n.summary.is_empty() {
        &n.app_name
    } else {
        &n.summary
    }
}

/// Taille de l'icône : carrée, ou vraie vignette pour une image large
/// (capture d'écran, photo) comme sur macOS.
fn icon_size(n: &Notification) -> (f32, f32) {
    match n.icon.as_ref() {
        Some(img) if img.width as f32 >= img.height as f32 * 1.25 => {
            let w = 76.0;
            (
                w,
                (w * img.height as f32 / img.width.max(1) as f32).clamp(28.0, 52.0),
            )
        }
        _ => (ICON, ICON),
    }
}

fn text_width(n: &Notification) -> f32 {
    CARD_W - PAD - icon_size(n).0 - GAP - PAD
}

/// Lignes de texte du corps (à l'échelle 1, pour la mise en page).
fn body_lines(fonts: &Fonts, n: &Notification, max_lines: usize) -> Vec<String> {
    if n.body.is_empty() {
        return Vec::new();
    }
    fonts.regular.wrap(&n.body, 13.0, text_width(n), max_lines)
}

/// Hauteur logique d'une carte
pub fn height(fonts: &Fonts, n: &Notification, max_lines: usize) -> f32 {
    let lines = body_lines(fonts, n, max_lines).len() as f32;
    let progress = if n.value.is_some() { PROGRESS_H } else { 0.0 };
    let content = (LINE + lines * LINE + progress).max(icon_size(n).1);
    let buttons = if n.buttons.is_empty() { 0.0 } else { BUTTONS_H };
    PAD + content + PAD + buttons
}

/// « maintenant », « il y a 5 min », ou l'heure
fn when(n: &Notification) -> String {
    let minutes = (Local::now() - n.time).num_minutes();
    match minutes {
        ..1 => "maintenant".into(),
        1..60 => format!("il y a {minutes} min"),
        _ => n.time.format("%H:%M").to_string(),
    }
}

/// Couleur d'une pastille d'application, stable pour un même nom
fn app_color(name: &str) -> Rgba {
    const COLORS: [u32; 6] = [0x8aadf4, 0xa6e3a1, 0xf5a97f, 0xc6a0f6, 0x8bd5ca, 0xeed49f];
    let h = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    Rgba::hex(COLORS[(h % COLORS.len() as u32) as usize], 1.0)
}

/// Dessine la carte en (x, y) logiques ; renvoie ses zones cliquables.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Canvas,
    fonts: &Fonts,
    n: &Notification,
    x: f32,
    y: f32,
    s: f32,
    hovered: bool,
    max_lines: usize,
) -> Hit {
    let h = height(fonts, n, max_lines);
    let (px, py, pw, ph) = (x * s, y * s, CARD_W * s, h * s);
    let radius = theme::PANEL_RADIUS * s;
    canvas.shadow(px, py + 4.0 * s, pw, ph, radius, 18.0 * s, 0.35);
    canvas.fill_rounded(px, py, pw, ph, radius, theme::PANEL);
    let border = if n.critical {
        Rgba::hex(0xf38ba8, 0.7)
    } else {
        theme::PANEL_BORDER
    };
    canvas.stroke_rounded(px, py, pw, ph, radius, s.max(1.0), border);

    // Icône : image, ou pastille à l'initiale de l'application (ou glyphe du volume)
    let (ix, iy) = (px + PAD * s, py + PAD * s);
    let isz = ICON * s;
    let (iw, ih) = icon_size(n);
    if let Some(img) = n.icon.as_ref() {
        canvas.image(img, ix, iy, iw * s, ih * s, 6.0 * s);
    } else if n.synchronous.as_deref() == Some("volume") {
        canvas.fill_rounded(ix, iy, isz, isz, 8.0 * s, theme::TILE_HOVER);
        let glyph = match n.value {
            Some(0) => icon::VOLUME_MUTE,
            Some(v) if v < 34 => icon::VOLUME_0,
            Some(v) if v < 67 => icon::VOLUME_1,
            _ => icon::VOLUME_2,
        };
        fonts.icons.icon(
            canvas,
            glyph,
            ix + isz / 2.0,
            iy + isz / 2.0,
            20.0 * s,
            theme::TEXT,
        );
    } else {
        let color = app_color(&n.app_name);
        canvas.fill_rounded(ix, iy, isz, isz, 8.0 * s, color);
        let initial: String = n
            .app_name
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "•".into());
        fonts.bold.draw_centered(
            canvas,
            &initial,
            ix + isz / 2.0,
            iy,
            isz,
            17.0 * s,
            theme::BACKGROUND,
        );
    }

    // Titre et heure
    let tx = px + (PAD + iw + GAP) * s;
    let tw = text_width(n) * s;
    let mut ty = py + PAD * s;
    let time = when(n);
    let time_w = fonts.regular.measure(&time, theme::FONT_SMALL * s);
    fonts.regular.draw_in(
        canvas,
        &time,
        tx + tw - time_w,
        ty,
        LINE * s,
        theme::FONT_SMALL * s,
        theme::TEXT_DIM,
    );
    let t = fonts
        .bold
        .truncate(title(n), theme::FONT_SIZE * s, tw - time_w - 8.0 * s);
    fonts.bold.draw_in(
        canvas,
        &t,
        tx,
        ty,
        LINE * s,
        theme::FONT_SIZE * s,
        theme::TEXT,
    );
    ty += LINE * s;

    // Corps (mis en page à l'échelle 1 puis dessiné à l'échelle)
    for line in body_lines(fonts, n, max_lines) {
        fonts.regular.draw_in(
            canvas,
            &line,
            tx,
            ty,
            LINE * s,
            13.0 * s,
            Rgba::hex(0xc8c8d8, 1.0),
        );
        ty += LINE * s;
    }

    // Jauge (volume…)
    if let Some(v) = n.value {
        let bar_y = ty + 6.0 * s;
        let bar_h = 6.0 * s;
        canvas.fill_rounded(tx, bar_y, tw, bar_h, bar_h / 2.0, theme::TILE_HOVER);
        let filled = tw * v as f32 / 100.0;
        if filled > 0.0 {
            canvas.fill_rounded(
                tx,
                bar_y,
                filled.max(bar_h),
                bar_h,
                bar_h / 2.0,
                theme::TEXT,
            );
        }
    }

    // Boutons d'action
    let mut buttons = Vec::new();
    let buttons_y = y + h - BUTTONS_H;
    if !n.buttons.is_empty() {
        let by = py + ph - BUTTONS_H * s;
        canvas.fill_rounded(
            px + PAD * s,
            by,
            pw - 2.0 * PAD * s,
            s.max(1.0),
            0.0,
            theme::PANEL_BORDER,
        );
        let bw = CARD_W / n.buttons.len() as f32;
        for (i, (key, label)) in n.buttons.iter().enumerate() {
            let bx = x + i as f32 * bw;
            if i > 0 {
                canvas.fill_rounded(
                    bx * s,
                    by + 6.0 * s,
                    s.max(1.0),
                    (BUTTONS_H - 12.0) * s,
                    0.0,
                    theme::PANEL_BORDER,
                );
            }
            let label = fonts.bold.truncate(label, 13.0 * s, (bw - 16.0) * s);
            fonts.bold.draw_centered(
                canvas,
                &label,
                (bx + bw / 2.0) * s,
                by,
                BUTTONS_H * s,
                13.0 * s,
                theme::ACCENT,
            );
            buttons.push((key.clone(), bx, bx + bw));
        }
    }

    // Fermeture : pastille en haut à gauche, au survol (comme macOS)
    let close = (x + 2.0, y + 2.0);
    if hovered {
        let (cx, cy) = (close.0 * s, close.1 * s);
        canvas.fill_circle(cx, cy, CLOSE_R * s, Rgba::hex(0x45475a, 1.0));
        canvas.stroke_rounded(
            cx - CLOSE_R * s,
            cy - CLOSE_R * s,
            2.0 * CLOSE_R * s,
            2.0 * CLOSE_R * s,
            CLOSE_R * s,
            s.max(1.0),
            theme::PANEL_BORDER,
        );
        fonts
            .icons
            .icon(canvas, icon::CLOSE, cx, cy, 12.0 * s, theme::TEXT);
    }

    Hit {
        id: n.id,
        x,
        y,
        w: CARD_W,
        h,
        close,
        buttons,
        buttons_y,
    }
}
