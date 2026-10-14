use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::Arc,
    time::Instant,
};

use smithay::{
    backend::session::libseat::LibSeatSession,
    desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{Seat, SeatState, keyboard::XkbConfig, pointer::CursorImageStatus},
    output::Output,
    reexports::{
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason, ObjectId},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer as WlrLayer, WlrLayerShellState},
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::CalloopData;

pub struct Odysseus {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay State
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Odysseus>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,

    pub seat: Seat<Self>,

    /// Fenêtres pas encore placées : centrées à leur premier rendu (taille connue)
    pub unplaced: HashSet<ObjectId>,
    /// Début de l'animation d'ouverture de chaque fenêtre récente
    pub opening: HashMap<ObjectId, Instant>,
    /// Touche des raccourcis : Alt en mode imbriqué (sway garde Super), Super sur tty
    pub mod_key: ModKey,
    /// Image du curseur demandée par le client sous le pointeur
    pub cursor_status: CursorImageStatus,
    /// Session logind (mode tty) : changement de terminal virtuel
    pub session: Option<LibSeatSession>,
    /// Capture d'écran demandée (Impr. écran ou Mod+Maj+s), faite à la prochaine image
    pub capture_pending: bool,
    pub workspaces: crate::workspaces::Workspaces,
    /// IPC compatible sway : chemin du socket et abonnés aux événements
    pub ipc_path: Option<std::path::PathBuf>,
    pub ipc_subscribers: Vec<std::os::unix::net::UnixStream>,
    /// Fenêtres en cours d'animation de fermeture
    pub closing: Vec<crate::closing::Closing>,
    /// Contexte GPU du moteur de rendu (pour retrouver les textures des surfaces)
    pub renderer_context: Option<
        smithay::backend::renderer::ContextId<smithay::backend::renderer::gles::GlesTexture>,
    >,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModKey {
    Alt,
    Super,
}

impl Odysseus {
    pub fn new(event_loop: &mut EventLoop<CalloopData>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();

        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let popups = PopupManager::default();
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);

        // A seat is a group of keyboards, pointer and touch devices.
        // A seat typically has a pointer and maintains a keyboard focus and a pointer focus.
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");

        // Notify clients that we have a keyboard, for the sake of the example we assume that keyboard is always present.
        // You may want to track keyboard hot-plug in real compositor.
        // Disposition clavier : XKB_DEFAULT_LAYOUT (profil système), fr par défaut
        let layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "fr".into());
        let xkb = XkbConfig {
            layout: &layout,
            ..Default::default()
        };
        seat.add_keyboard(xkb, 200, 25).unwrap();

        // Notify clients that we have a pointer (mouse)
        // Here we assume that there is always pointer plugged in
        seat.add_pointer();

        // A space represents a two-dimensional plane. Windows and Outputs can be mapped onto it.
        //
        // Windows get a position and stacking order through mapping.
        // Outputs become views of a part of the Space and can be rendered via Space::render_output.
        let space = Space::default();

        let socket_name = Self::init_wayland_listener(display, event_loop);

        // Get the loop signal, used to stop the event loop
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            socket_name,

            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups,
            xdg_decoration_state,
            layer_shell_state,
            seat,
            unplaced: HashSet::new(),
            opening: HashMap::new(),
            // Le backend winit tourne toujours dans un autre compositeur
            mod_key: ModKey::Alt,
            cursor_status: CursorImageStatus::default_named(),
            session: None,
            capture_pending: false,
            workspaces: Default::default(),
            ipc_path: None,
            ipc_subscribers: Vec::new(),
            closing: Vec::new(),
            renderer_context: None,
        }
    }

    /// Après chaque image : rappels de rendu aux clients, nettoyage, envoi des messages.
    pub fn after_frame(&mut self, output: &Output) {
        let time = self.start_time.elapsed();
        let zero = Some(std::time::Duration::ZERO);
        self.space.elements().for_each(|window| {
            window.send_frame(output, time, zero, |_, _| Some(output.clone()));
        });
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, zero, |_, _| Some(output.clone()));
        }
        self.opening
            .retain(|_, start| start.elapsed() < crate::render::open_duration());
        self.closing
            .retain(|c| c.start.elapsed() < crate::render::close_duration());
        self.space.refresh();
        self.refresh_workspaces();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }

    /// Lance une commande shell comme client de ce compositeur.
    pub fn spawn(&self, command: &str) {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env_remove("DISPLAY");
        match &self.ipc_path {
            Some(path) => cmd.env("SWAYSOCK", path),
            None => cmd.env_remove("SWAYSOCK"),
        };
        let result = cmd.spawn();
        if let Err(e) = result {
            tracing::warn!("impossible de lancer « {command} » : {e}");
        }
    }

    fn init_wayland_listener(
        display: Display<Odysseus>,
        event_loop: &mut EventLoop<CalloopData>,
    ) -> OsString {
        // Creates a new listening socket, automatically choosing the next available `wayland` socket name.
        let listening_socket = ListeningSocketSource::new_auto().unwrap();

        // Get the name of the listening socket.
        // Clients will connect to this socket.
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display
                            .get_mut()
                            .dispatch_clients(&mut state.state)
                            .unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    /// Surface sous le pointeur : couches du haut, puis fenêtres, puis couches du bas.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.outputs().find(|o| {
            self.space
                .output_geometry(o)
                .is_some_and(|g| g.to_f64().contains(pos))
        })?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let layers = layer_map_for_output(output);
        let in_layer = |layer_kind: WlrLayer| {
            let local = pos - output_loc.to_f64();
            layers.layer_under(layer_kind, local).and_then(|layer| {
                let layer_loc = layers.layer_geometry(layer)?.loc;
                layer
                    .surface_under(local - layer_loc.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + layer_loc + output_loc).to_f64()))
            })
        };
        in_layer(WlrLayer::Overlay)
            .or_else(|| in_layer(WlrLayer::Top))
            .or_else(|| {
                self.space
                    .element_under(pos)
                    .and_then(|(window, location)| {
                        window
                            .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                            .map(|(s, p)| (s, (p + location).to_f64()))
                    })
            })
            .or_else(|| in_layer(WlrLayer::Bottom))
            .or_else(|| in_layer(WlrLayer::Background))
    }

    /// Zone utilisable par les fenêtres sur la première sortie (hors zones exclusives des couches).
    pub fn usable_area(&self) -> Option<smithay::utils::Rectangle<i32, Logical>> {
        let output = self.space.outputs().next()?;
        let out = self.space.output_geometry(output)?;
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        zone.loc += out.loc;
        Some(zone)
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
