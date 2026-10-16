//! Champ de saisie d'une ligne : renommage, chemin tapé (Ctrl+L), recherche.
//! Curseur, sélection (Maj+flèches, Ctrl+A), dessin avec défilement quand le
//! texte dépasse.

use common::{
    draw::{Canvas, Text},
    theme,
};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

#[derive(Default)]
pub struct Field {
    pub text: String,
    /// Curseur et début de la sélection (octets)
    pub cursor: usize,
    pub anchor: usize,
}

/// Effet d'une touche
#[derive(PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Entrée
    Submit,
    /// Échap
    Cancel,
    /// Texte modifié
    Changed,
    /// Curseur ou sélection déplacés
    Moved,
    /// Touche sans effet ici (Tab, ↑, ↓, raccourcis…)
    Ignored,
}

impl Field {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            cursor: text.len(),
            anchor: text.len(),
        }
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn select(&mut self, from: usize, to: usize) {
        self.anchor = from.min(self.text.len());
        self.cursor = to.min(self.text.len());
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = text.len();
        self.anchor = text.len();
    }

    fn insert(&mut self, s: &str) {
        let (a, b) = self.selection();
        self.text.replace_range(a..b, s);
        self.cursor = a + s.len();
        self.anchor = self.cursor;
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().last().map_or(0, |(p, _)| p)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..]
            .chars()
            .next()
            .map_or(i, |c| i + c.len_utf8())
    }

    /// Début du mot avant `i` (Ctrl+← , Ctrl+Retour arrière)
    fn word_start(&self, i: usize) -> usize {
        let before = &self.text[..i];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed
            .rfind(|c: char| !c.is_alphanumeric())
            .map_or(0, |p| {
                p + trimmed[p..].chars().next().map_or(1, char::len_utf8)
            })
    }

    /// Fin du mot après `i` (Ctrl+→)
    fn word_end(&self, i: usize) -> usize {
        let after = &self.text[i..];
        let skip = after.len()
            - after
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .len();
        let rest = &after[skip..];
        i + skip
            + rest
                .find(|c: char| !c.is_alphanumeric())
                .unwrap_or(rest.len())
    }

    pub fn key(&mut self, event: &KeyEvent, ctrl: bool, shift: bool) -> Outcome {
        let moved = |f: &mut Self, to: usize| {
            f.cursor = to;
            if !shift {
                f.anchor = to;
            }
            Outcome::Moved
        };
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => Outcome::Submit,
            Keysym::Escape => Outcome::Cancel,
            Keysym::BackSpace => {
                if self.cursor == self.anchor {
                    self.anchor = if ctrl {
                        self.word_start(self.cursor)
                    } else {
                        self.prev(self.cursor)
                    };
                }
                self.insert("");
                Outcome::Changed
            }
            Keysym::Delete | Keysym::KP_Delete => {
                if self.cursor == self.anchor {
                    self.anchor = self.next(self.cursor);
                }
                self.insert("");
                Outcome::Changed
            }
            Keysym::Left if !shift && self.cursor != self.anchor && !ctrl => {
                let to = self.selection().0;
                moved(self, to)
            }
            Keysym::Right if !shift && self.cursor != self.anchor && !ctrl => {
                let to = self.selection().1;
                moved(self, to)
            }
            Keysym::Left => {
                let to = if ctrl {
                    self.word_start(self.cursor)
                } else {
                    self.prev(self.cursor)
                };
                moved(self, to)
            }
            Keysym::Right => {
                let to = if ctrl {
                    self.word_end(self.cursor)
                } else {
                    self.next(self.cursor)
                };
                moved(self, to)
            }
            Keysym::Home => moved(self, 0),
            Keysym::End => {
                let to = self.text.len();
                moved(self, to)
            }
            Keysym::a if ctrl => {
                self.select(0, self.text.len());
                Outcome::Moved
            }
            _ => match event.utf8.as_deref() {
                Some(t) if !ctrl && !t.is_empty() && t.chars().all(|c| !c.is_control()) => {
                    self.insert(t);
                    Outcome::Changed
                }
                _ => Outcome::Ignored,
            },
        }
    }

    /// Dessine le champ dans [x, x + w] × [y, y + h] (pixels du tampon) ;
    /// `placeholder` s'affiche quand il est vide
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        font: &Text,
        c: &mut Canvas,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        s: f32,
        placeholder: &str,
    ) {
        let p = theme::palette();
        let px = 14.0 * s;
        let pad = 8.0 * s;
        c.fill_rounded(x, y, w, h, 7.0 * s, p.content);
        c.stroke_rounded(x, y, w, h, 7.0 * s, s, p.accent);
        // Défilement : le curseur reste visible
        let inner = w - 2.0 * pad;
        let xc = font.measure(&self.text[..self.cursor], px);
        let shift = (xc - inner + 2.0 * s).max(0.0);
        let tx = x + pad - shift;
        let (a, b) = self.selection();
        if b > a {
            let xa = (tx + font.measure(&self.text[..a], px)).max(x + pad);
            let xb = (tx + font.measure(&self.text[..b], px)).min(x + w - pad);
            c.fill_rounded(
                xa,
                y + 4.0 * s,
                xb - xa,
                h - 8.0 * s,
                2.0 * s,
                p.accent.with_alpha(0.6),
            );
        }
        if self.text.is_empty() {
            font.draw_in(c, placeholder, x + pad, y, h, px, p.text_dim);
        } else {
            // Ce qui dépasse à gauche est retiré caractère par caractère
            let mut start = 0;
            let mut sx = tx;
            for (i, ch) in self.text.char_indices() {
                if sx >= x + pad - 0.5 {
                    start = i;
                    break;
                }
                sx += font.advance(ch, px);
                start = i + ch.len_utf8();
            }
            let visible = font.truncate(&self.text[start..], px, x + w - pad - sx);
            font.draw_in(c, &visible, sx, y, h, px, p.text);
        }
        c.fill_rounded(tx + xc, y + 5.0 * s, s.max(1.5), h - 10.0 * s, 0.0, p.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        let f = Field::new("mon fichier.txt");
        assert_eq!(f.word_start(f.text.len()), 12);
        assert_eq!(f.word_start(12), 4);
        assert_eq!(f.word_end(0), 3);
        assert_eq!(f.word_end(3), 11);
    }
}
