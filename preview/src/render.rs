//! Affichage d'une image à n'importe quelle échelle : on choisit le niveau de
//! la pyramide le plus proche (au-dessus) de l'échelle voulue, puis on
//! échantillonne en bilinéaire, les lignes réparties sur tous les cœurs.

use crate::picture::{Level, Picture};

/// Rectangle de destination, en pixels du tampon
#[derive(Clone, Copy)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// Dessine `pic` dans `buf` (largeur `bw`) : coin haut-gauche de l'image en
/// (`ox`, `oy`) pixels du tampon, `scale` pixels du tampon par pixel d'image.
#[allow(clippy::too_many_arguments)]
pub fn draw(buf: &mut [u8], bw: u32, pic: &Picture, scale: f64, ox: f64, oy: f64, clip: Clip) {
    if pic.levels.is_empty() || scale <= 0.0 {
        return;
    }
    // Niveau : réduction par 2^n tant qu'elle reste au moins à l'échelle voulue
    let mut n = 0usize;
    while n + 1 < pic.levels.len() && scale * (1u64 << (n + 1)) as f64 <= 1.0 {
        n += 1;
    }
    let level = &pic.levels[n];
    let ls = scale * (1u64 << n) as f64;
    let (iw, ih) = (pic.width() as f64 * scale, pic.height() as f64 * scale);
    let x0 = clip.x0.max(ox.floor() as i32);
    let y0 = clip.y0.max(oy.floor() as i32);
    let x1 = clip.x1.min((ox + iw).ceil() as i32);
    let y1 = clip.y1.min((oy + ih).ceil() as i32);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let stride = bw as usize * 4;
    let rows = &mut buf[y0 as usize * stride..y1 as usize * stride];
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);
    let chunk_rows = ((y1 - y0) as usize).div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (i, chunk) in rows.chunks_mut(chunk_rows * stride).enumerate() {
            let first = y0 + (i * chunk_rows) as i32;
            s.spawn(move || {
                for (r, row) in chunk.chunks_mut(stride).enumerate() {
                    let y = first + r as i32;
                    let v = (y as f64 + 0.5 - oy) / ls - 0.5;
                    for x in x0..x1 {
                        let u = (x as f64 + 0.5 - ox) / ls - 0.5;
                        let px = sample(level, u, v);
                        let o = x as usize * 4;
                        let inv = 255 - px[3] as u32;
                        for c in 0..4 {
                            row[o + c] =
                                (px[c] as u32 + (row[o + c] as u32 * inv + 127) / 255) as u8;
                        }
                    }
                }
            });
        }
    });
}

/// Échantillon bilinéaire (coordonnées en pixels du niveau, bornées aux bords)
#[inline]
fn sample(level: &Level, u: f64, v: f64) -> [u8; 4] {
    let (w, h) = (level.width as i64, level.height as i64);
    let u = u.clamp(0.0, (w - 1) as f64);
    let v = v.clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (u.floor() as i64, v.floor() as i64);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    // Poids en virgule fixe (8 bits)
    let fx = ((u - x0 as f64) * 256.0) as u32;
    let fy = ((v - y0 as f64) * 256.0) as u32;
    let at = |x: i64, y: i64| ((y * w + x) * 4) as usize;
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let data = &level.data;
    let mut out = [0u8; 4];
    for k in 0..4 {
        let top = data[a + k] as u32 * (256 - fx) + data[b + k] as u32 * fx;
        let bottom = data[c + k] as u32 * (256 - fx) + data[d + k] as u32 * fx;
        out[k] = ((top * (256 - fy) + bottom * fy + 32768) >> 16) as u8;
    }
    out
}
