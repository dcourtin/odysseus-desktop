use crate::{Odysseus, grabs::resize_grab, state::ClientState};
use smithay::reexports::wayland_server::Resource;
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, get_parent, is_sync_subsurface, with_states,
        },
        shm::{ShmHandler, ShmState},
    },
};

use super::xdg_shell;

impl CompositorHandler for Odysseus {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Avant que la texture ne soit libérée par un tampon vide (fenêtre démappée)
        self.capture_if_unmapping(surface);
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                window.on_commit();
            }
            self.place_if_new(&root);
        };

        xdg_shell::handle_commit(&mut self.popups, &self.space, surface);
        self.layer_commit(surface);
        resize_grab::handle_commit(&mut self.space, surface);
    }
}

impl Odysseus {
    /// Le client retire le tampon de sa fenêtre (préalable habituel à la
    /// fermeture) : on garde la dernière image pour l'animation de fermeture.
    fn capture_if_unmapping(&mut self, surface: &WlSurface) {
        let removed = with_states(surface, |states| {
            matches!(
                states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .buffer,
                Some(BufferAssignment::Removed)
            )
        });
        if !removed {
            return;
        }
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
        else {
            return;
        };
        if let (Some(loc), Some(context)) = (
            self.space.element_location(&window),
            self.renderer_context.as_ref(),
        ) && let Some(closing) = crate::closing::Closing::capture(&window, loc, context)
        {
            self.closing.push(closing);
        }
    }

    /// Centre une nouvelle fenêtre dès que sa taille est connue.
    fn place_if_new(&mut self, root: &WlSurface) {
        if !self.unplaced.contains(&root.id()) {
            return;
        }
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == root)
            .cloned()
        else {
            return;
        };
        let size = window.geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let Some(out) = self.usable_area() else {
            return;
        };
        // Centrée, puis décalée en cascade (32 px par fenêtre déjà ouverte) pour rester visible
        let step = 32 * ((self.space.elements().count() as i32 - 1).max(0) % 8);
        let loc = (
            out.loc.x + (out.size.w - size.w) / 2 + step,
            out.loc.y + (out.size.h - size.h) / 2 + step,
        );
        self.unplaced.remove(&root.id());
        self.opening.insert(root.id(), std::time::Instant::now());
        let placed = match self.initial_state.remove(&root.id()) {
            Some(state) => self.apply_initial_state(&window, state),
            None => false,
        };
        if !placed {
            self.space.map_element(window.clone(), loc, true);
        }
        self.focus_window(&window);
    }
}

impl BufferHandler for Odysseus {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Odysseus {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(Odysseus);
delegate_shm!(Odysseus);
