//! Animations du compositeur, toutes pilotées par le temps (une image = un
//! état calculé) et ralenties ensemble par `ODYSSEUS_ANIM_SLOW` :
//! - mouvement d'une fenêtre d'un rectangle à un autre (aimantation, agrandir,
//!   restaurer) : le contenu est mis à l'échelle pendant le trajet ;
//! - changement de bureau : tout le bureau glisse latéralement ;
//! - aperçu d'aimantation : la zone visée grandit sous la fenêtre déplacée.

use std::time::{Duration, Instant};

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle},
};

/// Facteur de ralenti global (ODYSSEUS_ANIM_SLOW, 1 par défaut)
pub fn slow() -> f64 {
    static SLOW: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SLOW.get_or_init(|| {
        std::env::var("ODYSSEUS_ANIM_SLOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0)
    })
}

pub fn motion_duration() -> Duration {
    Duration::from_millis(240).mul_f64(slow())
}

pub fn slide_duration() -> Duration {
    Duration::from_millis(340).mul_f64(slow())
}

pub fn preview_duration() -> Duration {
    Duration::from_millis(160).mul_f64(slow())
}

/// Avancement 0 → 1 depuis `start`
pub fn progress(start: Instant, duration: Duration) -> f64 {
    (start.elapsed().as_secs_f64() / duration.as_secs_f64().max(1e-6)).min(1.0)
}

/// Ralenti en fin de course, léger et sans rebond (comme un ressort amorti)
pub fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(4)
}

pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

pub fn lerp_rect(
    a: Rectangle<f64, Logical>,
    b: Rectangle<f64, Logical>,
    t: f64,
) -> Rectangle<f64, Logical> {
    Rectangle::new(
        (lerp(a.loc.x, b.loc.x, t), lerp(a.loc.y, b.loc.y, t)).into(),
        (lerp(a.size.w, b.size.w, t), lerp(a.size.h, b.size.h, t)).into(),
    )
}

/// Trajet d'une fenêtre : de `from` (rectangle affiché au départ) à `to`
/// (place et taille demandées).
#[derive(Clone, Copy)]
pub struct Motion {
    pub from: Rectangle<f64, Logical>,
    pub to: Rectangle<f64, Logical>,
    pub start: Instant,
}

impl Motion {
    pub fn current(&self) -> Rectangle<f64, Logical> {
        lerp_rect(
            self.from,
            self.to,
            ease_out(progress(self.start, motion_duration())),
        )
    }

    pub fn done(&self) -> bool {
        self.start.elapsed() >= motion_duration()
    }
}

/// Changement de bureau : les fenêtres quittées glissent hors de l'écran,
/// les nouvelles arrivent de l'autre côté.
pub struct WorkspaceSlide {
    pub outgoing: Vec<(Window, Point<i32, Logical>)>,
    /// +1 : vers la droite (bureau suivant), -1 : vers la gauche
    pub direction: f64,
    pub start: Instant,
}

impl WorkspaceSlide {
    /// Décalages horizontaux (en largeurs d'écran) : (quittées, arrivantes)
    pub fn offsets(&self) -> (f64, f64) {
        let t = ease_out(progress(self.start, slide_duration()));
        (-self.direction * t, self.direction * (1.0 - t))
    }

    pub fn done(&self) -> bool {
        self.start.elapsed() >= slide_duration()
    }
}

/// Aperçu de la zone d'aimantation pendant un déplacement
#[derive(Clone, Copy)]
pub struct SnapPreview {
    pub zone: Rectangle<i32, Logical>,
    /// Rectangle de départ de l'animation (la fenêtre déplacée)
    pub from: Rectangle<f64, Logical>,
    pub start: Instant,
}

impl SnapPreview {
    pub fn current(&self) -> (Rectangle<f64, Logical>, f32) {
        let t = ease_out(progress(self.start, preview_duration()));
        (lerp_rect(self.from, self.zone.to_f64(), t), t as f32)
    }
}
