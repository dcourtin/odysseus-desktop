//! Capture d'écran intégrée (mode tty) : la scène est redessinée dans une
//! texture hors écran, relue, recadrée et enregistrée en PNG dans ~/Images.
//! Déclenchement : voir screenshot.rs, ou présence du fichier
//! $XDG_RUNTIME_DIR/odysseus-capture (écran entier, pratique pour les tests).

use std::{error::Error, path::PathBuf};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen, TextureMapping,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Buffer, Physical, Rectangle, Size, Transform},
};

use crate::render::OdysseusElement;
use common::theme;

fn trigger_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("odysseus-capture"))
}

/// Vrai si une capture est demandée par fichier (le fichier est consommé).
pub fn requested_by_file() -> bool {
    trigger_file().is_some_and(|f| std::fs::remove_file(f).is_ok())
}

/// Dessine `elements` hors écran et renvoie l'image PNG (fichier dans ~/Images
/// et octets, pour le presse-papiers). `crop` : zone gardée, en pixels de la
/// sortie ; `transparent` : fond transparent (capture d'une fenêtre seule).
pub fn save(
    renderer: &mut GlesRenderer,
    elements: &[OdysseusElement],
    size: Size<i32, Physical>,
    scale: f64,
    crop: Option<Rectangle<i32, Physical>>,
    transparent: bool,
) -> Result<(PathBuf, Vec<u8>), Box<dyn Error>> {
    let png = render_png(renderer, elements, size, scale, crop, transparent)?;
    let dir = PathBuf::from(std::env::var("HOME")?).join("Images");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(crate::screenshot::file_name());
    std::fs::write(&path, &png)?;
    Ok((path, png))
}

/// Comme [`save`], sans rien enregistrer : les octets PNG seulement.
pub fn render_png(
    renderer: &mut GlesRenderer,
    elements: &[OdysseusElement],
    size: Size<i32, Physical>,
    scale: f64,
    crop: Option<Rectangle<i32, Physical>>,
    transparent: bool,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let buffer_size: Size<i32, Buffer> = (size.w, size.h).into();
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buffer_size)?;
    let mapping = {
        let mut framebuffer = renderer.bind(&mut texture)?;
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let bg = theme::palette().background;
        let clear = if transparent {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [bg.r, bg.g, bg.b, 1.0]
        };
        tracker
            .render_output(renderer, &mut framebuffer, 0, elements, clear)
            .map_err(|e| format!("rendu de la capture : {e:?}"))?;
        renderer.copy_framebuffer(
            &framebuffer,
            Rectangle::from_size(buffer_size),
            Fourcc::Abgr8888,
        )?
    };
    let flipped = mapping.flipped();
    let data = renderer.map_texture(&mapping)?;

    // glReadPixels rend les lignes de bas en haut, sauf si la texture est déjà retournée
    let stride = size.w as usize * 4;
    let rows: Vec<&[u8]> = if flipped {
        data.chunks_exact(stride).collect()
    } else {
        data.chunks_exact(stride).rev().collect()
    };
    // Zone gardée, bornée à l'écran
    let full = Rectangle::from_size(size);
    let area = crop.and_then(|c| c.intersection(full)).unwrap_or(full);
    let (x0, w) = (area.loc.x as usize, area.size.w as usize);
    let mut rgba = Vec::with_capacity(w * area.size.h as usize * 4);
    for row in rows
        .iter()
        .skip(area.loc.y as usize)
        .take(area.size.h as usize)
    {
        rgba.extend_from_slice(&row[x0 * 4..(x0 + w) * 4]);
    }
    // Pixels prémultipliés → alpha droit (le PNG l'attend)
    if transparent {
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    }

    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        &rgba,
        area.size.w as u32,
        area.size.h as u32,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}
