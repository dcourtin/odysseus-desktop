//! Flou d'arrière-plan, façon macOS : ce qui passe sous la barre, le Dock et les
//! panneaux (et, sous les barres latérales des fenêtres d'odysseus, ce qui est
//! empilé sous chacune : autres fenêtres, fond d'écran) est flouté, puis dessiné sous les pixels non transparents de ces
//! surfaces (voir `render::BLUR_FRAG`).
//!
//! Méthode « double Kawase » : le dessous est rendu hors écran avec la même
//! projection que l'image finale (un pixel de l'écran = un texel, quel que soit
//! le backend), puis réduit de moitié quatre fois et ré-agrandi d'autant, avec
//! cinq ou huit lectures par pixel. Le résultat est gardé tant que rien ne
//! change dessous (identifiants, numéros de commit, positions) : au repos, aucun
//! calcul.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Frame, Offscreen, Renderer,
            damage::OutputDamageTracker,
            element::Element,
            gles::{
                GlesError, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
                UniformType,
            },
            utils::CommitCounter,
        },
    },
    reexports::wayland_server::backend::ObjectId,
    utils::{Buffer, Physical, Rectangle, Scale, Size, Transform},
};

use crate::render::OdysseusElement;

/// Réductions successives de moitié (1/2 … 1/16)
const LEVELS: usize = 4;

const DOWN_FRAG: &str = r#"#version 100
//_DEFINES_
precision highp float;
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform vec2 halfpixel;

void main() {
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords - halfpixel);
    sum += texture2D(tex, v_coords + halfpixel);
    sum += texture2D(tex, v_coords + vec2(halfpixel.x, -halfpixel.y));
    sum += texture2D(tex, v_coords - vec2(halfpixel.x, -halfpixel.y));
    gl_FragColor = sum / 8.0;
}
"#;

const UP_FRAG: &str = r#"#version 100
//_DEFINES_
precision highp float;
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform vec2 halfpixel;

void main() {
    vec4 sum = texture2D(tex, v_coords + vec2(-halfpixel.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(-halfpixel.x, halfpixel.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, halfpixel.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(halfpixel.x, halfpixel.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(halfpixel.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(halfpixel.x, -halfpixel.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, -halfpixel.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(-halfpixel.x, -halfpixel.y)) * 2.0;
    gl_FragColor = sum / 12.0;
}
"#;

/// Les flous de chaque écran : sous les surfaces du bureau (tout ce qui est
/// sous la couche du haut), et un par fenêtre translucide (ce qui est sous elle)
pub struct Blurs {
    pub desk: Blur,
    windows: HashMap<ObjectId, Blur>,
    down: GlesTexProgram,
    up: GlesTexProgram,
}

impl Blurs {
    pub fn new(renderer: &mut GlesRenderer) -> Result<Self, GlesError> {
        let uniforms = [UniformName::new("halfpixel", UniformType::_2f)];
        let down = renderer.compile_custom_texture_shader(DOWN_FRAG, &uniforms)?;
        let up = renderer.compile_custom_texture_shader(UP_FRAG, &uniforms)?;
        Ok(Self {
            desk: Blur::new(down.clone(), up.clone()),
            windows: HashMap::new(),
            down,
            up,
        })
    }

    /// Le flou propre à une fenêtre (sa surface), créé au premier usage
    pub fn window(&mut self, id: &ObjectId) -> &mut Blur {
        self.windows
            .entry(id.clone())
            .or_insert_with(|| Blur::new(self.down.clone(), self.up.clone()))
    }

    /// Libère les flous des fenêtres qui ne sont plus affichées
    pub fn keep_windows(&mut self, shown: &[ObjectId]) {
        self.windows.retain(|id, _| shown.contains(id));
    }
}

pub struct Blur {
    down: GlesTexProgram,
    up: GlesTexProgram,
    scene: Option<GlesTexture>,
    levels: Vec<GlesTexture>,
    size: Size<i32, Physical>,
    key: Option<u64>,
    generation: usize,
}

impl Blur {
    fn new(down: GlesTexProgram, up: GlesTexProgram) -> Self {
        Self {
            down,
            up,
            scene: None,
            levels: Vec::new(),
            size: Size::default(),
            key: None,
            generation: 0,
        }
    }

    /// Le dessous flouté (moitié de la taille de l'écran) et sa génération, qui
    /// change à chaque recalcul. Recalculé seulement si `elements` a changé.
    pub fn update(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[OdysseusElement],
        size: Size<i32, Physical>,
        scale: f64,
        transform: Transform,
    ) -> Option<(GlesTexture, usize)> {
        let key = signature(elements, size, scale, transform);
        if self.key != Some(key) || self.levels.is_empty() {
            if let Err(e) = self.compute(renderer, elements, size, scale, transform) {
                tracing::warn!("flou : {e}");
                self.key = None;
                return None;
            }
            self.key = Some(key);
            self.generation = self.generation.wrapping_add(1);
        }
        self.levels.first().map(|t| (t.clone(), self.generation))
    }

    fn compute(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[OdysseusElement],
        size: Size<i32, Physical>,
        scale: f64,
        transform: Transform,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let buffer =
            |s: Size<i32, Physical>| -> Size<i32, Buffer> { (s.w.max(1), s.h.max(1)).into() };
        let level_size = |i: usize| -> Size<i32, Physical> {
            ((size.w >> (i + 1)).max(1), (size.h >> (i + 1)).max(1)).into()
        };
        if self.size != size || self.scene.is_none() {
            self.scene = Some(renderer.create_buffer(Fourcc::Abgr8888, buffer(size))?);
            self.levels = (0..LEVELS)
                .map(|i| renderer.create_buffer(Fourcc::Abgr8888, buffer(level_size(i))))
                .collect::<Result<_, _>>()?;
            self.size = size;
        }
        // Le dessous, avec la projection de l'image finale
        let scene = self.scene.as_mut().ok_or("pas de texture")?;
        {
            let mut fb = renderer.bind(scene)?;
            let bg = common::theme::palette().background;
            OutputDamageTracker::new(size, scale, transform)
                .render_output(renderer, &mut fb, 0, elements, [bg.r, bg.g, bg.b, 1.0])
                .map_err(|e| format!("rendu du dessous : {e:?}"))?;
        }
        let scene = scene.clone();
        // Réductions, puis agrandissements
        let mut src = (scene, size);
        for i in 0..LEVELS {
            let dst = level_size(i);
            pass(
                renderer,
                &self.down,
                &src.0,
                src.1,
                &mut self.levels[i],
                dst,
            )?;
            src = (self.levels[i].clone(), dst);
        }
        for i in (0..LEVELS - 1).rev() {
            let dst = level_size(i);
            pass(renderer, &self.up, &src.0, src.1, &mut self.levels[i], dst)?;
            src = (self.levels[i].clone(), dst);
        }
        Ok(())
    }
}

/// Une passe : `src` (de taille `src_size`) dessinée dans `dst` par `program`
fn pass(
    renderer: &mut GlesRenderer,
    program: &GlesTexProgram,
    src: &GlesTexture,
    src_size: Size<i32, Physical>,
    dst: &mut GlesTexture,
    dst_size: Size<i32, Physical>,
) -> Result<(), GlesError> {
    let mut fb = renderer.bind(dst)?;
    let mut frame = renderer.render(&mut fb, dst_size, Transform::Normal)?;
    let full = Rectangle::from_size(dst_size);
    let halfpixel = [0.5 / src_size.w as f32, 0.5 / src_size.h as f32];
    frame.render_texture_from_to(
        src,
        Rectangle::from_size((src_size.w as f64, src_size.h as f64).into()),
        full,
        &[full],
        &[],
        Transform::Normal,
        1.0,
        Some(program),
        &[Uniform::new("halfpixel", halfpixel)],
    )?;
    // Même contexte GL que l'image finale : les passes s'enchaînent dans l'ordre
    let _ = frame.finish()?;
    Ok(())
}

/// Empreinte de ce qui est dessous : identifiants, commits, positions, opacités
fn signature(
    elements: &[OdysseusElement],
    size: Size<i32, Physical>,
    scale: f64,
    transform: Transform,
) -> u64 {
    let mut h = DefaultHasher::new();
    (size.w, size.h, scale.to_bits(), transform as u32).hash(&mut h);
    let s = Scale::from(scale);
    for e in elements {
        e.id().hash(&mut h);
        e.current_commit()
            .distance(Some(CommitCounter::from(0)))
            .hash(&mut h);
        let g = e.geometry(s);
        (g.loc.x, g.loc.y, g.size.w, g.size.h, e.alpha().to_bits()).hash(&mut h);
    }
    h.finish()
}
