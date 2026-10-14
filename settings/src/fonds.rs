//! Fonds d'écran : ceux d'odysseus (dégradés calculés ici, une fois, en 4K) et
//! ceux de l'utilisateur (dossier wallpapers de la configuration, où les
//! images déposées sont copiées). Vignettes décodées dans un fil.

use std::path::{Path, PathBuf};

use common::draw::Image;

/// Un fond de la galerie
pub struct Fond {
    pub name: String,
    pub path: PathBuf,
    pub thumb: Option<Image>,
}

/// Fonds d'odysseus : nom, couleur du haut, couleur du bas, lueurs (x, y, rayon, couleur, force)
type Glow = (f32, f32, f32, u32, f32);
const BUILTIN: [(&str, u32, u32, &[Glow]); 6] = [
    (
        "Odyssée",
        0x14141f,
        0x22223a,
        &[
            (0.78, 0.18, 0.55, 0x3b4a8a, 0.55),
            (0.12, 0.92, 0.6, 0x4a3070, 0.5),
        ],
    ),
    (
        "Mer Égée",
        0x061a33,
        0x0f5a7a,
        &[
            (0.25, 0.8, 0.6, 0x4fc3d6, 0.45),
            (0.85, 0.15, 0.5, 0x1d3f8c, 0.6),
        ],
    ),
    (
        "Aube sur Ithaque",
        0x1b2047,
        0xd9845f,
        &[
            (0.7, 0.95, 0.65, 0xffcf9a, 0.6),
            (0.15, 0.2, 0.5, 0x3a2a6b, 0.5),
        ],
    ),
    (
        "Crépuscule",
        0x150c2b,
        0x6b2f5f,
        &[
            (0.82, 0.72, 0.55, 0xf08a5d, 0.5),
            (0.2, 0.15, 0.45, 0x2e1f66, 0.6),
        ],
    ),
    (
        "Oliveraie",
        0x161d14,
        0x55602f,
        &[
            (0.3, 0.85, 0.6, 0xc9a35e, 0.45),
            (0.85, 0.2, 0.45, 0x2c4a2a, 0.5),
        ],
    ),
    (
        "Marbre",
        0xece6da,
        0xd6cbb6,
        &[
            (0.75, 0.25, 0.6, 0xffffff, 0.7),
            (0.2, 0.85, 0.5, 0xc4b59a, 0.4),
        ],
    ),
];

/// Dossier des fonds générés par odysseus
fn builtin_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("odysseus/fonds"))
}

/// Dossier des fonds de l'utilisateur (images déposées)
pub fn user_dir() -> Option<PathBuf> {
    common::theme::config_path().map(|p| p.with_file_name("wallpapers"))
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png")
    )
}

/// Liste de la galerie : d'abord ceux d'odysseus (dans l'ordre), puis ceux de
/// l'utilisateur par nom. Ceux d'odysseus manquants sont à générer.
pub fn list() -> (Vec<Fond>, Vec<usize>) {
    let mut fonds = Vec::new();
    let mut missing = Vec::new();
    if let Some(dir) = builtin_dir() {
        for (i, (name, ..)) in BUILTIN.iter().enumerate() {
            let path = dir.join(format!("{name}.jpg"));
            if !path.exists() {
                missing.push(i);
            }
            fonds.push(Fond {
                name: name.to_string(),
                path,
                thumb: None,
            });
        }
    }
    if let Some(dir) = user_dir() {
        let mut user: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| is_image(p))
                    .collect()
            })
            .unwrap_or_default();
        user.sort();
        for path in user {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().replace(['-', '_'], " "))
                .unwrap_or_default();
            fonds.push(Fond {
                name,
                path,
                thumb: None,
            });
        }
    }
    (fonds, missing)
}

/// Calcule un fond d'odysseus en 4K (dégradé, lueurs, léger grain contre les
/// bandes) et l'enregistre en JPEG
pub fn generate(index: usize) -> Option<PathBuf> {
    let (name, top, bottom, glows) = BUILTIN[index];
    let path = builtin_dir()?.join(format!("{name}.jpg"));
    std::fs::create_dir_all(path.parent()?).ok()?;
    let (w, h) = (3840u32, 2160u32);
    let rgb = |c: u32| {
        [
            ((c >> 16) & 0xff) as f32,
            ((c >> 8) & 0xff) as f32,
            (c & 0xff) as f32,
        ]
    };
    let (t, b) = (rgb(top), rgb(bottom));
    let mut seed = 0x9e37_79b9u32.wrapping_add(index as u32);
    let mut img = image::RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
        // Dégradé vertical, un peu incliné
        let k = (v * 0.85 + u * 0.15).clamp(0.0, 1.0);
        let mut c = [0.0f32; 3];
        for i in 0..3 {
            c[i] = t[i] + (b[i] - t[i]) * k;
        }
        for &(gx, gy, r, color, strength) in glows {
            let (dx, dy) = ((u - gx) * 16.0 / 9.0, v - gy);
            let d2 = (dx * dx + dy * dy) / (r * r);
            let a = strength * (-d2 * 2.2).exp();
            let g = rgb(color);
            for i in 0..3 {
                c[i] += (g[i] - c[i]) * a;
            }
        }
        // Grain d'un niveau : pas de bandes dans les dégradés lents
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let n = (seed & 0xff) as f32 / 255.0 - 0.5;
        *px = image::Rgb(c.map(|v| (v + n).round().clamp(0.0, 255.0) as u8));
    }
    let tmp = path.with_extension("tmp");
    let file = std::fs::File::create(&tmp).ok()?;
    let mut writer = std::io::BufWriter::new(file);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 92)
        .encode_image(&img)
        .ok()?;
    drop(writer);
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

/// Vignette 16:9 (prémultipliée, B G R A)
pub fn thumbnail(path: &Path) -> Option<Image> {
    let img = image::open(path).ok()?;
    let (w, h) = (480u32, 270u32);
    let img = img
        .resize_to_fill(w, h, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(Image {
        width: w,
        height: h,
        data,
    })
}

/// Copie une image déposée dans le dossier des fonds de l'utilisateur
pub fn import(source: &Path) -> Result<PathBuf, String> {
    if !is_image(source) {
        return Err("Seules les images JPEG et PNG peuvent servir de fond".into());
    }
    let dir = user_dir().ok_or("dossier de configuration introuvable")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = source.file_name().ok_or("nom de fichier invalide")?;
    let dest = dir.join(name);
    if dest != source {
        std::fs::copy(source, &dest).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

/// Décode une liste d'URI (glisser-déposer) en chemins
pub fn paths_from_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(|p| {
            let bytes = p.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%'
                    && i + 2 < bytes.len()
                    && let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16)
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(out))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        assert_eq!(
            paths_from_uri_list("file:///home/d/Mon%20fond.jpg\r\n"),
            vec![PathBuf::from("/home/d/Mon fond.jpg")]
        );
    }
}
