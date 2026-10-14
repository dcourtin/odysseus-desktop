//! Icônes d'applications (thème hicolor, pixmaps, chemins absolus), chargées
//! en images prémultipliées. Une icône seulement disponible en SVG est
//! convertie une fois en PNG par `rsvg-convert` (librsvg), puis gardée dans
//! ~/.cache/odysseus/icons : aucun moteur SVG dans les programmes du bureau.

use std::path::{Path, PathBuf};

use crate::draw::Image;

/// Icône `name` (nom de thème ou chemin) d'environ `size` pixels.
pub fn load(name: &str, size: u32) -> Option<Image> {
    let name = name.strip_prefix("file://").unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    let path = if name.starts_with('/') {
        PathBuf::from(name)
    } else {
        lookup(name, size)?
    };
    if path.extension().is_some_and(|e| e == "svg" || e == "svgz") {
        return load_raster(&rasterize_svg(&path, size)?);
    }
    load_raster(&path)
}

/// Dossiers de thème, du plus prioritaire au moins
fn theme_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(&home).join(".local/share/icons/hicolor"));
    }
    roots.push(PathBuf::from("/usr/share/icons/hicolor"));
    roots.push(PathBuf::from("/usr/share/icons/Adwaita"));
    roots
}

/// PNG à la taille la plus proche au-dessus de `size`, sinon SVG, sinon pixmaps.
fn lookup(name: &str, size: u32) -> Option<PathBuf> {
    const SIZES: [u32; 9] = [16, 22, 24, 32, 48, 64, 96, 128, 256];
    let mut sizes: Vec<u32> = SIZES.iter().copied().filter(|s| *s >= size).collect();
    sizes.extend(SIZES.iter().rev().copied().filter(|s| *s < size));
    let roots = theme_roots();
    for s in &sizes {
        for root in &roots {
            let p = root.join(format!("{s}x{s}/apps/{name}.png"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    for root in &roots {
        let p = root.join(format!("scalable/apps/{name}.svg"));
        if p.exists() {
            return Some(p);
        }
    }
    ["png", "svg"]
        .iter()
        .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}")))
        .find(|p| p.exists())
}

/// SVG → PNG en cache (rsvg-convert), à la taille demandée.
fn rasterize_svg(svg: &Path, size: u32) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".cache/odysseus/icons");
    let stem = svg.file_stem()?.to_string_lossy();
    let out = dir.join(format!("{stem}-{size}.png"));
    let fresh = match (out.metadata(), svg.metadata()) {
        (Ok(o), Ok(s)) => o.modified().ok() >= s.modified().ok(),
        _ => false,
    };
    if !fresh {
        std::fs::create_dir_all(&dir).ok()?;
        let status = std::process::Command::new("rsvg-convert")
            .args(["-w", &size.to_string(), "-h", &size.to_string(), "-o"])
            .arg(&out)
            .arg(svg)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
    }
    Some(out)
}

fn load_raster(path: &Path) -> Option<Image> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .thumbnail(256, 256)
        .to_rgba8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        *px = [
            (px[2] as u32 * a / 255) as u8,
            (px[1] as u32 * a / 255) as u8,
            (px[0] as u32 * a / 255) as u8,
            a as u8,
        ];
    }
    Some(Image {
        width,
        height,
        data,
    })
}
