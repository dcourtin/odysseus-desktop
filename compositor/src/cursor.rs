//! Curseur dessiné par le compositeur (mode tty) : thème xcursor
//! (XCURSOR_THEME, Bibata par défaut, sinon Adwaita), ou la surface fournie par le client.

use std::io::Read;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    utils::{IsAlive, Logical, Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};
use xcursor::{CursorTheme, parser::parse_xcursor};

/// Images du thème chargées en plus de « default » (redimensionnement)
const NAMED: [&str; 4] = ["ew-resize", "ns-resize", "nwse-resize", "nesw-resize"];

pub struct Cursor {
    buffer: MemoryRenderBuffer,
    /// Point chaud, en pixels de l'image
    hotspot: Point<i32, Physical>,
    /// Autres images, par nom
    named: Vec<(&'static str, MemoryRenderBuffer, Point<i32, Physical>)>,
    /// Croix de visée (capture d'écran)
    crosshair: Option<(MemoryRenderBuffer, Point<i32, Physical>)>,
}

/// Créé une fois par image, consommé aussitôt : la taille des variantes importe peu
#[allow(clippy::large_enum_variant)]
pub enum CursorElement {
    Theme(MemoryRenderBufferRenderElement<GlesRenderer>),
    Surface(Vec<WaylandSurfaceRenderElement<GlesRenderer>>),
}

impl Cursor {
    /// Charge l'image « default » du thème, à la taille la plus proche de `size_px`.
    pub fn load(size_px: u32) -> Option<Self> {
        let (buffer, hotspot) = Self::load_icon("default", size_px)?;
        let named = NAMED
            .iter()
            .filter_map(|n| Self::load_icon(n, size_px).map(|(b, h)| (*n, b, h)))
            .collect();
        Some(Self {
            buffer,
            hotspot,
            named,
            crosshair: Self::load_icon("crosshair", size_px),
        })
    }

    fn load_icon(icon: &str, size_px: u32) -> Option<(MemoryRenderBuffer, Point<i32, Physical>)> {
        // Thème demandé, puis ceux qu'on trouve presque partout
        let wanted =
            std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Bibata-Modern-Classic".into());
        let path = [wanted.as_str(), "Adwaita", "default"]
            .iter()
            .find_map(|name| CursorTheme::load(name).load_icon(icon))?;
        let mut data = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .read_to_end(&mut data)
            .ok()?;
        let images = parse_xcursor(&data)?;
        let image = images
            .iter()
            .min_by_key(|img| (img.size as i32 - size_px as i32).abs())?;
        let buffer = MemoryRenderBuffer::from_slice(
            &image.pixels_rgba,
            Fourcc::Abgr8888,
            (image.width as i32, image.height as i32),
            1,
            Transform::Normal,
            None,
        );
        Some((buffer, (image.xhot as i32, image.yhot as i32).into()))
    }

    /// Éléments du curseur à la position logique `pos` (relative à la sortie).
    pub fn element(
        &self,
        renderer: &mut GlesRenderer,
        status: &mut CursorImageStatus,
        pos: Point<f64, Logical>,
        scale: f64,
        crosshair: bool,
    ) -> Option<CursorElement> {
        if crosshair && let Some((buffer, hotspot)) = self.crosshair.as_ref() {
            let loc = pos.to_physical(scale) - hotspot.to_f64();
            return MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                loc,
                buffer,
                None,
                None,
                None,
                Kind::Cursor,
            )
            .ok()
            .map(CursorElement::Theme);
        }
        if let CursorImageStatus::Surface(surface) = status
            && !surface.alive()
        {
            *status = CursorImageStatus::default_named();
        }
        match status {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Named(icon) => {
                let (buffer, hotspot) = self
                    .named
                    .iter()
                    .find(|(n, ..)| *n == icon.name())
                    .map_or((&self.buffer, self.hotspot), |(_, b, h)| (b, *h));
                let loc = pos.to_physical(scale) - hotspot.to_f64();
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc,
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .ok()
                .map(CursorElement::Theme)
            }
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<CursorImageAttributes>>()
                        .map(|attrs| attrs.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let loc = (pos - hotspot.to_f64()).to_physical(scale).to_i32_round();
                Some(CursorElement::Surface(render_elements_from_surface_tree(
                    renderer,
                    surface,
                    loc,
                    Scale::from(scale),
                    1.0,
                    Kind::Cursor,
                )))
            }
        }
    }
}
