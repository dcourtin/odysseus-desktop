//! Verrouillage de session (ext-session-lock) et inactivité.
//!
//! Verrouillé, le compositeur n'affiche que la surface du verrou (odysseus-lock)
//! et ne lui transmet que le clavier et la souris : aucune fenêtre n'est visible.
//! Si le verrou plante, l'écran reste noir et verrouillé ; Super+L le relance.
//!
//! Inactivité (aucune entrée) : au bout de `ODYSSEUS_IDLE` secondes (600 par
//! défaut, comme macOS), l'écran s'éteint et la session se verrouille. Écran
//! verrouillé, il se rééteint après 30 s. Une vidéo en lecture (idle-inhibit)
//! suspend le compte à rebours.

use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::{element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer},
    output::Output,
    reexports::{
        calloop::{
            LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    utils::{SERIAL_COUNTER, Scale},
    wayland::session_lock::{LockSurface, SessionLocker},
};

use crate::{CalloopData, Odysseus, render::OdysseusElement};

/// Écran verrouillé : délai avant de le rééteindre
const LOCKED_SCREEN_OFF: Duration = Duration::from_secs(30);

#[derive(Default)]
pub enum LockState {
    #[default]
    Unlocked,
    /// Demande reçue : confirmée au client après une image noire affichée
    Locking(SessionLocker),
    Locked,
}

impl Odysseus {
    pub fn is_locked(&self) -> bool {
        !matches!(self.lock_state, LockState::Unlocked)
    }

    /// Surface du verrou (une seule sortie gérée).
    pub fn lock_surface(&self) -> Option<&LockSurface> {
        self.lock_surfaces.first()
    }

    /// Lance le verrou s'il n'est pas déjà affiché.
    pub fn lock_session(&mut self) {
        if self.lock_surface().is_some() {
            return;
        }
        self.spawn_sibling("odysseus-lock", &[]);
    }

    /// Appelée après chaque image : la première image noire confirme le verrouillage.
    pub fn confirm_lock(&mut self) {
        if let LockState::Locking(_) = self.lock_state
            && let LockState::Locking(locker) = std::mem::take(&mut self.lock_state)
        {
            locker.lock();
            self.lock_state = LockState::Locked;
        }
    }

    pub fn on_lock(&mut self, locker: SessionLocker) {
        self.lock_state = LockState::Locking(locker);
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(
            self,
            Option::<WlSurface>::None,
            SERIAL_COUNTER.next_serial(),
        );
    }

    pub fn on_unlock(&mut self) {
        self.lock_state = LockState::Unlocked;
        self.lock_surfaces.clear();
        self.focus_top_window();
    }

    pub fn on_new_lock_surface(&mut self, surface: LockSurface) {
        if let Some(geo) = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
        {
            surface.with_pending_state(|s| {
                s.size = Some((geo.size.w as u32, geo.size.h as u32).into());
            });
            surface.send_configure();
        }
        let wl = surface.wl_surface().clone();
        self.lock_surfaces.retain(|s| s.wl_surface().is_alive());
        self.lock_surfaces.push(surface);
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, Some(wl), SERIAL_COUNTER.next_serial());
    }

    /// Activité de l'utilisateur : rallume l'écran et relance le compte à rebours.
    pub fn note_activity(&mut self) {
        self.last_activity = Instant::now();
        if self.screen_off {
            self.screen_off = false;
            self.screen_power_changed = true;
        }
    }

    /// Une surface empêche la mise en veille (vidéo en cours)
    fn idle_inhibited(&mut self) -> bool {
        self.idle_inhibitors.retain(|s| s.is_alive());
        !self.idle_inhibitors.is_empty()
    }

    /// Vérifie l'inactivité toutes les 5 s.
    fn check_idle(&mut self) {
        if self.idle_inhibited() && !self.is_locked() {
            self.last_activity = Instant::now();
            return;
        }
        let idle = self.last_activity.elapsed();
        if self.is_locked() {
            if idle >= LOCKED_SCREEN_OFF && !self.screen_off {
                self.screen_off = true;
                self.screen_power_changed = true;
            }
        } else if idle >= idle_timeout() {
            self.lock_session();
            self.screen_off = true;
            self.screen_power_changed = true;
        }
    }
}

/// Délai d'inactivité avant extinction et verrouillage
fn idle_timeout() -> Duration {
    std::env::var("ODYSSEUS_IDLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(600))
}

pub fn init_idle_timer(handle: &LoopHandle<'static, CalloopData>) {
    let period = Duration::from_secs(5);
    handle
        .insert_source(Timer::from_duration(period), move |_, _, data| {
            data.state.check_idle();
            TimeoutAction::ToDuration(period)
        })
        .expect("minuteur d'inactivité");
}

/// Verrouillé : seule la surface du verrou est affichée (rien, avant qu'elle n'arrive).
pub fn lock_elements(
    state: &Odysseus,
    output: &Output,
    renderer: &mut GlesRenderer,
) -> Option<Vec<OdysseusElement>> {
    if !state.is_locked() {
        return None;
    }
    let scale = output.current_scale().fractional_scale();
    let Some(surface) = state.lock_surface() else {
        return Some(Vec::new());
    };
    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
            renderer,
            surface.wl_surface(),
            (0, 0),
            Scale::from(scale),
            1.0,
            smithay::backend::renderer::element::Kind::Unspecified,
        );
    Some(elements.into_iter().map(OdysseusElement::Layer).collect())
}

/// Rappels de rendu pour la surface du verrou.
pub fn send_lock_frame(state: &Odysseus, output: &Output) {
    if let Some(surface) = state.lock_surface() {
        let time = state.start_time.elapsed();
        smithay::desktop::utils::send_frames_surface_tree(
            surface.wl_surface(),
            output,
            time,
            Some(Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }
}
