//! Habillage des fenêtres : coins arrondis, liseré intérieur et ombre portée,
//! dessinés par des shaders GLES. Les rectangles sont passés aux shaders en
//! pixels du framebuffer et comparés à gl_FragCoord, ce qui marche quel que
//! soit le découpage en tuiles du client.

use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

use common::theme;

use crate::closing::Closing;
use smithay::{
    backend::renderer::{
        Renderer,
        element::{
            AsRenderElements, Element, Id, Kind, RenderElement,
            memory::MemoryRenderBufferRenderElement, surface::WaylandSurfaceRenderElement,
            texture::TextureRenderElement,
        },
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture,
            Uniform, UniformName, UniformType,
            element::{PixelShaderElement, TextureShaderElement},
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions, with_renderer_surface_state},
    },
    desktop::{Space, Window, layer_map_for_output},
    output::Output,
    reexports::wayland_server::{Resource, backend::ObjectId, protocol::wl_surface::WlSurface},
    render_elements,
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::shell::wlr_layer::Layer,
};

const ROUNDED_FRAG: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 geo;
uniform float radius;
uniform float border_width;
uniform vec4 border_color;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    // Seuls les pixels dans le rectangle de la fenêtre sont arrondis :
    // les popups qui en débordent restent intactes.
    vec2 half_size = geo.zw * 0.5;
    vec2 p = gl_FragCoord.xy - (geo.xy + half_size);
    if (abs(p.x) <= half_size.x && abs(p.y) <= half_size.y) {
        float d = rounded_box(p, half_size, radius);
        float inside = clamp(0.5 - d, 0.0, 1.0);
        float ring = inside - clamp(0.5 - (d + border_width), 0.0, 1.0);
        color = color * (1.0 - ring * border_color.a) + border_color * ring;
        color *= inside;
    }
    color *= alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

const SHADOW_FRAG: &str = r#"
precision highp float;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform vec2 size;
varying vec2 v_coords;

uniform vec4 geo;
uniform vec4 win;
uniform float radius;
uniform float blur;
uniform vec4 shadow_color;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 hs = geo.zw * 0.5;
    float d = rounded_box(gl_FragCoord.xy - (geo.xy + hs), hs, radius);
    float a = shadow_color.a * pow(1.0 - clamp(d / blur, 0.0, 1.0), 2.5);

    // Pas d'ombre sous la fenêtre elle-même : elle peut être translucide
    vec2 whs = win.zw * 0.5;
    float dw = rounded_box(gl_FragCoord.xy - (win.xy + whs), whs, radius);
    a *= clamp(dw + 0.5, 0.0, 1.0);

    gl_FragColor = vec4(shadow_color.rgb * a, a) * alpha;
}
"#;

/// Aperçu d'aimantation : rectangle arrondi translucide avec liseré
const PREVIEW_FRAG: &str = r#"
precision highp float;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform vec2 size;
varying vec2 v_coords;

uniform vec4 geo;
uniform float radius;
uniform float border_width;
uniform vec4 fill;
uniform vec4 border;

float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 hs = geo.zw * 0.5;
    float d = rounded_box(gl_FragCoord.xy - (geo.xy + hs), hs, radius);
    float inside = clamp(0.5 - d, 0.0, 1.0);
    float ring = inside - clamp(0.5 - (d + border_width), 0.0, 1.0);
    vec4 color = fill * (inside - ring) + border * ring;
    gl_FragColor = color * alpha;
}
"#;

/// Programmes compilés une fois au démarrage
pub struct Shaders {
    rounded: GlesTexProgram,
    shadow: GlesPixelProgram,
    preview: GlesPixelProgram,
}

impl Shaders {
    pub fn compile(renderer: &mut GlesRenderer) -> Result<Self, GlesError> {
        let rounded = renderer.compile_custom_texture_shader(
            ROUNDED_FRAG,
            &[
                UniformName::new("geo", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("border_width", UniformType::_1f),
                UniformName::new("border_color", UniformType::_4f),
            ],
        )?;
        let shadow = renderer.compile_custom_pixel_shader(
            SHADOW_FRAG,
            &[
                UniformName::new("geo", UniformType::_4f),
                UniformName::new("win", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("blur", UniformType::_1f),
                UniformName::new("shadow_color", UniformType::_4f),
            ],
        )?;
        let preview = renderer.compile_custom_pixel_shader(
            PREVIEW_FRAG,
            &[
                UniformName::new("geo", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("border_width", UniformType::_1f),
                UniformName::new("fill", UniformType::_4f),
                UniformName::new("border", UniformType::_4f),
            ],
        )?;
        Ok(Self {
            rounded,
            shadow,
            preview,
        })
    }
}

/// Surface de client dessinée avec le shader à coins arrondis.
pub struct RoundedSurface {
    inner: WaylandSurfaceRenderElement<GlesRenderer>,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
}

impl Element for RoundedSurface {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }
    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    /// Les coins sont transparents : aucune zone n'est garantie opaque.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for RoundedSurface {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
        );
        frame.clear_tex_program_override();
        result
    }
}

render_elements! {
    /// Un élément par image : la différence de taille entre variantes est sans importance
    #[allow(clippy::large_enum_variant)]
    pub OdysseusElement<=GlesRenderer>;
    Surface=RoundedSurface,
    Shadow=PixelShaderElement,
    Layer=WaylandSurfaceRenderElement<GlesRenderer>,
    Closing=TextureShaderElement,
    Wallpaper=TextureRenderElement<GlesTexture>,
    CursorTheme=MemoryRenderBufferRenderElement<GlesRenderer>,
}

/// Contenu (coins arrondis) et ombre d'une fenêtre, à sa position animée :
/// trajet en cours (aimantation, agrandissement…), ouverture, glissement de bureau.
fn window_elements(
    window: &Window,
    loc: Point<i32, Logical>,
    dx: f64,
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    frame: &Frame,
    scene: &Scene,
) -> Vec<OdysseusElement> {
    let mut out = Vec::new();
    // Fenêtre démappée (sans tampon) : ni contenu ni ombre
    let has_buffer = window.toplevel().is_some_and(|t| {
        with_renderer_surface_state(t.wl_surface(), |s| s.buffer().is_some()).unwrap_or(false)
    });
    if !has_buffer {
        return out;
    }
    let geometry = window.geometry();
    let id = window.toplevel().map(|t| t.wl_surface().id());
    // Trajet en cours, sinon place réelle
    let base = id
        .as_ref()
        .and_then(|id| scene.motions.get(id))
        .filter(|m| !m.done())
        .map(|m| m.current())
        .unwrap_or_else(|| Rectangle::new(loc, geometry.size).to_f64());
    // Ouverture : fondu et zoom de 94 % à 100 % autour du centre
    let progress = open_progress(id.as_ref().and_then(|id| scene.opening.get(id)));
    let zoom = 0.94 + 0.06 * progress;
    let alpha = progress as f32;
    let size = base.size;
    let center = base.loc + Point::from((size.w / 2.0 + dx, size.h / 2.0));
    let geo = Rectangle::new(
        center - Point::from((size.w * zoom / 2.0, size.h * zoom / 2.0)),
        (size.w * zoom, size.h * zoom).into(),
    );
    // Échelle du contenu : la taille affichée rapportée à celle du tampon
    let sx = geo.size.w / geometry.size.w.max(1) as f64;
    let sy = geo.size.h / geometry.size.h.max(1) as f64;
    let is_focused = scene
        .focused
        .is_some_and(|f| window.toplevel().is_some_and(|t| t.wl_surface() == f));

    let uniforms = frame.rounded_uniforms(geo);
    let render_loc = (geo.loc
        - Point::from((geometry.loc.x as f64 * sx, geometry.loc.y as f64 * sy)))
    .to_physical(frame.scale)
    .to_i32_round();
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = window.render_elements(
        renderer,
        render_loc,
        Scale::from((frame.scale * sx, frame.scale * sy)),
        alpha,
    );
    out.extend(surfaces.into_iter().map(|inner| {
        OdysseusElement::Surface(RoundedSurface {
            inner,
            program: shaders.rounded.clone(),
            uniforms: uniforms.clone(),
        })
    }));
    // Ombre portée, plus marquée sur la fenêtre active
    let shadow_alpha = if is_focused {
        theme::SHADOW_ALPHA_FOCUSED
    } else {
        theme::SHADOW_ALPHA
    };
    out.push(frame.shadow(shaders, geo, alpha * shadow_alpha));
    out
}

/// Surfaces d'une couche (barre, fond…), de la plus haute à la plus basse.
fn layer_elements(
    output: &Output,
    renderer: &mut GlesRenderer,
    scale: f64,
    layer: Layer,
) -> Vec<OdysseusElement> {
    let map = layer_map_for_output(output);
    let mut elements = Vec::new();
    for surface in map.layers_on(layer).rev() {
        let Some(geo) = map.layer_geometry(surface) else {
            continue;
        };
        let loc = geo.loc.to_physical_precise_round(scale);
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            surface.render_elements(renderer, loc, Scale::from(scale), 1.0);
        elements.extend(surfaces.into_iter().map(OdysseusElement::Layer));
    }
    elements
}

/// Durée de l'animation d'ouverture. `ODYSSEUS_ANIM_SLOW=10` la ralentit
/// dix fois, pour régler les animations à l'œil.
pub fn open_duration() -> Duration {
    static SLOW: OnceLock<f64> = OnceLock::new();
    let slow = *SLOW.get_or_init(|| {
        std::env::var("ODYSSEUS_ANIM_SLOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0)
    });
    Duration::from_millis(260).mul_f64(slow)
}

/// Durée de l'animation de fermeture (ralentie aussi par ODYSSEUS_ANIM_SLOW)
pub fn close_duration() -> Duration {
    open_duration().mul_f64(200.0 / 260.0)
}

/// Avancement de la fermeture, 0 → 1, courbe ease-out cubique.
fn close_progress(start: &Instant) -> f64 {
    let t = (start.elapsed().as_secs_f64() / close_duration().as_secs_f64()).min(1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Avancement de l'ouverture, 0 → 1, courbe ease-out cubique.
fn open_progress(start: Option<&Instant>) -> f64 {
    let Some(start) = start else {
        return 1.0;
    };
    let t = (start.elapsed().as_secs_f64() / open_duration().as_secs_f64()).min(1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Rectangle logique → pixels du framebuffer tels que les voit gl_FragCoord, [x, y, w, h].
/// La transformation de la sortie porte déjà le sens de l'axe y : rangée 0 en haut
/// de l'écran pour DRM (`Normal`), en bas pour une fenêtre EGL (`Flipped180`).
fn gl_rect(
    rect: Rectangle<f64, Logical>,
    scale: f64,
    transform: Transform,
    fb: Size<i32, Physical>,
) -> [f32; 4] {
    let r = rect.to_physical(scale);
    let r = transform.transform_rect_in(r, &fb.to_f64());
    [
        r.loc.x as f32,
        r.loc.y as f32,
        r.size.w as f32,
        r.size.h as f32,
    ]
}

/// Rectangle `rect` réduit/agrandi d'un facteur `zoom` autour de son centre.
fn zoom_around_center(rect: Rectangle<i32, Logical>, zoom: f64) -> Rectangle<f64, Logical> {
    let size = rect.size.to_f64();
    let center = rect.loc.to_f64() + Point::from((size.w / 2.0, size.h / 2.0));
    let zoomed: Size<f64, Logical> = (size.w * zoom, size.h * zoom).into();
    Rectangle::new(
        center - Point::from((zoomed.w / 2.0, zoomed.h / 2.0)),
        zoomed,
    )
}

/// Paramètres communs aux éléments d'une sortie
struct Frame {
    scale: f64,
    transform: Transform,
    fb_size: Size<i32, Physical>,
    radius: f32,
    border_color: [f32; 4],
}

impl Frame {
    fn rounded_uniforms(&self, geo: Rectangle<f64, Logical>) -> Vec<Uniform<'static>> {
        vec![
            Uniform::new(
                "geo",
                gl_rect(geo, self.scale, self.transform, self.fb_size),
            ),
            Uniform::new("radius", self.radius),
            Uniform::new("border_width", self.scale as f32),
            Uniform::new("border_color", self.border_color),
        ]
    }

    /// Ombre portée sous `geo`, décalée vers le bas, d'opacité `alpha`.
    fn shadow(
        &self,
        shaders: &Shaders,
        geo: Rectangle<f64, Logical>,
        alpha: f32,
    ) -> OdysseusElement {
        let blur = theme::SHADOW_BLUR as f64;
        let shadow_rect = Rectangle::new(
            geo.loc + Point::from((0.0, theme::SHADOW_OFFSET_Y as f64)),
            geo.size,
        );
        let area = Rectangle::new(
            shadow_rect.loc - Point::from((blur, blur)),
            shadow_rect.size + Size::from((2.0 * blur, 2.0 * blur)),
        )
        .to_i32_round();
        OdysseusElement::Shadow(PixelShaderElement::new(
            shaders.shadow.clone(),
            area,
            None,
            1.0,
            vec![
                Uniform::new(
                    "geo",
                    gl_rect(shadow_rect, self.scale, self.transform, self.fb_size),
                ),
                Uniform::new(
                    "win",
                    gl_rect(geo, self.scale, self.transform, self.fb_size),
                ),
                Uniform::new("radius", self.radius),
                Uniform::new("blur", (blur * self.scale) as f32),
                Uniform::new("shadow_color", [0.0, 0.0, 0.0, alpha]),
            ],
            Kind::Unspecified,
        ))
    }
}

/// État animé de la scène, fourni par le compositeur à chaque image
pub struct Scene<'a> {
    pub focused: Option<&'a WlSurface>,
    pub opening: &'a HashMap<ObjectId, Instant>,
    pub closing: &'a [Closing],
    /// Fenêtre plein écran au premier plan : la barre s'efface
    pub hide_top_layer: bool,
    pub motions: &'a HashMap<ObjectId, crate::anim::Motion>,
    pub slide: Option<&'a crate::anim::WorkspaceSlide>,
    pub snap_preview: Option<crate::anim::SnapPreview>,
}

/// Éléments à dessiner pour une sortie, du plus haut au plus bas.
#[allow(clippy::too_many_arguments)]
pub fn output_elements(
    space: &Space<Window>,
    output: &Output,
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    fb_size: Size<i32, Physical>,
    wallpaper: Option<&crate::wallpaper::Wallpaper>,
    scene: &Scene,
) -> Vec<OdysseusElement> {
    let scale = output.current_scale().fractional_scale();
    let b = theme::WINDOW_BORDER;
    let frame = Frame {
        scale,
        transform: output.current_transform(),
        fb_size,
        radius: (theme::WINDOW_RADIUS as f64 * scale) as f32,
        border_color: [b.r * b.a, b.g * b.a, b.b * b.a, b.a],
    };
    let context = renderer.context_id();
    let out_w = space
        .output_geometry(output)
        .map(|g| g.size.w as f64)
        .unwrap_or(1920.0);

    let mut elements = layer_elements(output, renderer, scale, Layer::Overlay);
    if !scene.hide_top_layer {
        elements.extend(layer_elements(output, renderer, scale, Layer::Top));
    }

    // Fenêtres en cours de fermeture : fondu et rétrécissement de 100 % à 94 %
    for c in scene.closing.iter().rev() {
        let t = close_progress(&c.start);
        let zoom = 1.0 - 0.06 * t;
        let alpha = (1.0 - t) as f32;
        let geo = zoom_around_center(c.geo, zoom);
        let loc = (geo.loc + c.offset.to_f64().upscale(zoom)).to_physical(scale);
        let size: Size<i32, Logical> = (c.size.to_f64().upscale(zoom)).to_i32_round();
        let texture = TextureRenderElement::from_static_texture(
            c.id.clone(),
            context.clone(),
            loc,
            c.texture.clone(),
            c.buffer_scale,
            c.transform,
            Some(alpha),
            Some(c.src),
            Some(size),
            None,
            Kind::Unspecified,
        );
        elements.push(OdysseusElement::Closing(TextureShaderElement::new(
            texture,
            shaders.rounded.clone(),
            frame.rounded_uniforms(geo),
        )));
        elements.push(frame.shadow(shaders, geo, alpha * theme::SHADOW_ALPHA));
    }

    // Changement de bureau : décalage des fenêtres arrivantes et quittées
    let (out_off, in_off) = scene.slide.map(|s| s.offsets()).unwrap_or((0.0, 0.0));
    let windows: Vec<(Window, Point<i32, Logical>)> = space
        .elements()
        .filter_map(|w| space.element_location(w).map(|l| (w.clone(), l)))
        .collect();
    for (i, (window, loc)) in windows.iter().rev().enumerate() {
        elements.extend(window_elements(
            window,
            *loc,
            in_off * out_w,
            renderer,
            shaders,
            &frame,
            scene,
        ));
        // L'aperçu d'aimantation se glisse juste sous la fenêtre déplacée (au-dessus)
        if i == 0
            && let Some(preview) = scene.snap_preview
        {
            let (rect, t) = preview.current();
            let accent = theme::ACCENT;
            elements.push(OdysseusElement::Shadow(PixelShaderElement::new(
                shaders.preview.clone(),
                rect.to_i32_round(),
                None,
                1.0,
                vec![
                    Uniform::new("geo", gl_rect(rect, scale, frame.transform, fb_size)),
                    Uniform::new("radius", frame.radius),
                    Uniform::new("border_width", (1.5 * scale) as f32),
                    Uniform::new(
                        "fill",
                        [
                            accent.r * 0.22 * t,
                            accent.g * 0.22 * t,
                            accent.b * 0.22 * t,
                            0.22 * t,
                        ],
                    ),
                    Uniform::new(
                        "border",
                        [
                            accent.r * 0.7 * t,
                            accent.g * 0.7 * t,
                            accent.b * 0.7 * t,
                            0.7 * t,
                        ],
                    ),
                ],
                Kind::Unspecified,
            )));
        }
    }
    if let Some(slide) = scene.slide {
        for (window, loc) in slide.outgoing.iter().rev() {
            elements.extend(window_elements(
                window,
                *loc,
                out_off * out_w,
                renderer,
                shaders,
                &frame,
                scene,
            ));
        }
    }
    elements.extend(layer_elements(output, renderer, scale, Layer::Bottom));
    elements.extend(layer_elements(output, renderer, scale, Layer::Background));
    if let (Some(wallpaper), Some(geo)) = (wallpaper, space.output_geometry(output)) {
        elements.push(OdysseusElement::Wallpaper(wallpaper.element(geo.size)));
    }
    elements
}
