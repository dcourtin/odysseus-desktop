//! Capture d'écran intégrée (mode tty) : la scène est redessinée dans une
//! texture hors écran, relue et enregistrée en PNG dans ~/Images.
//! Déclenchement : Super+Maj+3, ou présence du fichier
//! $XDG_RUNTIME_DIR/odysseus-capture (pratique pour les tests à distance).

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

pub fn save(
    renderer: &mut GlesRenderer,
    elements: &[OdysseusElement],
    size: Size<i32, Physical>,
    scale: f64,
) -> Result<PathBuf, Box<dyn Error>> {
    let buffer_size: Size<i32, Buffer> = (size.w, size.h).into();
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buffer_size)?;
    let mapping = {
        let mut framebuffer = renderer.bind(&mut texture)?;
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let bg = theme::BACKGROUND;
        tracker
            .render_output(
                renderer,
                &mut framebuffer,
                0,
                elements,
                [bg.r, bg.g, bg.b, 1.0],
            )
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
    let mut rgba = Vec::with_capacity(data.len());
    if !flipped {
        for row in data.chunks_exact(stride).rev() {
            rgba.extend_from_slice(row);
        }
    } else {
        rgba.extend_from_slice(data);
    }

    let dir = PathBuf::from(std::env::var("HOME")?).join("Images");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let path = dir.join(format!("capture-{stamp}.png"));
    image::save_buffer(
        &path,
        &rgba,
        size.w as u32,
        size.h as u32,
        image::ColorType::Rgba8,
    )?;
    Ok(path)
}
