//! Protocoles d'appoint attendus par les navigateurs et les clients GL :
//! tampons GPU (linux-dmabuf), échelle fractionnaire et viewporter, sélection
//! primaire (clic du milieu), activation xdg, inhibition de veille, verrou.

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf, delegate_fractional_scale, delegate_idle_inhibit, delegate_primary_selection,
    delegate_session_lock, delegate_single_pixel_buffer, delegate_viewporter,
    delegate_xdg_activation,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::Transform,
    wayland::{
        compositor::{self, TraversalAction},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{self, FractionalScaleHandler},
        idle_inhibit::IdleInhibitHandler,
        selection::primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::Odysseus;

impl DmabufHandler for Odysseus {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    // Les formats annoncés sont ceux du moteur de rendu : l'import réel se fait
    // au premier affichage, un tampon invalide y est simplement ignoré.
    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<Odysseus>();
    }
}
delegate_dmabuf!(Odysseus);

impl FractionalScaleHandler for Odysseus {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.send_scale(&surface);
    }
}
delegate_fractional_scale!(Odysseus);
delegate_viewporter!(Odysseus);
delegate_single_pixel_buffer!(Odysseus);

impl PrimarySelectionHandler for Odysseus {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}
delegate_primary_selection!(Odysseus);

impl XdgActivationHandler for Odysseus {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    // Lien ouvert depuis le terminal, notification cliquée… : la fenêtre
    // visée passe au premier plan (les jetons trop vieux sont ignorés).
    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let window = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &surface))
            .cloned();
        if let Some(window) = window
            && token_data.timestamp.elapsed().as_secs() < 10
        {
            self.focus_window(&window);
        }
        self.activation_state.remove_token(&token);
    }
}
delegate_xdg_activation!(Odysseus);

impl IdleInhibitHandler for Odysseus {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.push(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.retain(|s| s != &surface);
    }
}
delegate_idle_inhibit!(Odysseus);

impl SessionLockHandler for Odysseus {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        self.on_lock(confirmation);
    }

    fn unlock(&mut self) {
        self.on_unlock();
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        self.on_new_lock_surface(surface);
    }
}
delegate_session_lock!(Odysseus);

impl Odysseus {
    /// Échelle de l'écran transmise à une surface et à ses sous-surfaces :
    /// entière (wl_surface v6) et fractionnaire (wp_fractional_scale).
    pub fn send_scale(&self, surface: &WlSurface) {
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let scale = output.current_scale();
        compositor::with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, states, _| {
                compositor::send_surface_state(
                    surface,
                    states,
                    scale.integer_scale(),
                    Transform::Normal,
                );
                fractional_scale::with_fractional_scale(states, |fractional| {
                    fractional.set_preferred_scale(scale.fractional_scale());
                });
            },
            |_, _, _| true,
        );
    }
}
