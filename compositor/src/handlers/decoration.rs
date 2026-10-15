//! Décorations : le compositeur habille toutes les fenêtres (coins, ombres) et
//! dessine la barre de titre de celles qui le lui demandent (voir decor.rs).

use smithay::{
    delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    reexports::wayland_server::Resource,
    wayland::shell::xdg::{ToplevelSurface, decoration::XdgDecorationHandler},
};

use crate::Odysseus;

/// Mode de décoration d'une fenêtre : côté serveur (le compositeur dessine sa
/// barre de titre), sauf si le client demande à décorer lui-même (applis
/// d'odysseus, qui dessinent leurs boutons)
fn set_mode(state: &mut Odysseus, toplevel: &ToplevelSurface, mode: Mode) {
    toplevel.with_pending_state(|s| {
        s.decoration_mode = Some(mode);
    });
    let id = toplevel.wl_surface().id();
    if mode == Mode::ServerSide {
        state.ssd.insert(id);
    } else {
        state.ssd.remove(&id);
    }
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

impl XdgDecorationHandler for Odysseus {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        set_mode(self, &toplevel, Mode::ServerSide);
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        set_mode(self, &toplevel, mode);
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_mode(self, &toplevel, Mode::ServerSide);
    }
}

delegate_xdg_decoration!(Odysseus);
