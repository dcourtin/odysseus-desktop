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
        dmabuf::{DmabufGlobal, DmabufState},
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        selection::primary_selection::PrimarySelectionState,
        session_lock::{LockSurface, SessionLockManagerState},
        shell::{
            wlr_layer::{Layer as WlrLayer, WlrLayerShellState},
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};

use smithay::reexports::wayland_server::Resource;

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
    pub dmabuf_state: DmabufState,
    /// Global linux-dmabuf, créé par le backend une fois le GPU connu
    pub dmabuf_global: Option<DmabufGlobal>,
    pub primary_selection_state: PrimarySelectionState,
    pub activation_state: XdgActivationState,
    pub session_lock_state: SessionLockManagerState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub single_pixel_state: SinglePixelBufferState,
    pub idle_inhibit_state: IdleInhibitManagerState,

    pub seat: Seat<Self>,

    /// Fenêtres pas encore placées : centrées à leur premier rendu (taille connue)
    pub unplaced: HashSet<ObjectId>,
    /// Début de l'animation d'ouverture de chaque fenêtre récente
    pub opening: HashMap<ObjectId, Instant>,
    /// Fenêtres agrandies ou en plein écran : position et taille à restaurer
    pub restore: HashMap<ObjectId, smithay::utils::Rectangle<i32, Logical>>,
    /// Agrandie / plein écran demandé avant le premier affichage
    pub initial_state: HashMap<ObjectId, crate::windows::InitialState>,
    /// Fenêtres en plein écran
    pub fullscreen: HashSet<ObjectId>,
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
    /// Verrouillage de session et surface(s) du verrou
    pub lock_state: crate::lock::LockState,
    pub lock_surfaces: Vec<LockSurface>,
    /// Dernière entrée clavier / souris (mise en veille)
    pub last_activity: Instant,
    /// Surfaces qui empêchent la mise en veille (vidéo)
    pub idle_inhibitors: Vec<WlSurface>,
    /// Écran éteint (veille) ; `screen_power_changed` : le backend doit appliquer
    pub screen_off: bool,
    pub screen_power_changed: bool,
    /// Couches « à la demande » qui ont déjà reçu le clavier à leur apparition
    pub layers_focused_once: HashSet<ObjectId>,
    /// Réglages d'écran demandés par IPC, appliqués par le backend à l'image suivante
    pub pending_scale: Option<f64>,
    pub pending_mode: Option<(i32, i32, Option<i32>)>,
    pub night: crate::night::NightLight,
    /// Animations : trajets de fenêtres, glissement de bureau, aperçu d'aimantation
    pub motions: HashMap<ObjectId, crate::anim::Motion>,
    pub slide: Option<crate::anim::WorkspaceSlide>,
    pub snap_preview: Option<crate::anim::SnapPreview>,
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
        let dmabuf_state = DmabufState::new();
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let activation_state = XdgActivationState::new::<Self>(&dh);
        // Seul le verrou d'odysseus en a besoin, mais un client tiers (swaylock) marche aussi
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let single_pixel_state = SinglePixelBufferState::new::<Self>(&dh);
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&dh);

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
            dmabuf_state,
            dmabuf_global: None,
            primary_selection_state,
            activation_state,
            session_lock_state,
            fractional_scale_state,
            viewporter_state,
            single_pixel_state,
            idle_inhibit_state,
            seat,
            unplaced: HashSet::new(),
            opening: HashMap::new(),
            restore: HashMap::new(),
            initial_state: HashMap::new(),
            fullscreen: HashSet::new(),
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
            lock_state: Default::default(),
            lock_surfaces: Vec::new(),
            last_activity: Instant::now(),
            idle_inhibitors: Vec::new(),
            screen_off: false,
            screen_power_changed: false,
            layers_focused_once: HashSet::new(),
            pending_scale: None,
            pending_mode: None,
            night: crate::night::NightLight::load(),
            motions: HashMap::new(),
            slide: None,
            snap_preview: None,
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
        crate::lock::send_lock_frame(self, output);
        self.confirm_lock();
        self.opening
            .retain(|_, start| start.elapsed() < crate::render::open_duration());
        self.closing
            .retain(|c| c.start.elapsed() < crate::render::close_duration());
        self.motions.retain(|_, m| !m.done());
        if self.slide.as_ref().is_some_and(|s| s.done()) {
            self.slide = None;
        }
        self.space.refresh();
        self.refresh_workspaces();
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();
    }

    /// État animé de la scène pour le rendu d'une image
    pub fn scene<'a>(&'a self, focused: Option<&'a WlSurface>) -> crate::render::Scene<'a> {
        crate::render::Scene {
            focused,
            opening: &self.opening,
            closing: &self.closing,
            hide_top_layer: self.top_is_fullscreen(),
            motions: &self.motions,
            slide: self.slide.as_ref(),
            snap_preview: self.snap_preview,
        }
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
        match cmd.spawn() {
            // Attente dans un fil : pas de processus zombie
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(e) => tracing::warn!("impossible de lancer « {command} » : {e}"),
        }
    }

    /// Lance un programme d'odysseus installé à côté du compositeur.
    pub fn spawn_sibling(&self, name: &str, args: &[&str]) {
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(name)))
            .filter(|p| p.exists());
        match path {
            Some(path) => {
                let mut command = path.to_string_lossy().into_owned();
                for arg in args {
                    command.push_str(" '");
                    command.push_str(&arg.replace('\'', "'\\''"));
                    command.push('\'');
                }
                self.spawn(&command)
            }
            None => tracing::warn!("{name} introuvable à côté du compositeur"),
        }
    }

    /// Commande shell pour un programme d'odysseus voisin du compositeur.
    pub fn sibling_command(&self, command: &str) -> String {
        match std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        {
            Some(dir) => format!("'{}'/{command}", dir.display()),
            None => command.to_string(),
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
        if self.is_locked() {
            let surface = self.lock_surface()?.wl_surface().clone();
            return Some((surface, (0.0, 0.0).into()));
        }
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
        let top_hidden = self.top_is_fullscreen();
        in_layer(WlrLayer::Overlay)
            .or_else(|| {
                if top_hidden {
                    None
                } else {
                    in_layer(WlrLayer::Top)
                }
            })
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

    /// Après un changement d'écran (taille, échelle) : fenêtres agrandies
    /// recalées, les autres ramenées dans la zone visible.
    pub fn fit_windows_to_output(&mut self) {
        let Some(area) = self.usable_area() else {
            return;
        };
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let id = toplevel.wl_surface().id();
            if self.restore.contains_key(&id) {
                // Agrandie ou plein écran : nouvelle taille
                let full = self.fullscreen.contains(&id);
                let target = if full {
                    self.space
                        .outputs()
                        .next()
                        .and_then(|o| self.space.output_geometry(o))
                        .unwrap_or(area)
                } else {
                    area
                };
                toplevel.with_pending_state(|s| s.size = Some(target.size));
                toplevel.send_pending_configure();
                self.space.map_element(window, target.loc, false);
                continue;
            }
            let Some(loc) = self.space.element_location(&window) else {
                continue;
            };
            let size = window.geometry().size;
            let x = loc.x.clamp(
                area.loc.x,
                (area.loc.x + area.size.w - size.w.min(area.size.w)).max(area.loc.x),
            );
            let y = loc.y.clamp(
                area.loc.y,
                (area.loc.y + area.size.h - size.h.min(area.size.h)).max(area.loc.y),
            );
            if (x, y) != (loc.x, loc.y) {
                self.space.map_element(window, (x, y), false);
            }
        }
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
