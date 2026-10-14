//! Animation de fermeture : au moment où le client détruit sa fenêtre, on garde
//! la dernière texture de sa surface principale (encore en mémoire GPU) et on la
//! fait disparaître en fondu et en léger rétrécissement.

use std::time::Instant;

use smithay::{
    backend::renderer::{
        ContextId, element::Id, gles::GlesTexture, utils::with_renderer_surface_state,
    },
    desktop::Window,
    utils::{Logical, Point, Rectangle, Size, Transform},
};

pub struct Closing {
    /// Identifiant stable d'une image à l'autre (suivi des dégâts)
    pub id: Id,
    pub texture: GlesTexture,
    pub buffer_scale: i32,
    pub transform: Transform,
    pub src: Rectangle<f64, Logical>,
    pub size: Size<i32, Logical>,
    /// Géométrie de la fenêtre dans l'espace au moment de la fermeture
    pub geo: Rectangle<i32, Logical>,
    /// Position de la surface principale par rapport à `geo.loc`
    pub offset: Point<i32, Logical>,
    pub start: Instant,
}

impl Closing {
    pub fn capture(
        window: &Window,
        loc: Point<i32, Logical>,
        context: &ContextId<GlesTexture>,
    ) -> Option<Self> {
        let surface = window.toplevel()?.wl_surface().clone();
        let geometry = window.geometry();
        with_renderer_surface_state(&surface, |state| {
            let texture = state.texture::<GlesTexture>(context.clone())?.clone();
            let view = state.view()?;
            Some(Closing {
                id: Id::new(),
                texture,
                buffer_scale: state.buffer_scale(),
                transform: state.buffer_transform(),
                src: view.src,
                size: view.dst,
                geo: Rectangle::new(loc, geometry.size),
                offset: view.offset - geometry.loc,
                start: Instant::now(),
            })
        })
        .flatten()
    }
}
