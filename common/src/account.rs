//! Compte de l'utilisateur, tel qu'odysseus le montre (Réglages > Compte,
//! verrou, en-tête des Réglages) :
//! - nom affiché : `nom = …` dans `~/.config/odysseus/compte`, sinon le nom
//!   complet de /etc/passwd (champ GECOS), sinon l'identifiant. Changer le
//!   champ GECOS demanderait des droits d'administrateur ; l'identifiant de
//!   connexion, lui, ne change pas ;
//! - photo : `~/.face` (PNG carré de 512 pixels), la convention des autres
//!   bureaux, recadrée au centre depuis n'importe quelle image (HEIC compris).

use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};

/// Côté de la photo enregistrée
const AVATAR_PX: u32 = 512;

/// Identifiant de connexion
pub fn login() -> String {
    std::env::var("USER").unwrap_or_else(|_| "utilisateur".into())
}

fn config_path() -> Option<PathBuf> {
    crate::theme::config_path().map(|p| p.with_file_name("compte"))
}

/// Nom choisi dans Réglages > Compte, s'il y en a un
pub fn custom_name() -> Option<String> {
    std::fs::read_to_string(config_path()?)
        .ok()?
        .lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "nom")
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Nom complet de /etc/passwd (champ GECOS), s'il est renseigné
pub fn system_name() -> Option<String> {
    let login = login();
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find(|l| l.starts_with(&format!("{login}:")))
        .and_then(|l| l.split(':').nth(4))
        .map(|g| g.split(',').next().unwrap_or("").trim().to_string())
        .filter(|g| !g.is_empty())
}

/// Nom montré par odysseus
pub fn display_name() -> String {
    custom_name().or_else(system_name).unwrap_or_else(|| {
        let login = login();
        let mut c = login.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect())
            .unwrap_or(login)
    })
}

/// Enregistre le nom affiché ; vide : retour au nom du système
pub fn set_name(name: &str) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::other("HOME inconnu"))?;
    let name = name.replace(['\n', '\r'], " ");
    let name = name.trim();
    if name.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        format!("# Compte (Réglages > Compte)\nnom = {name}\n"),
    )?;
    std::fs::rename(tmp, path)
}

/// Deux initiales, pour un avatar sans photo
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Emplacement de la photo
pub fn avatar_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".face"))
}

/// Photo de l'utilisateur, en image prémultipliée, s'il en a une
pub fn load_avatar() -> Option<crate::draw::Image> {
    let path = avatar_path().filter(|p| p.is_file())?;
    crate::icons::load(&path.to_string_lossy(), AVATAR_PX)
}

/// Fait de `source` la photo : recadrée en carré au centre, 512 × 512, PNG
pub fn set_avatar(source: &Path) -> Result<(), String> {
    let img = decode(source)?;
    let side = img.width().min(img.height());
    if side < 16 {
        return Err("image trop petite".into());
    }
    let (x, y) = ((img.width() - side) / 2, (img.height() - side) / 2);
    let square =
        img.crop_imm(x, y, side, side)
            .resize_exact(AVATAR_PX, AVATAR_PX, FilterType::Lanczos3);
    let path = avatar_path().ok_or("HOME inconnu")?;
    // Écriture à côté puis renommage : le verrou ne lit jamais une photo à moitié écrite
    let tmp = path.with_extension("tmp");
    square
        .to_rgba8()
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Retire la photo (retour aux initiales)
pub fn remove_avatar() -> std::io::Result<()> {
    match avatar_path().map(std::fs::remove_file) {
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Images qui peuvent servir de photo
pub fn is_photo(path: &Path) -> bool {
    crate::heic::is_heic(path)
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
}

/// Décode une image en appliquant son orientation (photo de téléphone)
pub fn decode(path: &Path) -> Result<DynamicImage, String> {
    if crate::heic::is_heic(path) {
        return crate::heic::decode(path);
    }
    let mut decoder = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Images récentes de l'utilisateur (Images, Téléchargements), les plus
/// récentes d'abord, au plus `max`
pub fn recent_photos(max: usize) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = ["Images", "Téléchargements"]
        .iter()
        .filter_map(|d| std::fs::read_dir(home.join(d)).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_photo(p) && p.is_file())
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(max).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initiales() {
        assert_eq!(initials("David Courtin"), "DC");
        assert_eq!(initials("élise"), "É");
        assert_eq!(initials("Jean Paul Sartre"), "JP");
    }
}
