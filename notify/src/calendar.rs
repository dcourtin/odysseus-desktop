//! Petit calendrier du mois en tête du centre de notifications : date du
//! jour en titre, grille lundi → dimanche, aujourd'hui en pastille.

use chrono::{Datelike, Local, NaiveDate};
use common::{
    draw::Canvas,
    theme::{self, Rgba},
};

use crate::card::Fonts;

const JOURS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MOIS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const INITIALES: [&str; 7] = ["L", "M", "M", "J", "V", "S", "D"];
const PAD: f32 = 14.0;
const CELL_H: f32 = 26.0;

/// Dessine le calendrier en (x, y) sur `w` pixels logiques ; renvoie sa hauteur.
pub fn draw(canvas: &mut Canvas, fonts: &Fonts, x: f32, y: f32, w: f32, s: f32) -> f32 {
    let today = Local::now().date_naive();
    let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let offset = first.weekday().num_days_from_monday() as usize;
    let days = days_in_month(today.year(), today.month());
    let rows = (offset + days as usize).div_ceil(7);
    let h = PAD + 22.0 + 18.0 + 20.0 + rows as f32 * CELL_H + PAD;

    canvas.shadow(
        x * s,
        (y + 4.0) * s,
        w * s,
        h * s,
        theme::PANEL_RADIUS * s,
        18.0 * s,
        0.3,
    );
    canvas.fill_rounded(
        x * s,
        y * s,
        w * s,
        h * s,
        theme::PANEL_RADIUS * s,
        theme::PANEL,
    );
    canvas.stroke_rounded(
        x * s,
        y * s,
        w * s,
        h * s,
        theme::PANEL_RADIUS * s,
        s.max(1.0),
        theme::PANEL_BORDER,
    );

    // Titre : « mercredi 14 octobre », puis le mois et l'année
    let mut cy = y + PAD;
    let title = format!(
        "{} {} {}",
        JOURS[today.weekday().num_days_from_monday() as usize],
        today.day(),
        MOIS[today.month0() as usize]
    );
    fonts.bold.draw_in(
        canvas,
        &title,
        (x + PAD) * s,
        cy * s,
        22.0 * s,
        theme::FONT_TITLE * s,
        theme::TEXT,
    );
    cy += 22.0;
    let month = format!(
        "{} {}",
        capitalize(MOIS[today.month0() as usize]),
        today.year()
    );
    fonts.regular.draw_in(
        canvas,
        &month,
        (x + PAD) * s,
        cy * s,
        18.0 * s,
        theme::FONT_SMALL * s,
        theme::ACCENT,
    );
    cy += 18.0;

    let cell_w = (w - 2.0 * PAD) / 7.0;
    for (i, d) in INITIALES.iter().enumerate() {
        let cx = x + PAD + (i as f32 + 0.5) * cell_w;
        fonts.bold.draw_centered(
            canvas,
            d,
            cx * s,
            cy * s,
            20.0 * s,
            11.0 * s,
            theme::TEXT_DIM,
        );
    }
    cy += 20.0;
    for day in 1..=days {
        let index = offset + day as usize - 1;
        let (col, row) = (index % 7, index / 7);
        let cx = x + PAD + (col as f32 + 0.5) * cell_w;
        let top = cy + row as f32 * CELL_H;
        let label = day.to_string();
        let color = if day == today.day() {
            canvas.fill_circle(cx * s, (top + CELL_H / 2.0) * s, 11.0 * s, theme::ACCENT);
            theme::BACKGROUND
        } else if col >= 5 {
            Rgba::hex(0x9a9ab0, 1.0)
        } else {
            theme::TEXT
        };
        fonts
            .regular
            .draw_centered(canvas, &label, cx * s, top * s, CELL_H * s, 12.0 * s, color);
    }
    h
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(30)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
