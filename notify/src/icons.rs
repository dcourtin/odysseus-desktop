//! Icône d'une notification : image jointe, chemin, ou icône de l'application
//! (thème hicolor, pixmaps, ou entrée .desktop). Sans icône, l'interface
//! dessine une pastille avec l'initiale de l'application.

use std::path::{Path, PathBuf};

use common::draw::Image;

pub fn resolve(
    image: Option<Image>,
    image_path: Option<&str>,
    app_icon: &str,
    desktop_entry: Option<&str>,
) -> Option<Image> {
    if image.is_some() {
        return image;
    }
    if let Some(img) = image_path.and_then(from_name_or_path) {
        return Some(img);
    }
    if let Some(img) = from_name_or_path(app_icon) {
        return Some(img);
    }
    let icon_name = desktop_entry.and_then(desktop_icon)?;
    from_name_or_path(&icon_name)
}

fn from_name_or_path(name: &str) -> Option<Image> {
    let name = name.strip_prefix("file://").unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return load(Path::new(name));
    }
    lookup(name).and_then(|p| load(&p))
}

/// Cherche une icône PNG par nom (les tailles moyennes d'abord).
fn lookup(name: &str) -> Option<PathBuf> {
    let mut roots = vec![PathBuf::from("/usr/share/icons/hicolor")];
    if let Ok(home) = std::env::var("HOME") {
        roots.insert(0, PathBuf::from(home).join(".local/share/icons/hicolor"));
    }
    for size in ["128x128", "96x96", "64x64", "256x256", "48x48", "32x32"] {
        for root in &roots {
            let p = root.join(size).join("apps").join(format!("{name}.png"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    let p = PathBuf::from("/usr/share/pixmaps").join(format!("{name}.png"));
    p.exists().then_some(p)
}

/// Clé Icon= d'une entrée .desktop
fn desktop_icon(entry: &str) -> Option<String> {
    let file = format!("{}.desktop", entry.trim_end_matches(".desktop"));
    let mut dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        dirs.insert(0, PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.iter().find_map(|d| {
        let text = std::fs::read_to_string(d.join(&file)).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix("Icon=").map(|v| v.trim().to_string()))
    })
}

fn load(path: &Path) -> Option<Image> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .thumbnail(128, 128)
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
