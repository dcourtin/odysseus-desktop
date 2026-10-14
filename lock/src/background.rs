//! Fond de l'écran verrouillé : le fond d'écran, très flouté et assombri,
//! comme sur macOS. Calculé une fois à la taille du tampon.

use common::draw::Image;

/// Fond d'écran flouté de `width`×`height` pixels (pixels prémultipliés B, G, R, A).
/// Sans fond d'écran lisible : un dégradé sombre du thème.
pub fn blurred(width: u32, height: u32) -> Image {
    match load_small() {
        Some(small) => upscale(&small, width, height),
        None => gradient(width, height),
    }
}

/// Image réduite (environ 1/24) puis floutée : le flou coûte peu à cette taille,
/// et l'agrandissement bilinéaire achève de lisser.
fn load_small() -> Option<image::RgbImage> {
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::Path::new(&home).join(".config/odysseus");
    let path = ["wallpaper.jpg", "wallpaper.png"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())?;
    let img = image::ImageReader::open(path).ok()?.decode().ok()?;
    let small = img.thumbnail(160, 160).to_rgb8();
    Some(image::imageops::blur(&small, 3.0))
}

/// Agrandit en recadrant pour couvrir (sans déformation), assombrit de 35 %.
fn upscale(src: &image::RgbImage, width: u32, height: u32) -> Image {
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let k = (sw / width as f32).min(sh / height as f32);
    let (ox, oy) = (
        (sw - width as f32 * k) / 2.0,
        (sh - height as f32 * k) / 2.0,
    );
    let mut data = vec![0u8; (width * height * 4) as usize];
    let dim = 0.65;
    for y in 0..height {
        let fy = (oy + (y as f32 + 0.5) * k - 0.5).clamp(0.0, sh - 1.0);
        let (y0, ty) = (fy.floor() as u32, fy.fract());
        let y1 = (y0 + 1).min(src.height() - 1);
        for x in 0..width {
            let fx = (ox + (x as f32 + 0.5) * k - 0.5).clamp(0.0, sw - 1.0);
            let (x0, tx) = (fx.floor() as u32, fx.fract());
            let x1 = (x0 + 1).min(src.width() - 1);
            let (a, b) = (src.get_pixel(x0, y0).0, src.get_pixel(x1, y0).0);
            let (c, d) = (src.get_pixel(x0, y1).0, src.get_pixel(x1, y1).0);
            let i = ((y * width + x) * 4) as usize;
            for ch in 0..3 {
                let top = a[ch] as f32 * (1.0 - tx) + b[ch] as f32 * tx;
                let bottom = c[ch] as f32 * (1.0 - tx) + d[ch] as f32 * tx;
                let v = (top * (1.0 - ty) + bottom * ty) * dim;
                // RGB -> octets B, G, R
                data[i + 2 - ch] = v.round() as u8;
            }
            data[i + 3] = 255;
        }
    }
    Image {
        width,
        height,
        data,
    }
}

fn gradient(width: u32, height: u32) -> Image {
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        let t = y as f32 / height.max(1) as f32;
        // du bleu nuit (#1e1e2e) vers un violet sombre (#2a2140)
        let r = 0x1e as f32 + (0x2a - 0x1e) as f32 * t;
        let g = 0x1e as f32 + (0x21 - 0x1e) as f32 * t;
        let b = 0x2e as f32 + (0x40 - 0x2e) as f32 * t;
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            data[i] = b as u8;
            data[i + 1] = g as u8;
            data[i + 2] = r as u8;
            data[i + 3] = 255;
        }
    }
    Image {
        width,
        height,
        data,
    }
}
