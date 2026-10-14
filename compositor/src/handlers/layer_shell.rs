//! wlr-layer-shell : surfaces ancrées aux bords de l'écran (barre, fond,
//! notifications, lanceur). Elles réservent leur « zone exclusive », que les
//! fenêtres évitent.

use smithay::{
    delegate_layer_shell,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
            WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};

use crate::Odysseus;

impl WlrLayerShellHandler for Odysseus {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(e) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("couche refusée : {e}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        // La couche avait le clavier (lanceur…) : le rendre à la fenêtre du dessus
        let keyboard = self.seat.get_keyboard().unwrap();
        if keyboard.current_focus().as_ref() == Some(surface.wl_surface()) {
            self.focus_top_window();
        }
    }
}

delegate_layer_shell!(Odysseus);

impl Odysseus {
    /// À chaque commit d'une surface de couche : réarranger, envoyer la
    /// configuration initiale, et donner le clavier aux couches qui l'exigent.
    pub fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self
            .space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return;
        };
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
                .unwrap_or(true)
        });
        let wants_keyboard = {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) else {
                return;
            };
            if !initial_configure_sent {
                layer.layer_surface().send_configure();
            }
            initial_configure_sent
                && matches!(layer.layer(), Layer::Top | Layer::Overlay)
                && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
        };
        let keyboard = self.seat.get_keyboard().unwrap();
        if wants_keyboard && keyboard.current_focus().as_ref() != Some(surface) {
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        }
    }

    /// Couche sous le point, si elle accepte le clavier (clic sur le lanceur…).
    pub fn focusable_layer_at(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<Option<WlSurface>> {
        let output = self.space.outputs().find(|o| {
            self.space
                .output_geometry(o)
                .is_some_and(|g| g.to_f64().contains(pos))
        })?;
        let local = pos - self.space.output_geometry(output)?.loc.to_f64();
        let map = layer_map_for_output(output);
        let layer = map
            .layer_under(Layer::Overlay, local)
            .or_else(|| map.layer_under(Layer::Top, local))?;
        Some(
            layer
                .can_receive_keyboard_focus()
                .then(|| layer.wl_surface().clone()),
        )
    }

    /// Rend le clavier à la fenêtre du dessus (après la fermeture d'une couche).
    pub fn focus_top_window(&mut self) {
        let top = self
            .space
            .elements()
            .last()
            .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, top, SERIAL_COUNTER.next_serial());
    }
}
