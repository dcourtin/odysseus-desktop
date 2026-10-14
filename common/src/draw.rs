//! Dessin logiciel dans un tampon wl_shm ARGB8888 (alpha prémultiplié,
//! octets B, G, R, A en mémoire), partagé par la barre et le lanceur.

use crate::theme::Rgba;
use fontdue::{Font, FontSettings};

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

impl Canvas<'_> {
    pub fn fill(&mut self, c: Rgba) {
        let px = premultiply(c, 1.0);
        for chunk in self.buf.as_chunks_mut::<4>().0 {
            *chunk = px;
        }
    }

    /// Tout transparent (surfaces aux formes non rectangulaires).
    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    /// Rectangle aux coins arrondis, anticrénelé (distance signée à une boîte arrondie).
    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, c: Rgba) {
        let r = radius.min(w / 2.0).min(h / 2.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            for px in x.floor() as i32..(x + w).ceil() as i32 {
                let dx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
                let dy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() - r;
                self.blend(px, py, c, (0.5 - dist).clamp(0.0, 1.0));
            }
        }
    }

    /// Ombre douce autour d'un rectangle arrondi : `blur` pixels de dégradé.
    #[allow(clippy::too_many_arguments)]
    pub fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, blur: f32, alpha: f32) {
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let r = radius.min(w / 2.0).min(h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        let black = Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: alpha,
        };
        for py in (y - blur).floor() as i32..(y + h + blur).ceil() as i32 {
            for px in (x - blur).floor() as i32..(x + w + blur).ceil() as i32 {
                let dx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
                let dy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() - r;
                let k = (1.0 - (dist / blur).clamp(0.0, 1.0)).powf(2.5);
                self.blend(px, py, black, k);
            }
        }
    }

    /// Mélange « source par-dessus » d'un pixel avec une couverture 0.0–1.0.
    fn blend(&mut self, x: i32, y: i32, c: Rgba, coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height || coverage <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.width + x as u32) * 4) as usize;
        let src = premultiply(c, coverage);
        let inv = 1.0 - src[3] as f32 / 255.0;
        for (dst, s) in self.buf[i..i + 4].iter_mut().zip(src) {
            *dst = (s as f32 + *dst as f32 * inv).round() as u8;
        }
    }
}

fn premultiply(c: Rgba, coverage: f32) -> [u8; 4] {
    let a = c.a * coverage;
    [
        (c.b * a * 255.0).round() as u8,
        (c.g * a * 255.0).round() as u8,
        (c.r * a * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

pub struct Text {
    font: Font,
}

impl Text {
    pub fn load(path: &str) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| format!("{path} : {e}"))?;
        let font = Font::from_bytes(data, FontSettings::default())?;
        Ok(Self { font })
    }

    /// Largeur en pixels d'une ligne de texte à la taille `px`.
    pub fn measure(&self, s: &str, px: f32) -> f32 {
        s.chars()
            .map(|ch| self.font.metrics(ch, px).advance_width)
            .sum()
    }

    /// `s` raccourci avec « … » pour tenir dans `max_w` pixels.
    pub fn truncate(&self, s: &str, px: f32, max_w: f32) -> String {
        if self.measure(s, px) <= max_w {
            return s.to_string();
        }
        let ellipsis = self.measure("…", px);
        let mut out = String::new();
        let mut w = 0.0;
        for ch in s.chars() {
            let cw = self.font.metrics(ch, px).advance_width;
            if w + cw + ellipsis > max_w {
                break;
            }
            w += cw;
            out.push(ch);
        }
        out.push('…');
        out
    }

    /// Dessine `s` à partir de `x`, centré verticalement dans le canevas.
    pub fn draw(&self, canvas: &mut Canvas, s: &str, x: f32, px: f32, color: Rgba) {
        let h = canvas.height as f32;
        self.draw_in(canvas, s, x, 0.0, h, px, color);
    }

    /// Dessine `s` à partir de `x`, centré verticalement dans la bande [y, y + h].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_in(
        &self,
        canvas: &mut Canvas,
        s: &str,
        x: f32,
        y: f32,
        h: f32,
        px: f32,
        color: Rgba,
    ) {
        let line = self
            .font
            .horizontal_line_metrics(px)
            .expect("police sans métriques horizontales");
        let baseline = (y + (h + line.ascent + line.descent) / 2.0).round();
        let mut pen = x;
        for ch in s.chars() {
            let (m, coverage) = self.font.rasterize(ch, px);
            let left = (pen + m.xmin as f32).round() as i32;
            let top = (baseline - m.height as f32 - m.ymin as f32) as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = coverage[row * m.width + col] as f32 / 255.0;
                    canvas.blend(left + col as i32, top + row as i32, color, a);
                }
            }
            pen += m.advance_width;
        }
    }
}
