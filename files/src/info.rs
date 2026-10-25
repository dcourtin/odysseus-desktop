//! Panneau d'informations (à droite) : détails de l'élément sélectionné,
//! vignette des images, et texte à glisser vers d'autres applications.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use common::draw::Image;

use crate::model::format_size;

/// Détails de l'élément affiché dans le panneau
pub struct Details {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub kind: String,
    /// Taille d'un fichier, ou nombre d'éléments d'un dossier
    pub size: String,
    pub modified: String,
    pub created: Option<String>,
    pub dimensions: Option<(u32, u32)>,
    /// Vignette (images), chargée en arrière-plan
    pub thumbnail: Option<Image>,
}

impl Details {
    pub fn read(path: &Path) -> Self {
        let meta = std::fs::metadata(path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let size = if is_dir {
            let n = std::fs::read_dir(path).map(|d| d.count()).unwrap_or(0);
            format!("{n} élément{}", if n > 1 { "s" } else { "" })
        } else {
            meta.as_ref()
                .map(|m| format_size(m.len()))
                .unwrap_or_else(|| "—".into())
        };
        let dimensions = if is_image(path) {
            image::image_dimensions(path).ok()
        } else {
            None
        };
        Self {
            path: path.to_path_buf(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into()),
            is_dir,
            kind: kind(path, is_dir),
            size,
            modified: full_date(meta.as_ref().and_then(|m| m.modified().ok())),
            created: meta
                .as_ref()
                .and_then(|m| m.created().ok())
                .map(|t| full_date(Some(t))),
            dimensions,
            thumbnail: None,
        }
    }
}

/// Images que la vignette sait lire (le reste a une icône)
pub fn is_image(path: &Path) -> bool {
    matches!(extension(path).as_str(), "png" | "jpg" | "jpeg")
}

/// Type de contenu à annoncer pour copier l'image elle-même
pub fn image_mime(path: &Path) -> &'static str {
    match extension(path).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        _ => "image/png",
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Nature de l'élément, en clair
fn kind(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return "Dossier".into();
    }
    let ext = extension(path);
    let known = match ext.as_str() {
        "png" => "Image PNG",
        "jpg" | "jpeg" => "Image JPEG",
        "webp" => "Image WebP",
        "heic" | "heif" => "Photo HEIC",
        "gif" => "Image GIF",
        "svg" => "Image SVG",
        "pdf" => "Document PDF",
        "txt" => "Texte",
        "md" => "Texte Markdown",
        "rs" => "Code Rust",
        "c" | "h" => "Code C",
        "py" => "Script Python",
        "sh" => "Script shell",
        "toml" | "conf" | "ini" | "cfg" | "json" | "yaml" | "yml" => "Configuration",
        "log" => "Journal",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "zst" | "tgz" => "Archive",
        "mp4" | "mkv" | "webm" | "mov" => "Vidéo",
        "mp3" | "flac" | "ogg" | "opus" | "wav" => "Audio",
        "ttf" | "otf" => "Police",
        "desktop" => "Raccourci d'application",
        "" => "Fichier",
        _ => return format!("Fichier {}", ext.to_uppercase()),
    };
    known.into()
}

/// Date complète : « mercredi 14 octobre 2026 à 21:28 »
fn full_date(time: Option<SystemTime>) -> String {
    use chrono::{DateTime, Datelike, Local};
    const JOURS: [&str; 7] = [
        "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
    ];
    const MOIS: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    let Some(time) = time else {
        return "—".into();
    };
    let date: DateTime<Local> = time.into();
    format!(
        "{} {} {} {} à {}",
        JOURS[date.weekday().num_days_from_monday() as usize],
        date.day(),
        MOIS[date.month0() as usize],
        date.year(),
        date.format("%H:%M")
    )
}

/// Vignette d'au plus `max_w` × `max_h` pixels (prémultipliée, B G R A)
pub fn thumbnail(path: &Path, max_w: u32, max_h: u32) -> Option<Image> {
    let img = if common::heic::is_heic(path) {
        common::heic::decode(path).ok()?
    } else {
        image::open(path).ok()?
    };
    let img = img.thumbnail(max_w, max_h).to_rgba8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
        px[0] = (b * a / 255) as u8;
        px[1] = (g * a / 255) as u8;
        px[2] = (r * a / 255) as u8;
    }
    Some(Image {
        width,
        height,
        data,
    })
}

/// Liste d'URI (glisser-déposer) : file:// et chemin encodé
pub fn uri_list(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri() {
        assert_eq!(
            uri_list(Path::new("/home/d/Capture d’écran.png")),
            "file:///home/d/Capture%20d%E2%80%99%C3%A9cran.png\r\n"
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(kind(Path::new("a.PNG"), false), "Image PNG");
        assert_eq!(kind(Path::new("a.xyz"), false), "Fichier XYZ");
        assert_eq!(kind(Path::new("dossier"), true), "Dossier");
    }
}
