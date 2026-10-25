//! Icônes d'applications (thème hicolor, pixmaps, chemins absolus), chargées
//! en images prémultipliées. Une icône seulement disponible en SVG est
//! convertie une fois en PNG par `rsvg-convert` (librsvg), puis gardée dans
//! ~/.cache/odysseus/icons : aucun moteur SVG dans les programmes du bureau.
//!
//! Les icônes d'odysseus (attribut `data-odysseus-theme`, voir tools/icones.py)
//! prennent les couleurs du thème actif : une feuille de style tirée de la
//! palette est passée à librsvg, et le cache est propre à chaque palette.

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
        return load_raster(&rasterize_svg(&path, size)?, size);
    }
    load_raster(&path, size)
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

/// Feuille de style des icônes d'odysseus pour la palette active
fn theme_css() -> String {
    let p = crate::theme::palette();
    let css = |c: crate::theme::Rgba| {
        format!(
            "rgba({}, {}, {}, {:.3})",
            (c.r * 255.0).round(),
            (c.g * 255.0).round(),
            (c.b * 255.0).round(),
            c.a
        )
    };
    format!(
        ".tuile {{ fill: {}; }}\n.bord {{ stroke: {}; stroke-opacity: 1; }}\n\
         .trait {{ stroke: {}; }}\n.plein {{ fill: {}; }}\n",
        css(p.background.with_alpha(1.0)),
        css(p.ink(0.14)),
        css(p.accent),
        css(p.accent),
    )
}

/// Empreinte courte (FNV-1a) : nom de fichier propre à une feuille de style
fn fingerprint(text: &str) -> String {
    let h = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    format!("{:08x}", h as u32)
}

/// SVG → PNG en cache (rsvg-convert), à la taille demandée.
fn rasterize_svg(svg: &Path, size: u32) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".cache/odysseus/icons");
    let stem = svg.file_stem()?.to_string_lossy();
    // Icône d'odysseus : aux couleurs du thème, un PNG par palette
    let themed = std::fs::read(svg)
        .ok()
        .is_some_and(|b| b.windows(19).any(|w| w == b"data-odysseus-theme"));
    let css = themed.then(theme_css);
    let out = match &css {
        Some(css) => dir.join(format!("{stem}-{size}-{}.png", fingerprint(css))),
        None => dir.join(format!("{stem}-{size}.png")),
    };
    let fresh = match (out.metadata(), svg.metadata()) {
        (Ok(o), Ok(s)) => o.modified().ok() >= s.modified().ok(),
        _ => false,
    };
    if !fresh {
        std::fs::create_dir_all(&dir).ok()?;
        let mut cmd = std::process::Command::new("rsvg-convert");
        cmd.args(["-w", &size.to_string(), "-h", &size.to_string(), "-o"])
            .arg(&out);
        if let Some(css) = &css {
            let sheet = dir.join(format!("theme-{}.css", fingerprint(css)));
            std::fs::write(&sheet, css).ok()?;
            cmd.arg("-s").arg(sheet);
        }
        let status = cmd.arg(svg).status().ok()?;
        if !status.success() {
            return None;
        }
    }
    Some(out)
}

/// Image à la taille demandée : réduite en Lanczos si elle est plus grande,
/// jamais agrandie (un agrandissement puis une réduction au dessin la rendait
/// floue). Elle se dessine alors pixel pour pixel.
fn load_raster(path: &Path, size: u32) -> Option<Image> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let size = size.clamp(8, 1024);
    let img = if img.width() > size || img.height() > size {
        img.resize(size, size, image::imageops::FilterType::Lanczos3)
    } else {
        img
    }
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
