//! Ouverture de session : le bureau ne se monte pas pièce par pièce.
//!
//! Dès la première image, le fond d'écran paraît sous un voile de la couleur du
//! thème, avec « odysseus » et un indicateur à points qui tourne ; la barre et
//! le Dock, qui se lancent, restent cachés. Quand ils ont tous deux leur
//! surface (ou au bout de `MAX_WAIT`), le voile et le logo s'effacent en fondu
//! pendant que la barre et le Dock apparaissent. `ODYSSEUS_SPLASH=0` s'en passe.
//!
//! Session ouverte verrouillée (le verrou sert d'écran de connexion) : tout
//! attend le mot de passe ; au déverrouillage, seul le voile s'efface, sans logo.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Text},
    theme,
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::{
            Kind,
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
        },
        renderer::gles::GlesRenderer,
    },
    desktop::{Space, Window, layer_map_for_output},
    utils::{Logical, Point, Transform},
};

use crate::render::OdysseusElement;

/// Surfaces attendues avant de montrer le bureau (le Dock, s'il est activé)
const BAR: &str = "odysseus-bar";
const DOCK: &str = "odysseus-dock";
/// Attente au plus (un client absent ou planté ne bloque pas la session)
const MAX_WAIT: Duration = Duration::from_secs(4);
/// Une fois les surfaces là, le temps qu'elles reçoivent leur première image
const SETTLE: Duration = Duration::from_millis(150);
/// Apparition du logo, puis fondu final
const FADE_IN: Duration = Duration::from_millis(250);
const REVEAL: Duration = Duration::from_millis(450);
/// Opacité du voile pendant l'attente
const VEIL: f32 = 0.82;

pub struct Splash {
    start: Instant,
    /// Surfaces attendues présentes depuis…
    ready_since: Option<Instant>,
    /// Début du fondu final
    reveal: Option<Instant>,
    /// Logo et indicateur (pas après un écran de connexion)
    logo: bool,
}

impl Splash {
    pub fn new() -> Option<Self> {
        (std::env::var("ODYSSEUS_SPLASH").as_deref() != Ok("0")).then(|| Self {
            start: Instant::now(),
            ready_since: None,
            reveal: None,
            logo: true,
        })
    }

    /// Après chaque image ; vrai quand c'est fini (le bureau est montré)
    pub fn update(&mut self, space: &Space<Window>, locked: bool, dock: bool) -> bool {
        if locked {
            self.logo = false;
            self.ready_since = None;
            return false;
        }
        if self.reveal.is_none() {
            let ready = space.outputs().next().is_some()
                && space.outputs().all(|o| {
                    let map = layer_map_for_output(o);
                    let has = |ns: &str| map.layers().any(|l| l.namespace() == ns);
                    has(BAR) && (!dock || has(DOCK))
                });
            self.ready_since = if ready {
                self.ready_since.or(Some(Instant::now()))
            } else {
                None
            };
            let settled = self.ready_since.is_some_and(|t| t.elapsed() >= SETTLE);
            if settled || self.start.elapsed() >= MAX_WAIT {
                self.reveal = Some(Instant::now());
            }
        }
        self.reveal_progress() >= 1.0
    }

    /// Avancement du fondu final, 0 → 1 (0 pendant l'attente)
    fn reveal_progress(&self) -> f64 {
        self.reveal.map_or(0.0, |t| {
            crate::anim::progress(t, REVEAL.mul_f64(crate::anim::slow()))
        })
    }

    /// Opacité de la barre et du Dock (couche du haut)
    pub fn top_alpha(&self) -> f32 {
        ease_out(self.reveal_progress()) as f32
    }

    /// Opacité du voile (0 → plus rien à dessiner)
    pub fn veil_alpha(&self) -> f32 {
        VEIL * (1.0 - ease_out(self.reveal_progress())) as f32
    }

    /// Logo et indicateur, centrés sur l'écran (`center`, pixels logiques)
    pub fn logo_element(
        &self,
        renderer: &mut GlesRenderer,
        center: Point<f64, Logical>,
        scale: f64,
    ) -> Option<OdysseusElement> {
        if !self.logo {
            return None;
        }
        let fade_in = crate::anim::progress(self.start, FADE_IN) as f32;
        let t = ease_out(self.reveal_progress());
        let alpha = fade_in * (1.0 - t as f32);
        if alpha <= 0.0 {
            return None;
        }
        let font = font()?;
        let s = scale as f32;
        let (w, h) = ((220.0 * s).ceil() as u32, (110.0 * s).ceil() as u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        let mut c = Canvas {
            buf: &mut data,
            width: w,
            height: h,
        };
        let p = theme::palette();
        let cx = w as f32 / 2.0;
        font.draw_centered(&mut c, "odysseus", cx, 0.0, 44.0 * s, 26.0 * s, p.text);
        // Huit points en cercle ; le plus vif tourne
        let turn = self.start.elapsed().as_secs_f32() * 1.25;
        let (sy, r) = (78.0 * s, 13.0 * s);
        for k in 0..8 {
            let a = k as f32 / 8.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let lag = (turn - k as f32 / 8.0).rem_euclid(1.0);
            let dot = 0.18 + 0.82 * (1.0 - lag).powi(2);
            c.fill_circle(
                cx + r * a.cos(),
                sy + r * a.sin(),
                2.6 * s,
                p.text.with_alpha(dot),
            );
        }
        let buffer = MemoryRenderBuffer::from_slice(
            &data,
            Fourcc::Argb8888,
            (w as i32, h as i32),
            1,
            Transform::Normal,
            None,
        );
        // Au fondu, le logo monte un peu en s'effaçant
        let lift = 12.0 * t;
        let loc = (center - Point::from((0.0, 20.0 + lift))).to_physical(scale)
            - Point::from((w as f64 / 2.0, h as f64 / 2.0));
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc,
            &buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
        .map(OdysseusElement::CursorTheme)
    }
}

fn font() -> Option<&'static Text> {
    static FONT: OnceLock<Option<Text>> = OnceLock::new();
    FONT.get_or_init(|| Text::load(theme::FONT_BOLD).ok())
        .as_ref()
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}
