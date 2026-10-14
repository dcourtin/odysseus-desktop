//! Icône d'une notification : image jointe, chemin, ou icône de l'application
//! (directement ou via son entrée .desktop). Sans icône, l'interface dessine
//! une pastille avec l'initiale de l'application.

use std::path::PathBuf;

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
    if let Some(img) = image_path.and_then(|p| common::icons::load(p, 64)) {
        return Some(img);
    }
    if let Some(img) = common::icons::load(app_icon, 64) {
        return Some(img);
    }
    let icon_name = desktop_entry.and_then(desktop_icon)?;
    common::icons::load(&icon_name, 64)
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
