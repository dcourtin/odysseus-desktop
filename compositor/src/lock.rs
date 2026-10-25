//! Verrouillage de session (ext-session-lock) et inactivité.
//!
//! Verrouillé, le compositeur n'affiche que la surface du verrou (odysseus-lock)
//! et ne lui transmet que le clavier et la souris : aucune fenêtre n'est visible.
//! Si le verrou plante, l'écran reste noir et verrouillé ; Super+L le relance.
//!
//! Inactivité (aucune entrée) : au bout du délai choisi dans Réglages > Énergie
//! (10 min par défaut, comme macOS ; `ODYSSEUS_IDLE` en secondes sans fichier
//! de réglages), l'écran s'éteint et, si demandé, la session se verrouille ;
//! plus tard, éventuellement, le système se met en veille. Écran verrouillé,
//! il se rééteint après 30 s. Une vidéo en lecture (idle-inhibit) suspend le
//! compte à rebours.

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
    /// Ouverture de session (`ODYSSEUS_VERROU=1`) : verrouillé avant même que le
    /// verrou ne se connecte ; écran noir, aucune entrée transmise
    Pending,
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

    /// Ouverture de session verrouillée : rien du bureau n'est montré avant que
    /// le mot de passe ait été donné au verrou.
    pub fn start_locked(&mut self) {
        self.lock_state = LockState::Pending;
        self.lock_session();
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
        if idle < Duration::from_secs(5) {
            self.idle_suspended = false;
        }
        // Mise en veille du système (Réglages > Énergie), une fois par période
        let suspend = self.settings.suspend_min;
        if suspend > 0 && idle >= Duration::from_secs(suspend as u64 * 60) && !self.idle_suspended {
            self.idle_suspended = true;
            if self.settings.lock_on_idle && !self.is_locked() {
                self.lock_session();
            }
            tracing::info!("veille automatique");
            self.spawn("systemctl suspend");
            return;
        }
        if self.is_locked() {
            if idle >= LOCKED_SCREEN_OFF && !self.screen_off {
                self.screen_off = true;
                self.screen_power_changed = true;
            }
        } else if let Some(timeout) = self.idle_timeout()
            && idle >= timeout
            && !self.screen_off
        {
            if self.settings.lock_on_idle {
                self.lock_session();
            }
            self.screen_off = true;
            self.screen_power_changed = true;
        }
    }

    /// Délai d'inactivité avant extinction de l'écran (et verrouillage) :
    /// Réglages > Énergie (aucun : jamais), sinon `ODYSSEUS_IDLE` en secondes
    fn idle_timeout(&self) -> Option<Duration> {
        if let Some(secs) = std::env::var("ODYSSEUS_IDLE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            && common::settings::path().is_none_or(|p| !p.exists())
        {
            return Some(Duration::from_secs(secs));
        }
        let min = self.settings.screen_off_min;
        (min > 0).then(|| Duration::from_secs(min as u64 * 60))
    }
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
