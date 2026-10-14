//! Fond d'écran : une image (JPEG ou PNG) chargée une fois en texture GPU, recadrée pour
//! couvrir la sortie sans déformation (comme « fill » dans sway).

use std::{error::Error, path::Path};

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

pub struct Wallpaper {
    buffer: TextureBuffer<GlesTexture>,
    size: Size<i32, Buffer>,
}

impl Wallpaper {
    pub fn load(renderer: &mut GlesRenderer, path: &Path) -> Result<Self, Box<dyn Error>> {
        let img = image::ImageReader::open(path)?
            .with_guessed_format()?
            .decode()?
            .into_rgba8();
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
        let (iw, ih) = (self.size.w as f64, self.size.h as f64);
        let (ow, oh) = (output.w as f64, output.h as f64);
        let k = (iw / ow).min(ih / oh);
        let (sw, sh) = (ow * k, oh * k);
        let src = Rectangle::new(((iw - sw) / 2.0, (ih - sh) / 2.0).into(), (sw, sh).into());
        TextureRenderElement::from_texture_buffer(
            (0.0, 0.0),
            &self.buffer,
            None,
            Some(src),
            Some(output),
            Kind::Unspecified,
        )
    }
}
