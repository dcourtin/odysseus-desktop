//! Fond d'écran : une image (JPEG ou PNG) chargée une fois en texture GPU, recadrée pour
//! couvrir la sortie sans déformation (comme « fill » dans sway).
//!
//! Changement au hasard (Réglages > Fond d'écran, `fond_rotation` en secondes) :
//! parmi les images de l'utilisateur (~/.config/odysseus/wallpapers), décodée
//! dans un fil (`decode`) puis envoyée au GPU (`from_rgba`), avec un fondu
//! enchaîné de `FADE`.

use std::{
    error::Error,
    path::{Path, PathBuf},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                texture::{TextureBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Buffer, Logical, Rectangle, Size, Transform},
};

/// Fond choisi dans les Réglages (« uni » : aucun) ; sinon
/// ~/.config/odysseus/wallpaper.jpg en priorité, puis wallpaper.png
pub fn find(settings: &common::settings::Settings) -> Option<std::path::PathBuf> {
    match settings.wallpaper.as_deref() {
        Some("uni") => return None,
        Some(path) if std::path::Path::new(path).exists() => return Some(path.into()),
        _ => {}
    }
    let dir = std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/odysseus");
    ["wallpaper.jpg", "wallpaper.png"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
}

/// Fondu enchaîné d'un fond au suivant
pub const FADE: Duration = Duration::from_millis(1200);

/// Images de l'utilisateur parmi lesquelles tirer au hasard
pub fn pool() -> Vec<PathBuf> {
    let Some(dir) =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/odysseus/wallpapers"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut list: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
        })
        .collect();
    list.sort();
    list
}

/// Une image du lot, autre que `current` s'il y a le choix
pub fn pick(pool: &[PathBuf], current: Option<&Path>) -> Option<PathBuf> {
    let choices: Vec<&PathBuf> = pool
        .iter()
        .filter(|p| pool.len() < 2 || Some(p.as_path()) != current)
        .collect();
    if choices.is_empty() {
        return None;
    }
    // Hasard suffisant ici : les nanosecondes de l'horloge, mélangées
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    Some(choices[(x % choices.len() as u64) as usize].clone())
}

/// Décode une image en RGBA (dans un fil : une grande image prend du temps)
pub fn decode(path: &Path) -> Result<image::RgbaImage, String> {
    Ok(image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?
        .into_rgba8())
}

pub struct Wallpaper {
    buffer: TextureBuffer<GlesTexture>,
    size: Size<i32, Buffer>,
}

impl Wallpaper {
    pub fn load(renderer: &mut GlesRenderer, path: &Path) -> Result<Self, Box<dyn Error>> {
        Self::from_rgba(renderer, decode(path)?)
    }

    /// Envoie au GPU une image déjà décodée
    pub fn from_rgba(
        renderer: &mut GlesRenderer,
        img: image::RgbaImage,
    ) -> Result<Self, Box<dyn Error>> {
        let size: Size<i32, Buffer> = (img.width() as i32, img.height() as i32).into();
        // Texture en RGBA 8 bits (Fourcc ABGR8888 = octets R, G, B, A)
        let rgba = img.into_raw();
        let buffer = TextureBuffer::from_memory(
            renderer,
            &rgba,
            Fourcc::Abgr8888,
            size,
            false,
            1,
            Transform::Normal,
            Some(vec![Rectangle::from_size(size)]),
        )?;
        Ok(Self { buffer, size })
    }

    /// Élément couvrant une sortie de taille logique `output`, image centrée et recadrée.
    pub fn element(&self, output: Size<i32, Logical>) -> TextureRenderElement<GlesTexture> {
        self.element_alpha(output, 1.0)
    }

    /// Le même, à l'opacité `alpha` (fondu enchaîné)
    pub fn element_alpha(
        &self,
        output: Size<i32, Logical>,
        alpha: f32,
    ) -> TextureRenderElement<GlesTexture> {
        let (iw, ih) = (self.size.w as f64, self.size.h as f64);
        let (ow, oh) = (output.w as f64, output.h as f64);
        let k = (iw / ow).min(ih / oh);
        let (sw, sh) = (ow * k, oh * k);
        let src = Rectangle::new(((iw - sw) / 2.0, (ih - sh) / 2.0).into(), (sw, sh).into());
        TextureRenderElement::from_texture_buffer(
            (0.0, 0.0),
            &self.buffer,
            (alpha < 1.0).then_some(alpha),
            Some(src),
            Some(output),
            Kind::Unspecified,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_change_d_image() {
        let pool: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg"].map(PathBuf::from).into();
        for _ in 0..200 {
            let p = pick(&pool, Some(Path::new("b.jpg"))).unwrap();
            assert_ne!(p, PathBuf::from("b.jpg"));
        }
        // Une seule image : on la garde ; aucune : rien
        assert_eq!(
            pick(&pool[..1], Some(Path::new("a.jpg"))),
            Some(PathBuf::from("a.jpg"))
        );
        assert_eq!(pick(&[], None), None);
    }
}
