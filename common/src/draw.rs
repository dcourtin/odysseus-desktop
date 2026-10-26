//! Dessin logiciel dans un tampon wl_shm ARGB8888 (alpha prémultiplié,
//! octets B, G, R, A en mémoire), partagé par la barre et le lanceur.

use crate::theme::Rgba;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use fontdue::{Font, FontSettings, Metrics};

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
    /// Tout le tampon à l'opacité `a` (pixels prémultipliés) : fondu de sortie
    pub fn fade(&mut self, a: f32) {
        let k = (a.clamp(0.0, 1.0) * 256.0) as u32;
        for b in self.buf.iter_mut() {
            *b = ((*b as u32 * k) >> 8) as u8;
        }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    /// Rectangle aux coins arrondis, anticrénelé (distance signée à une boîte arrondie).
    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, c: Rgba) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if radius < 0.5 {
            return self.fill_rect(x, y, w, h, c);
        }
        let r = radius.min(w / 2.0).min(h / 2.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        let src = premultiply(c, 1.0);
        let (left, right) = (x.floor() as i32, (x + w).ceil() as i32);
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            let dy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
            // Pixels entièrement couverts de la ligne, remplis d'un bloc ; seuls
            // les bords (arrondis, anticrénelage) sont calculés un par un
            let ri = r - 0.5;
            let (mut a, mut b) = (right, right);
            if dy < ri {
                let half = hx + (ri * ri - dy * dy).sqrt();
                a = (cx - half - 0.5).ceil() as i32;
                b = ((cx + half - 0.5).floor() as i32 + 1).max(a);
            }
            for px in (left..a.min(right)).chain(b.max(left)..right) {
                let dx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() - r;
                self.blend(px, py, c, (0.5 - dist).clamp(0.0, 1.0));
            }
            self.fill_span(py, a.max(left), b.min(right), src);
        }
    }

    /// Pixels [x0, x1) de la ligne y, entièrement couverts (couleur prémultipliée)
    fn fill_span(&mut self, y: i32, x0: i32, x1: i32, src: [u8; 4]) {
        if y < 0 || y as u32 >= self.height {
            return;
        }
        let (x0, x1) = (
            x0.max(0) as usize,
            (x1.max(0) as usize).min(self.width as usize),
        );
        if x0 >= x1 {
            return;
        }
        let inv = 255 - src[3] as u32;
        let row = y as usize * self.width as usize * 4;
        for px in self.buf[row + x0 * 4..row + x1 * 4].as_chunks_mut::<4>().0 {
            for (dst, s) in px.iter_mut().zip(src) {
                *dst = (s as u32 + (*dst as u32 * inv + 127) / 255) as u8;
            }
        }
    }

    /// Rectangle droit (rapide : une couleur précalculée par ligne, bornes coupées).
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        let x0 = (x.round().max(0.0) as u32).min(self.width);
        let y0 = (y.round().max(0.0) as u32).min(self.height);
        let x1 = ((x + w).round().max(0.0) as u32).min(self.width);
        let y1 = ((y + h).round().max(0.0) as u32).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let src = premultiply(c, 1.0);
        let inv = 255 - src[3] as u32;
        let stride = self.width as usize * 4;
        for row in self.buf[y0 as usize * stride..y1 as usize * stride].chunks_exact_mut(stride) {
            for px in row[x0 as usize * 4..x1 as usize * 4].as_chunks_mut::<4>().0 {
                for (dst, s) in px.iter_mut().zip(src) {
                    *dst = (s as u32 + (*dst as u32 * inv + 127) / 255) as u8;
                }
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

    /// Contour d'un rectangle arrondi, d'épaisseur `width`, à l'intérieur du rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rounded(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        width: f32,
        c: Rgba,
    ) {
        let r = radius.min(w / 2.0).min(h / 2.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            for px in x.floor() as i32..(x + w).ceil() as i32 {
                // Distance signée exacte (négative à l'intérieur, même sans arrondi)
                let qx = (px as f32 + 0.5 - cx).abs() - hx;
                let qy = (py as f32 + 0.5 - cy).abs() - hy;
                let (dx, dy) = (qx.max(0.0), qy.max(0.0));
                let dist = (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0) - r;
                // dans la bande [-width, 0] de la distance signée
                let outer = (0.5 - dist).clamp(0.0, 1.0);
                let inner = (0.5 - (dist + width)).clamp(0.0, 1.0);
                self.blend(px, py, c, outer - inner);
            }
        }
    }

    /// Trait anticrénelé de (x0, y0) à (x1, y1), bouts arrondis (distance au
    /// segment, mesurée au centre de chaque pixel).
    #[allow(clippy::too_many_arguments)]
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        let r = width / 2.0;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let (left, right) = (
            (x0.min(x1) - r - 1.0).floor() as i32,
            (x0.max(x1) + r + 1.0).ceil() as i32,
        );
        let (top, bottom) = (
            (y0.min(y1) - r - 1.0).floor() as i32,
            (y0.max(y1) + r + 1.0).ceil() as i32,
        );
        for py in top..bottom {
            for px in left..right {
                let (qx, qy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
                let t = ((qx * dx + qy * dy) / len2).clamp(0.0, 1.0);
                let dist = (qx - t * dx).hypot(qy - t * dy) - r;
                self.blend(px, py, c, (0.5 - dist).clamp(0.0, 1.0));
            }
        }
    }

    /// Triangle plein anticrénelé (4 × 4 échantillons par pixel)
    pub fn fill_triangle(&mut self, a: (f32, f32), b: (f32, f32), c3: (f32, f32), c: Rgba) {
        let edge = |p: (f32, f32), q: (f32, f32), x: f32, y: f32| {
            (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0)
        };
        let area = edge(a, b, c3.0, c3.1);
        if area.abs() < 1e-6 {
            return;
        }
        let xs = [a.0, b.0, c3.0];
        let ys = [a.1, b.1, c3.1];
        let min = |v: [f32; 3]| v.iter().copied().fold(f32::MAX, f32::min);
        let max = |v: [f32; 3]| v.iter().copied().fold(f32::MIN, f32::max);
        for py in min(ys).floor() as i32..max(ys).ceil() as i32 {
            for px in min(xs).floor() as i32..max(xs).ceil() as i32 {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (x, y) = (
                            px as f32 + (sx as f32 + 0.5) / 4.0,
                            py as f32 + (sy as f32 + 0.5) / 4.0,
                        );
                        let (w0, w1, w2) = (edge(b, c3, x, y), edge(c3, a, x, y), edge(a, b, x, y));
                        if (w0 * area >= 0.0) && (w1 * area >= 0.0) && (w2 * area >= 0.0) {
                            hits += 1;
                        }
                    }
                }
                self.blend(px, py, c, hits as f32 / 16.0);
            }
        }
    }

    /// Disque plein centré en (cx, cy).
    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, c: Rgba) {
        self.fill_rounded(
            cx - radius,
            cy - radius,
            2.0 * radius,
            2.0 * radius,
            radius,
            c,
        );
    }

    /// Image (pixels prémultipliés B, G, R, A) mise à l'échelle dans le
    /// rectangle donné, chaque pixel étant la moyenne des pixels source qu'il couvre.
    #[allow(clippy::too_many_arguments)]
    pub fn image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        self.image_alpha(img, x, y, w, h, radius, 1.0);
    }

    /// Comme `image`, à l'opacité `alpha` (0 à 1) : fondus
    #[allow(clippy::too_many_arguments)]
    pub fn image_alpha(
        &mut self,
        img: &Image,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        alpha: f32,
    ) {
        if img.width == 0 || img.height == 0 || w < 1.0 || h < 1.0 || alpha <= 0.0 {
            return;
        }
        // Image à sa taille exacte : calée sur la grille des pixels, sinon chaque
        // pixel en mélangerait deux (icônes placées en cercle dans la roue)
        let (x, y) = if (w - img.width as f32).abs() < 0.5 && (h - img.height as f32).abs() < 0.5 {
            (x.round(), y.round())
        } else {
            (x, y)
        };
        // Rayon d'au moins un demi-pixel : à 0, la couverture des pixels
        // intérieurs tomberait à 0,5 (image à moitié transparente)
        let r = radius.max(0.5).min(w / 2.0).min(h / 2.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        let (sx, sy) = (img.width as f32 / w, img.height as f32 / h);
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            if py < 0 || py as u32 >= self.height {
                continue;
            }
            for px in x.floor() as i32..(x + w).ceil() as i32 {
                if px < 0 || px as u32 >= self.width {
                    continue;
                }
                let dx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
                let dy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
                let coverage =
                    (0.5 - ((dx * dx + dy * dy).sqrt() - r)).clamp(0.0, 1.0) * alpha.min(1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let u0 = (((px as f32 - x) * sx).max(0.0) as u32).min(img.width - 1);
                let v0 = (((py as f32 - y) * sy).max(0.0) as u32).min(img.height - 1);
                let u1 = (((px as f32 + 1.0 - x) * sx).ceil() as u32).clamp(u0 + 1, img.width);
                let v1 = (((py as f32 + 1.0 - y) * sy).ceil() as u32).clamp(v0 + 1, img.height);
                let (mut acc, mut n) = ([0u32; 4], 0u32);
                for v in v0..v1 {
                    for u in u0..u1 {
                        let i = ((v * img.width + u) * 4) as usize;
                        for (a, d) in acc.iter_mut().zip(&img.data[i..i + 4]) {
                            *a += *d as u32;
                        }
                        n += 1;
                    }
                }
                let src: [f32; 4] = acc.map(|a| a as f32 / n as f32 * coverage);
                let i = ((py as u32 * self.width + px as u32) * 4) as usize;
                let inv = 1.0 - src[3] / 255.0;
                for (dst, s) in self.buf[i..i + 4].iter_mut().zip(src) {
                    *dst = (s + *dst as f32 * inv).round() as u8;
                }
            }
        }
    }

    /// Recouvre tout le canevas d'une couleur translucide (voile sur un fond).
    pub fn tint(&mut self, c: Rgba) {
        let src = premultiply(c, 1.0);
        let inv = 1.0 - src[3] as f32 / 255.0;
        for px in self.buf.as_chunks_mut::<4>().0 {
            for (dst, s) in px.iter_mut().zip(src) {
                *dst = (s as f32 + *dst as f32 * inv).round() as u8;
            }
        }
    }

    /// Mélange « source par-dessus » d'un pixel avec une couverture 0.0–1.0.
    pub fn blend(&mut self, x: i32, y: i32, c: Rgba, coverage: f32) {
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

/// Image en mémoire, pixels prémultipliés (B, G, R, A).
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct Text {
    font: Font,
    /// Glyphes déjà rendus, par caractère et taille : les panneaux qui se
    /// redessinent souvent (moniteur, curseurs) ne refont pas le travail
    glyphs: Mutex<HashMap<(char, u32), Arc<Glyph>>>,
}

/// Glyphe rendu : mesures et couverture de chaque pixel
type Glyph = (Metrics, Vec<u8>);

/// Au-delà, le cache repart de zéro (changement d'échelle, textes très variés)
const GLYPH_CACHE: usize = 4096;

impl Text {
    /// Police d'après son chemin, ou son nom de fichier (cherché dans les
    /// polices livrées avec odysseus, puis celles du système : common::data)
    pub fn load(path: &str) -> Result<Self, String> {
        let path = crate::data::font(path);
        let data = std::fs::read(&path).map_err(|e| format!("{} : {e}", path.display()))?;
        let font = Font::from_bytes(data, FontSettings::default())?;
        Ok(Self {
            font,
            glyphs: Mutex::new(HashMap::new()),
        })
    }

    fn glyph(&self, ch: char, px: f32) -> Arc<Glyph> {
        let key = (ch, px.to_bits());
        let mut cache = self.glyphs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = cache.get(&key) {
            return g.clone();
        }
        let g = Arc::new(self.font.rasterize(ch, px));
        if cache.len() >= GLYPH_CACHE {
            cache.clear();
        }
        cache.insert(key, g.clone());
        g
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

    /// Découpe `s` en lignes d'au plus `max_w` pixels (au mot près), au plus
    /// `max_lines` lignes, la dernière raccourcie avec « … » si besoin.
    pub fn wrap(&self, s: &str, px: f32, max_w: f32, max_lines: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for paragraph in s.lines() {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let candidate = if line.is_empty() {
                    word.to_string()
                } else {
                    format!("{line} {word}")
                };
                if self.measure(&candidate, px) <= max_w || line.is_empty() {
                    line = candidate;
                } else {
                    lines.push(std::mem::replace(&mut line, word.to_string()));
                }
            }
            lines.push(line);
        }
        lines.retain(|l| !l.is_empty());
        if lines.len() > max_lines.max(1) {
            let rest = lines[max_lines.max(1) - 1..].join(" ");
            lines.truncate(max_lines.max(1) - 1);
            let last = self.truncate(&rest, px, max_w);
            lines.push(if last.ends_with('…') {
                last
            } else {
                format!("{last}…")
            });
        }
        lines
            .into_iter()
            .map(|l| self.truncate(&l, px, max_w))
            .collect()
    }

    /// Dessine `s` centré horizontalement sur `cx`, dans la bande [y, y + h].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_centered(
        &self,
        canvas: &mut Canvas,
        s: &str,
        cx: f32,
        y: f32,
        h: f32,
        px: f32,
        color: Rgba,
    ) {
        let w = self.measure(s, px);
        self.draw_in(canvas, s, cx - w / 2.0, y, h, px, color);
    }

    /// Dessine une icône (un glyphe de la police d'icônes) centrée en (cx, cy).
    pub fn icon(&self, canvas: &mut Canvas, glyph: char, cx: f32, cy: f32, px: f32, color: Rgba) {
        let g = self.glyph(glyph, px);
        let (m, coverage) = (&g.0, &g.1);
        let left = (cx - m.width as f32 / 2.0).round() as i32;
        let top = (cy - m.height as f32 / 2.0).round() as i32;
        for row in 0..m.height {
            for col in 0..m.width {
                let a = coverage[row * m.width + col] as f32 / 255.0;
                canvas.blend(left + col as i32, top + row as i32, color, a);
            }
        }
    }

    /// Dessine `s` à partir de `x`, centré verticalement dans le canevas.
    pub fn draw(&self, canvas: &mut Canvas, s: &str, x: f32, px: f32, color: Rgba) {
        let h = canvas.height as f32;
        self.draw_in(canvas, s, x, 0.0, h, px, color);
    }

    /// Avance d'un caractère à la taille `px`.
    pub fn advance(&self, ch: char, px: f32) -> f32 {
        self.font.metrics(ch, px).advance_width
    }

    /// Ligne de base d'un texte centré verticalement dans la bande [y, y + h].
    pub fn baseline(&self, y: f32, h: f32, px: f32) -> f32 {
        let line = self
            .font
            .horizontal_line_metrics(px)
            .expect("police sans métriques horizontales");
        (y + (h + line.ascent + line.descent) / 2.0).round()
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
        let baseline = self.baseline(y, h, px);
        self.draw_baseline(canvas, s, x, baseline, px, color);
    }

    /// Dessine `s` à partir de `x`, posé sur la ligne de base `baseline` (textes
    /// de polices différentes alignés sur une même ligne).
    pub fn draw_baseline(
        &self,
        canvas: &mut Canvas,
        s: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: Rgba,
    ) {
        let mut pen = x;
        for ch in s.chars() {
            let g = self.glyph(ch, px);
            let (m, coverage) = (&g.0, &g.1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_image_without_radius() {
        let img = Image {
            width: 2,
            height: 2,
            data: [200u8, 100, 50, 255].repeat(4),
        };
        let mut buf = vec![0u8; 8 * 8 * 4];
        let mut c = Canvas {
            buf: &mut buf,
            width: 8,
            height: 8,
        };
        c.image(&img, 2.0, 2.0, 4.0, 4.0, 0.0);
        // Pixel intérieur : l'image telle quelle, opaque
        let i = ((3 * 8 + 3) * 4) as usize;
        assert_eq!(&buf[i..i + 4], &[200, 100, 50, 255]);
    }
}
