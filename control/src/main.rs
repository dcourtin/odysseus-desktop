//! Centre de contrôle, ouvert depuis la barre (en haut à droite), à la manière
//! de macOS : Wi-Fi, Bluetooth, réseau filaire, « Ne pas déranger », son,
//! verrouiller, redémarrer, éteindre. Wi-Fi, Bluetooth et Son ont une vue de
//! détail (réseaux, appareils, sorties audio).
//!
//! Lancé à nouveau alors qu'il est ouvert, il se ferme (bascule). Il se ferme
//! aussi avec Échap ou dès qu'on clique ailleurs (perte du clavier).

mod sys;
mod worker;

use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            channel::{self, Event as ChannelEvent},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

use worker::{Act, Event, Snapshot};

/// Largeur du panneau et marge transparente (ombre), en pixels logiques
const W: f32 = 340.0;
const MARGIN: f32 = 16.0;
const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const ROW: f32 = 48.0;
const LIST_ROW: f32 = 40.0;
const MAX_ROWS: usize = 8;
const BTN_LEFT: u32 = 0x110;
/// Second clic attendu pour confirmer Éteindre / Redémarrer
const CONFIRM_TIME: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Main,
    Wifi,
    Bluetooth,
    Sound,
    Theme,
}

/// Élément cliquable
#[derive(Clone, PartialEq, Debug)]
enum Target {
    WifiToggle,
    WifiOpen,
    BtToggle,
    BtOpen,
    Dnd,
    Night,
    Slider,
    Mute,
    SoundOpen,
    Sleep,
    Lock,
    Reboot,
    PowerOff,
    Back,
    Network(usize),
    Device(usize),
    Sink(usize),
    Password,
    ThemeOpen,
    Theme(usize),
}

struct Zone {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    target: Target,
}

struct Control {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<Control>,
    /// Image en attente d'affichage, et dessin demandé entre-temps
    frame_pending: bool,
    dirty: bool,
    /// Fond du panneau (ombre, cadre) déjà dessiné, pour cette taille de tampon :
    /// le plus coûteux d'une image, et il ne change pas
    background: Option<(u32, u32, Vec<u8>)>,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Control>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    icons: Text,
    acts: mpsc::Sender<Act>,
    state: Snapshot,
    /// Le premier état n'est pas encore arrivé
    loading: bool,
    view: View,
    zones: Vec<Zone>,
    hover: Option<Target>,
    /// Glisser du curseur de volume en cours, et volume affiché pendant ce temps
    dragging: bool,
    volume: Option<f32>,
    /// Réseau Wi-Fi en attente du mot de passe, et sa saisie
    password_for: Option<String>,
    password: String,
    /// Opération longue en cours (connexion, appairage) et message à afficher
    busy: Option<String>,
    message: Option<String>,
    /// Éteindre / Redémarrer armé : un second clic confirme
    confirm: Option<(Target, Instant)>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    /// Mise en veille demandée : faite après la fermeture du panneau
    suspend: bool,
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-control.pid"))
}

/// Si le centre est déjà ouvert : le fermer, et renvoyer vrai (bascule).
fn toggle_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    // Un processus zombie (terminé, pas encore récupéré) ne compte pas
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|st| {
            st.rsplit(')')
                .next()
                .map(|r| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(true);
    if zombie || !comm.trim().starts_with("odysseus-contro") || pid == std::process::id() {
        return false;
    }
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    true
}

fn main() {
    if toggle_existing() {
        return;
    }
    if let Some(path) = pid_file() {
        let _ = std::fs::write(path, std::process::id().to_string());
    }

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<Control> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("odysseus-control"), None);
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    let width = (W + 2.0 * MARGIN) as u32;
    let height = main_height() as u32;
    layer.set_size(width, height);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.commit();

    let mut event_loop: EventLoop<Control> = EventLoop::try_new().expect("boucle d'événements");
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .expect("source Wayland");

    // Fil de travail : état du système et actions
    let (acts, acts_rx) = mpsc::channel();
    let (events_tx, events_rx) = channel::channel();
    std::thread::spawn(move || worker::run(acts_rx, events_tx));
    handle
        .insert_source(events_rx, |event, _, control| {
            if let ChannelEvent::Msg(event) = event {
                control.on_event(event);
            }
        })
        .expect("canal du fil de travail");
    let _ = acts.send(Act::Refresh);

    let mut control = Control {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024, &shm).expect("pool shm"),
        qh: qh.clone(),
        frame_pending: false,
        dirty: false,
        background: None,
        shm,
        layer,
        loop_handle: handle,
        pointer: None,
        keyboard: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        acts,
        state: Snapshot::default(),
        loading: true,
        view: View::Main,
        zones: Vec::new(),
        hover: None,
        dragging: false,
        volume: None,
        password_for: None,
        password: String::new(),
        busy: None,
        message: None,
        confirm: None,
        width,
        height,
        scale: 1,
        configured: false,
        exit: false,
        suspend: false,
    };

    // `odysseus-control --vue wifi|bluetooth|son` : ouvre directement une vue de détail
    let args: Vec<String> = std::env::args().collect();
    if let Some(view) = args
        .iter()
        .position(|a| a == "--vue")
        .and_then(|i| args.get(i + 1))
    {
        let view = match view.as_str() {
            "wifi" => Some(View::Wifi),
            "bluetooth" => Some(View::Bluetooth),
            "son" => Some(View::Sound),
            _ => None,
        };
        if let Some(view) = view {
            control.open_view(view);
        }
    }

    while !control.exit {
        if let Err(e) = event_loop.dispatch(None, &mut control) {
            eprintln!("odysseus-control : connexion au compositeur perdue ({e})");
            break;
        }
    }
    control.leave_view();
    if let Some(path) = pid_file() {
        let _ = std::fs::remove_file(path);
    }
    // Veille : le temps que le verrou s'affiche, puis logind
    if control.suspend {
        drop(control);
        std::thread::sleep(Duration::from_millis(1200));
        match dbus::blocking::Connection::new_system() {
            Ok(conn) => {
                if let Err(e) = sys::power(&conn, "Suspend") {
                    eprintln!("odysseus-control : veille refusée ({e})");
                }
            }
            Err(e) => eprintln!("odysseus-control : D-Bus système ({e})"),
        }
    }
}

fn main_height() -> f32 {
    2.0 * MARGIN + PAD + 3.0 * ROW + 2.0 * (GAP + NIGHT_H) + GAP + 76.0 + GAP + 80.0 + PAD
}

/// Hauteur des tuiles « Lumière de nuit » et « Apparence »
const NIGHT_H: f32 = 56.0;

/// Icône Wi-Fi selon la qualité du signal
fn wifi_glyph(quality: u8) -> char {
    match quality {
        0..=25 => icon::WIFI_ZERO,
        26..=50 => icon::WIFI_LOW,
        51..=75 => icon::WIFI_HIGH,
        _ => icon::WIFI,
    }
}

fn volume_glyph(v: f32, muted: bool) -> char {
    if muted || v <= 0.0 {
        icon::VOLUME_MUTE
    } else if v < 0.34 {
        icon::VOLUME_0
    } else if v < 0.67 {
        icon::VOLUME_1
    } else {
        icon::VOLUME_2
    }
}

fn bt_glyph(icon_name: &str) -> char {
    match icon_name {
        n if n.contains("audio") || n.contains("headset") || n.contains("headphone") => {
            icon::HEADPHONES
        }
        n if n.contains("keyboard") => icon::KEYBOARD,
        n if n.contains("mouse") => icon::MOUSE,
        n if n.contains("speaker") => icon::SPEAKER,
        _ => icon::BLUETOOTH,
    }
}

impl Control {
    fn send(&self, act: Act) {
        let _ = self.acts.send(act);
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::State(snapshot) => {
                self.state = *snapshot;
                self.loading = false;
                if !self.dragging {
                    self.volume = None;
                }
            }
            Event::Done(error) => {
                self.busy = None;
                self.message = error;
                if self.message.is_none() {
                    self.password_for = None;
                    self.password.clear();
                }
                self.send(Act::Refresh);
            }
        }
        self.resize_or_draw();
    }

    /// Hauteur voulue pour la vue courante
    fn wanted_height(&self) -> f32 {
        let header = 52.0;
        let message = if self.message.is_some() || self.busy.is_some() {
            28.0
        } else {
            0.0
        };
        let rows = |n: usize| n.clamp(1, MAX_ROWS) as f32 * LIST_ROW;
        let body = match self.view {
            View::Main => return main_height(),
            View::Wifi => {
                let extra = if self.password_for.is_some() {
                    44.0
                } else {
                    0.0
                };
                rows(self.state.wifi.networks.len()) + extra
            }
            View::Bluetooth => rows(self.state.bt.devices.len()),
            View::Sound => 56.0 + rows(self.state.sound.sinks.len()),
            View::Theme => rows(theme::THEMES.len()),
        };
        2.0 * MARGIN + PAD + header + body + message + PAD
    }

    fn resize_or_draw(&mut self) {
        let h = self.wanted_height() as u32;
        if h != self.height {
            self.height = h;
            self.layer.set_size(self.width, h);
            self.layer.commit();
            // Dessin à la configuration
        } else {
            self.draw();
        }
    }

    fn open_view(&mut self, view: View) {
        self.leave_view();
        self.view = view;
        self.message = None;
        match view {
            View::Wifi => self.send(Act::WifiScan),
            View::Bluetooth => self.send(Act::BtDiscovery(true)),
            _ => self.send(Act::Refresh),
        }
        self.resize_or_draw();
    }

    fn leave_view(&mut self) {
        if self.view == View::Bluetooth {
            self.send(Act::BtDiscovery(false));
        }
        self.password_for = None;
        self.password.clear();
    }

    fn back(&mut self) {
        self.leave_view();
        self.view = View::Main;
        self.message = None;
        self.resize_or_draw();
    }

    fn zone_at(&self, x: f32, y: f32) -> Option<Target> {
        self.zones
            .iter()
            .rev()
            .find(|z| x >= z.x && x < z.x + z.w && y >= z.y && y < z.y + z.h)
            .map(|z| z.target.clone())
    }

    fn slider_value(&self, x: f32) -> Option<f32> {
        let z = self.zones.iter().find(|z| z.target == Target::Slider)?;
        Some(((x - z.x) / z.w).clamp(0.0, 1.0))
    }

    fn click(&mut self, x: f32, y: f32) {
        let Some(target) = self.zone_at(x, y) else {
            return;
        };
        let armed = self
            .confirm
            .as_ref()
            .is_some_and(|(t, at)| *t == target && at.elapsed() < CONFIRM_TIME);
        if !matches!(target, Target::Reboot | Target::PowerOff) {
            self.confirm = None;
        }
        match target {
            Target::WifiToggle => self.send(Act::WifiPower(!self.state.wifi.powered)),
            Target::WifiOpen => self.open_view(View::Wifi),
            Target::BtToggle => self.send(Act::BtPower(!self.state.bt.powered)),
            Target::BtOpen => self.open_view(View::Bluetooth),
            Target::Night => {
                // Désactivée → automatique → activée → désactivée
                let next = match self.state.night.as_str() {
                    "off" => "auto",
                    "auto" => "on",
                    _ => "off",
                };
                self.state.night = next.to_string();
                self.send(Act::Night(next));
            }
            Target::Dnd => {
                self.state.dnd = !self.state.dnd;
                self.send(Act::Dnd(self.state.dnd));
            }
            Target::Slider => {
                self.dragging = true;
                if let Some(v) = self.slider_value(x) {
                    self.volume = Some(v);
                    self.send(Act::Volume(v));
                }
            }
            Target::Mute => self.send(Act::Mute),
            Target::SoundOpen => self.open_view(View::Sound),
            Target::ThemeOpen => self.open_view(View::Theme),
            Target::Theme(i) => {
                if let Err(e) = theme::set(theme::THEMES[i].id) {
                    self.message = Some(format!("Thème : {e}"));
                }
                // Le fond du panneau change de couleur : à redessiner
                self.background = None;
                self.resize_or_draw();
            }
            Target::Back => self.back(),
            // Veille : comme sur macOS, la session est verrouillée avant
            Target::Sleep => {
                spawn_sibling("odysseus-lock");
                self.suspend = true;
                self.exit = true;
            }
            Target::Lock => {
                spawn_sibling("odysseus-lock");
                self.exit = true;
            }
            Target::Reboot | Target::PowerOff => {
                if armed {
                    self.send(Act::Power(if target == Target::Reboot {
                        "Reboot"
                    } else {
                        "PowerOff"
                    }));
                    self.message = Some(if target == Target::Reboot {
                        "Redémarrage…".into()
                    } else {
                        "Extinction…".into()
                    });
                } else {
                    self.confirm = Some((target, Instant::now()));
                    // Le bouton reprend son aspect normal s'il n'est pas confirmé
                    let _ = self.loop_handle.insert_source(
                        smithay_client_toolkit::reexports::calloop::timer::Timer::from_duration(
                            CONFIRM_TIME,
                        ),
                        |_, _, control| {
                            control.draw();
                            smithay_client_toolkit::reexports::calloop::timer::TimeoutAction::Drop
                        },
                    );
                }
            }
            Target::Network(i) => self.click_network(i),
            Target::Device(i) => {
                if let Some(d) = self.state.bt.devices.get(i).cloned() {
                    if d.connected {
                        self.busy = Some(format!("Déconnexion de {}…", d.name));
                        self.send(Act::BtDisconnect(d.path));
                    } else {
                        self.busy = Some(format!("Connexion à {}…", d.name));
                        self.send(Act::BtConnect(d.path, d.paired));
                    }
                    self.message = None;
                }
            }
            Target::Sink(i) => {
                if let Some(s) = self.state.sound.sinks.get(i) {
                    self.send(Act::Sink(s.id));
                }
            }
            Target::Password => {}
        }
        self.resize_or_draw();
    }

    fn click_network(&mut self, i: usize) {
        let Some(n) = self.state.wifi.networks.get(i).cloned() else {
            return;
        };
        self.message = None;
        if n.connected {
            self.send(Act::WifiDisconnect);
        } else if n.known || n.kind == "open" {
            self.busy = Some(format!("Connexion à {}…", n.name));
            self.send(Act::WifiConnect(n.path, None));
        } else if n.kind == "psk" {
            self.password_for = Some(n.path);
            self.password.clear();
        } else {
            self.message = Some("Réseau d'entreprise (802.1X) : non pris en charge ici".into());
        }
    }

    fn key(&mut self, event: KeyEvent) {
        if let Some(path) = self.password_for.clone() {
            match event.keysym {
                Keysym::Escape => {
                    self.password_for = None;
                    self.password.clear();
                }
                Keysym::Return | Keysym::KP_Enter => {
                    if self.password.len() >= 8 {
                        let name = self
                            .state
                            .wifi
                            .networks
                            .iter()
                            .find(|n| n.path == path)
                            .map(|n| n.name.clone())
                            .unwrap_or_default();
                        self.busy = Some(format!("Connexion à {name}…"));
                        self.message = None;
                        self.send(Act::WifiConnect(path, Some(self.password.clone())));
                    } else {
                        self.message = Some("Au moins 8 caractères".into());
                    }
                }
                Keysym::BackSpace => {
                    self.password.pop();
                }
                _ => {
                    if !(self.modifiers.ctrl || self.modifiers.logo)
                        && let Some(text) = event
                            .utf8
                            .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                    {
                        self.password.push_str(&text);
                    }
                }
            }
            self.resize_or_draw();
            return;
        }
        if event.keysym == Keysym::Escape {
            if self.view == View::Main {
                self.exit = true;
            } else {
                self.back();
            }
        }
    }

    // --- Dessin --------------------------------------------------------------

    /// Demande un dessin : immédiat, puis au plus un par rafraîchissement
    /// d'écran (une souris rapide envoie bien plus d'événements que d'images).
    fn draw(&mut self) {
        if self.frame_pending {
            self.dirty = true;
            return;
        }
        self.render();
    }

    fn render(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        self.dirty = false;
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
        let Ok((buffer, data)) =
            self.pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut canvas = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        self.zones.clear();
        match &self.background {
            Some((bw, bh, pixels)) if (*bw, *bh) == (w, h) => canvas.buf.copy_from_slice(pixels),
            _ => {
                canvas.clear();
                // Panneau
                let (px, py, pw, ph) = (MARGIN, MARGIN, W, self.height as f32 - 2.0 * MARGIN);
                let r = theme::PANEL_RADIUS * s;
                canvas.shadow(px * s, (py + 4.0) * s, pw * s, ph * s, r, 22.0 * s, 0.45);
                canvas.fill_rounded(px * s, py * s, pw * s, ph * s, r, theme::palette().panel);
                canvas.stroke_rounded(
                    px * s,
                    py * s,
                    pw * s,
                    ph * s,
                    r,
                    s.max(1.0),
                    theme::palette().panel_border(),
                );
                self.background = Some((w, h, canvas.buf.to_vec()));
            }
        }

        let mut d = Drawer {
            canvas: &mut canvas,
            s,
            regular: &self.regular,
            bold: &self.bold,
            icons: &self.icons,
            zones: &mut self.zones,
            hover: self.hover.as_ref(),
        };
        let x = MARGIN + PAD;
        let y = MARGIN + PAD;
        let inner = W - 2.0 * PAD;
        match self.view {
            View::Main => draw_main(
                &mut d,
                &self.state,
                self.volume,
                self.confirm.as_ref(),
                x,
                y,
                inner,
            ),
            View::Wifi => draw_wifi(
                &mut d,
                &self.state,
                self.password_for.as_deref(),
                &self.password,
                x,
                y,
                inner,
            ),
            View::Bluetooth => draw_bt(&mut d, &self.state, x, y, inner),
            View::Sound => draw_sound(&mut d, &self.state, self.volume, x, y, inner),
            View::Theme => draw_theme(&mut d, x, y, inner),
        }
        // Ligne d'état en bas : opération en cours ou erreur
        let status = self.busy.clone().or_else(|| self.message.clone());
        if let Some(text) = status {
            let sy = self.height as f32 - MARGIN - PAD - 24.0;
            let color = if self.busy.is_some() {
                theme::palette().text_dim
            } else {
                theme::palette().danger
            };
            let t = d.regular.truncate(&text, theme::FONT_SMALL * s, inner * s);
            d.regular.draw_in(
                d.canvas,
                &t,
                x * s,
                sy * s,
                24.0 * s,
                theme::FONT_SMALL * s,
                color,
            );
        }
        if self.loading {
            d.regular.draw_centered(
                d.canvas,
                "…",
                (MARGIN + W / 2.0) * s,
                (self.height as f32 - MARGIN - 20.0) * s,
                16.0 * s,
                theme::FONT_SMALL * s,
                theme::palette().text_dim,
            );
        }

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(&self.qh, surface.clone());
        self.frame_pending = true;
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

/// Aide au dessin : canevas, polices, zones cliquables, survol
struct Drawer<'a, 'b> {
    canvas: &'a mut Canvas<'b>,
    s: f32,
    regular: &'a Text,
    bold: &'a Text,
    icons: &'a Text,
    zones: &'a mut Vec<Zone>,
    hover: Option<&'a Target>,
}

impl Drawer<'_, '_> {
    fn zone(&mut self, x: f32, y: f32, w: f32, h: f32, target: Target) {
        self.zones.push(Zone { x, y, w, h, target });
    }

    fn hovered(&self, t: &Target) -> bool {
        self.hover == Some(t)
    }

    fn tile(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let s = self.s;
        self.canvas.fill_rounded(
            x * s,
            y * s,
            w * s,
            h * s,
            theme::TILE_RADIUS * s,
            theme::palette().tile(),
        );
    }

    /// Pastille ronde avec icône : bleue si active
    fn circle(&mut self, cx: f32, cy: f32, r: f32, glyph: char, on: bool, hovered: bool) {
        let s = self.s;
        let bg = match (on, hovered) {
            (true, false) => theme::palette().accent,
            (true, true) => theme::palette().accent_hover(),
            (false, false) => theme::palette().ink(0.12),
            (false, true) => theme::palette().ink(0.2),
        };
        self.canvas.fill_circle(cx * s, cy * s, r * s, bg);
        let fg = if on {
            theme::palette().on_accent
        } else {
            theme::palette().text
        };
        self.icons
            .icon(self.canvas, glyph, cx * s, cy * s, r * 1.05 * s, fg);
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        s_: &str,
        x: f32,
        y: f32,
        h: f32,
        px: f32,
        bold: bool,
        color: Rgba,
        max_w: f32,
    ) {
        let s = self.s;
        let font = if bold { self.bold } else { self.regular };
        let t = font.truncate(s_, px * s, max_w * s);
        font.draw_in(self.canvas, &t, x * s, y * s, h * s, px * s, color);
    }

    fn hover_bg(&mut self, x: f32, y: f32, w: f32, h: f32, t: &Target) {
        if self.hovered(t) {
            let s = self.s;
            self.canvas.fill_rounded(
                x * s,
                y * s,
                w * s,
                h * s,
                8.0 * s,
                theme::palette().ink(0.06),
            );
        }
    }

    /// Interrupteur façon iOS
    fn switch(&mut self, x: f32, y: f32, on: bool) {
        let s = self.s;
        let (w, h) = (38.0, 22.0);
        let bg = if on {
            theme::palette().accent
        } else {
            theme::palette().ink(0.2)
        };
        self.canvas
            .fill_rounded(x * s, y * s, w * s, h * s, h / 2.0 * s, bg);
        let kx = if on { x + w - h / 2.0 } else { x + h / 2.0 };
        self.canvas.fill_circle(
            kx * s,
            (y + h / 2.0) * s,
            (h / 2.0 - 2.0) * s,
            Rgba::hex(0xffffff, 1.0),
        );
    }

    /// Curseur épais avec l'icône du volume à gauche, comme sur macOS
    fn slider(&mut self, x: f32, y: f32, w: f32, value: f32, muted: bool) {
        let s = self.s;
        let h = 26.0;
        self.canvas.fill_rounded(
            x * s,
            y * s,
            w * s,
            h * s,
            h / 2.0 * s,
            theme::palette().ink(0.14),
        );
        let filled = (w * value).max(h);
        let fill = if muted {
            theme::palette().ink(0.35)
        } else {
            theme::palette().ink(0.92)
        };
        self.canvas
            .fill_rounded(x * s, y * s, filled * s, h * s, h / 2.0 * s, fill);
        self.icons.icon(
            self.canvas,
            volume_glyph(value, muted),
            (x + h / 2.0 + 1.0) * s,
            (y + h / 2.0) * s,
            14.0 * s,
            theme::palette().content,
        );
        // Tout le curseur répond (le remplissage part du bord gauche) ;
        // l'icône du haut-parleur, ajoutée après, l'emporte : sourdine
        self.zone(x, y - 4.0, w, h + 8.0, Target::Slider);
        self.zone(x, y, h, h, Target::Mute);
    }

    /// En-tête des vues de détail : retour, titre, interrupteur éventuel
    fn header(&mut self, x: f32, y: f32, w: f32, title: &str, toggle: Option<(bool, Target)>) {
        let back = Target::Back;
        let hovered = self.hovered(&back);
        self.circle(x + 14.0, y + 18.0, 14.0, icon::CHEVRON_LEFT, false, hovered);
        self.zone(x, y, 32.0, 36.0, back);
        self.text(
            title,
            x + 38.0,
            y,
            36.0,
            theme::FONT_TITLE,
            true,
            theme::palette().text,
            w - 100.0,
        );
        if let Some((on, target)) = toggle {
            self.switch(x + w - 40.0, y + 7.0, on);
            self.zone(x + w - 48.0, y, 48.0, 36.0, target);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_main(
    d: &mut Drawer,
    st: &Snapshot,
    volume: Option<f32>,
    confirm: Option<&(Target, Instant)>,
    x: f32,
    y: f32,
    w: f32,
) {
    // Connectivité (2/3) et « Ne pas déranger » (1/3)
    let right_w = 104.0;
    let left_w = w - right_w - GAP;
    let tile_h = 3.0 * ROW;
    d.tile(x, y, left_w, tile_h);

    // Wi-Fi
    let wifi = &st.wifi;
    let (wifi_sub, wifi_glyph_c) = if !wifi.present {
        ("Aucune carte".to_string(), icon::WIFI_OFF)
    } else if !wifi.powered {
        ("Désactivé".to_string(), icon::WIFI_OFF)
    } else if let Some(name) = wifi.connected.as_ref() {
        let q = wifi
            .networks
            .iter()
            .find(|n| n.connected)
            .map(|n| n.quality)
            .unwrap_or(100);
        (name.clone(), wifi_glyph(q))
    } else {
        ("Non connecté".to_string(), icon::WIFI)
    };
    connectivity_row(
        d,
        x,
        y,
        left_w,
        ("Wi-Fi", &wifi_sub),
        wifi_glyph_c,
        wifi.powered,
        Some((Target::WifiToggle, Target::WifiOpen)),
    );

    // Bluetooth
    let bt = &st.bt;
    let connected = bt.devices.iter().filter(|d| d.connected).count();
    let bt_sub = if !bt.present {
        "Aucun adaptateur".to_string()
    } else if !bt.powered {
        "Désactivé".to_string()
    } else if connected > 0 {
        let first = bt
            .devices
            .iter()
            .find(|d| d.connected)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        if connected > 1 {
            format!("{first} et {} autre(s)", connected - 1)
        } else {
            first
        }
    } else {
        "Activé".to_string()
    };
    let bt_glyph_c = if !bt.powered {
        icon::BLUETOOTH_OFF
    } else if connected > 0 {
        icon::BLUETOOTH_CONNECTED
    } else {
        icon::BLUETOOTH
    };
    connectivity_row(
        d,
        x,
        y + ROW,
        left_w,
        ("Bluetooth", &bt_sub),
        bt_glyph_c,
        bt.powered,
        Some((Target::BtToggle, Target::BtOpen)),
    );

    // Réseau filaire (information seule)
    let wired = &st.wired;
    let wired_sub = match (wired.interface.as_ref(), wired.up, wired.address.as_ref()) {
        (None, ..) => "Aucune interface".to_string(),
        (Some(_), false, _) => "Câble débranché".to_string(),
        (Some(_), true, Some(ip)) => ip.clone(),
        (Some(_), true, None) => "Connexion…".to_string(),
    };
    connectivity_row(
        d,
        x,
        y + 2.0 * ROW,
        left_w,
        ("Ethernet", &wired_sub),
        icon::ETHERNET,
        wired.up && wired.address.is_some(),
        None,
    );

    // Ne pas déranger
    let rx = x + left_w + GAP;
    let dnd_hover = d.hovered(&Target::Dnd);
    d.tile(rx, y, right_w, tile_h);
    if dnd_hover {
        let s = d.s;
        d.canvas.fill_rounded(
            rx * s,
            y * s,
            right_w * s,
            tile_h * s,
            theme::TILE_RADIUS * s,
            Rgba::hex(0xffffff, 0.05),
        );
    }
    d.circle(
        rx + right_w / 2.0,
        y + 42.0,
        20.0,
        icon::MOON,
        st.dnd,
        dnd_hover,
    );
    let s = d.s;
    d.bold.draw_centered(
        d.canvas,
        "Ne pas",
        (rx + right_w / 2.0) * s,
        (y + 70.0) * s,
        18.0 * s,
        13.0 * s,
        theme::palette().text,
    );
    d.bold.draw_centered(
        d.canvas,
        "déranger",
        (rx + right_w / 2.0) * s,
        (y + 86.0) * s,
        18.0 * s,
        13.0 * s,
        theme::palette().text,
    );
    let state = if st.dnd { "Activé" } else { "Désactivé" };
    d.regular.draw_centered(
        d.canvas,
        state,
        (rx + right_w / 2.0) * s,
        (y + 106.0) * s,
        18.0 * s,
        theme::FONT_SMALL * s,
        theme::palette().text_dim,
    );
    d.zone(rx, y, right_w, tile_h, Target::Dnd);

    // Lumière de nuit
    let ny = y + tile_h + GAP;
    let night_on = st.night != "off";
    let target = Target::Night;
    let hovered = d.hovered(&target);
    d.tile(x, ny, w, NIGHT_H);
    d.hover_bg(x + 4.0, ny + 4.0, w - 8.0, NIGHT_H - 8.0, &target);
    d.circle(
        x + 26.0,
        ny + NIGHT_H / 2.0,
        16.0,
        icon::SUN_MOON,
        night_on,
        hovered,
    );
    d.text(
        "Lumière de nuit",
        x + 50.0,
        ny + 9.0,
        20.0,
        13.0,
        true,
        theme::palette().text,
        w - 76.0,
    );
    let sub = match st.night.as_str() {
        "on" => "Activée",
        "auto" => "Automatique, de 21 h à 7 h",
        _ => "Désactivée",
    };
    d.text(
        sub,
        x + 50.0,
        ny + 27.0,
        18.0,
        theme::FONT_SMALL,
        false,
        theme::palette().text_dim,
        w - 76.0,
    );
    d.zone(x, ny, w, NIGHT_H, target);

    // Apparence
    let ay = ny + NIGHT_H + GAP;
    let target = Target::ThemeOpen;
    let hovered = d.hovered(&target);
    d.tile(x, ay, w, NIGHT_H);
    d.hover_bg(x + 4.0, ay + 4.0, w - 8.0, NIGHT_H - 8.0, &target);
    d.circle(
        x + 26.0,
        ay + NIGHT_H / 2.0,
        16.0,
        icon::PALETTE,
        false,
        hovered,
    );
    d.text(
        "Apparence",
        x + 50.0,
        ay + 9.0,
        20.0,
        13.0,
        true,
        theme::palette().text,
        w - 76.0,
    );
    d.text(
        theme::palette().name,
        x + 50.0,
        ay + 27.0,
        18.0,
        theme::FONT_SMALL,
        false,
        theme::palette().text_dim,
        w - 76.0,
    );
    let s = d.s;
    d.icons.icon(
        d.canvas,
        icon::CHEVRON_RIGHT,
        (x + w - 18.0) * s,
        (ay + NIGHT_H / 2.0) * s,
        14.0 * s,
        theme::palette().text_dim,
    );
    d.zone(x, ay, w, NIGHT_H, target);

    // Son
    let sy = ay + NIGHT_H + GAP;
    let sound_h = 76.0;
    d.tile(x, sy, w, sound_h);
    d.text(
        "Son",
        x + PAD,
        sy + 8.0,
        24.0,
        13.0,
        true,
        theme::palette().text,
        120.0,
    );
    let sink = st
        .sound
        .sinks
        .iter()
        .find(|s| s.default)
        .map(|s| s.name.clone());
    let open = Target::SoundOpen;
    d.hover_bg(x + w - 150.0, sy + 6.0, 144.0, 28.0, &open);
    if let Some(name) = sink {
        d.text(
            &name,
            x + w - 146.0,
            sy + 8.0,
            24.0,
            theme::FONT_SMALL,
            false,
            theme::palette().text_dim,
            118.0,
        );
    }
    d.icons.icon(
        d.canvas,
        icon::CHEVRON_RIGHT,
        (x + w - 18.0) * s,
        (sy + 20.0) * s,
        14.0 * s,
        theme::palette().text_dim,
    );
    d.zone(x + w - 150.0, sy + 6.0, 144.0, 28.0, open);
    if st.sound.available {
        let v = volume.unwrap_or(st.sound.volume);
        d.slider(
            x + PAD,
            sy + 40.0,
            w - 2.0 * PAD,
            v,
            st.sound.muted && volume.is_none(),
        );
    } else {
        d.text(
            "PipeWire indisponible",
            x + PAD,
            sy + 40.0,
            26.0,
            theme::FONT_SMALL,
            false,
            theme::palette().text_dim,
            w,
        );
    }

    // Veille, Verrouiller, Redémarrer, Éteindre
    let by = sy + sound_h + GAP;
    let bw = (w - 3.0 * GAP) / 4.0;
    let bh = 80.0;
    let buttons = [
        (Target::Sleep, icon::SLEEP, "Veille"),
        (Target::Lock, icon::LOCK, "Verrouiller"),
        (Target::Reboot, icon::RESTART, "Redémarrer"),
        (Target::PowerOff, icon::POWER, "Éteindre"),
    ];
    for (i, (target, glyph, label)) in buttons.into_iter().enumerate() {
        let bx = x + i as f32 * (bw + GAP);
        let armed = confirm.is_some_and(|(t, at)| *t == target && at.elapsed() < CONFIRM_TIME);
        let hovered = d.hovered(&target);
        d.tile(bx, by, bw, bh);
        if armed {
            let s = d.s;
            d.canvas.fill_rounded(
                bx * s,
                by * s,
                bw * s,
                bh * s,
                theme::TILE_RADIUS * s,
                theme::palette().danger.with_alpha(0.25),
            );
        }
        let cx = bx + bw / 2.0;
        if armed {
            let s = d.s;
            d.canvas
                .fill_circle(cx * s, (by + 30.0) * s, 18.0 * s, theme::palette().danger);
            d.icons.icon(
                d.canvas,
                glyph,
                cx * s,
                (by + 30.0) * s,
                18.0 * s,
                theme::palette().on_accent,
            );
        } else {
            d.circle(cx, by + 30.0, 18.0, glyph, false, hovered);
        }
        let text = if armed { "Confirmer ?" } else { label };
        let s = d.s;
        d.regular.draw_centered(
            d.canvas,
            text,
            cx * s,
            (by + 54.0) * s,
            20.0 * s,
            theme::FONT_SMALL * s,
            theme::palette().text,
        );
        d.zone(bx, by, bw, bh, target);
    }
}

/// Rangée de la tuile de connectivité : pastille (bascule), titre, sous-titre, chevron (détail)
#[allow(clippy::too_many_arguments)]
fn connectivity_row(
    d: &mut Drawer,
    x: f32,
    y: f32,
    w: f32,
    (title, subtitle): (&str, &str),
    glyph: char,
    on: bool,
    targets: Option<(Target, Target)>,
) {
    if let Some((_, open)) = targets.as_ref() {
        d.hover_bg(x + 4.0, y + 4.0, w - 8.0, ROW - 8.0, open);
    }
    let toggle_hover = targets.as_ref().is_some_and(|(t, _)| d.hovered(t));
    d.circle(x + 26.0, y + ROW / 2.0, 16.0, glyph, on, toggle_hover);
    d.text(
        title,
        x + 50.0,
        y + 6.0,
        20.0,
        13.0,
        true,
        theme::palette().text,
        w - 76.0,
    );
    d.text(
        subtitle,
        x + 50.0,
        y + 24.0,
        18.0,
        theme::FONT_SMALL,
        false,
        theme::palette().text_dim,
        w - 76.0,
    );
    if let Some((toggle, open)) = targets {
        let s = d.s;
        d.icons.icon(
            d.canvas,
            icon::CHEVRON_RIGHT,
            (x + w - 16.0) * s,
            (y + ROW / 2.0) * s,
            14.0 * s,
            theme::palette().text_dim,
        );
        d.zone(x, y, w, ROW, open);
        d.zone(x + 8.0, y + 6.0, 36.0, 36.0, toggle);
    }
}

fn draw_wifi(
    d: &mut Drawer,
    st: &Snapshot,
    password_for: Option<&str>,
    password: &str,
    x: f32,
    y: f32,
    w: f32,
) {
    let wifi = &st.wifi;
    d.header(x, y, w, "Wi-Fi", Some((wifi.powered, Target::WifiToggle)));
    let mut ry = y + 52.0;
    if !wifi.powered {
        d.text(
            "Le Wi-Fi est désactivé.",
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
        return;
    }
    if wifi.networks.is_empty() {
        let label = if wifi.scanning {
            "Recherche des réseaux…"
        } else {
            "Aucun réseau trouvé"
        };
        d.text(
            label,
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
        return;
    }
    let s = d.s;
    for (i, n) in wifi.networks.iter().take(MAX_ROWS).enumerate() {
        let target = Target::Network(i);
        d.hover_bg(x, ry, w, LIST_ROW, &target);
        let fg = if n.connected {
            theme::palette().accent
        } else {
            theme::palette().text
        };
        d.icons.icon(
            d.canvas,
            wifi_glyph(n.quality),
            (x + 16.0) * s,
            (ry + LIST_ROW / 2.0) * s,
            16.0 * s,
            fg,
        );
        d.text(
            &n.name,
            x + 36.0,
            ry,
            LIST_ROW,
            13.0,
            n.connected,
            theme::palette().text,
            w - 90.0,
        );
        let mut ix = x + w - 16.0;
        if n.connected {
            d.icons.icon(
                d.canvas,
                icon::CHECK,
                ix * s,
                (ry + LIST_ROW / 2.0) * s,
                15.0 * s,
                theme::palette().accent,
            );
            ix -= 22.0;
        }
        if n.kind != "open" {
            d.icons.icon(
                d.canvas,
                icon::LOCK,
                ix * s,
                (ry + LIST_ROW / 2.0) * s,
                12.0 * s,
                theme::palette().text_dim,
            );
        }
        d.zone(x, ry, w, LIST_ROW, target);
        ry += LIST_ROW;
        // Saisie du mot de passe sous le réseau choisi
        if password_for == Some(n.path.as_str()) {
            let fh = 34.0;
            d.canvas.fill_rounded(
                (x + 8.0) * s,
                (ry + 2.0) * s,
                (w - 16.0) * s,
                fh * s,
                8.0 * s,
                theme::palette().ink(0.1),
            );
            d.canvas.stroke_rounded(
                (x + 8.0) * s,
                (ry + 2.0) * s,
                (w - 16.0) * s,
                fh * s,
                8.0 * s,
                s.max(1.0),
                theme::palette().accent,
            );
            if password.is_empty() {
                d.text(
                    "Mot de passe, puis Entrée",
                    x + 18.0,
                    ry + 2.0,
                    fh,
                    13.0,
                    false,
                    theme::palette().text_dim,
                    w - 40.0,
                );
            } else {
                let dots = "•".repeat(password.chars().count().min(32));
                d.text(
                    &dots,
                    x + 18.0,
                    ry + 2.0,
                    fh,
                    13.0,
                    false,
                    theme::palette().text,
                    w - 40.0,
                );
            }
            d.zone(x, ry, w, fh + 6.0, Target::Password);
            ry += 44.0;
        }
    }
}

fn draw_bt(d: &mut Drawer, st: &Snapshot, x: f32, y: f32, w: f32) {
    let bt = &st.bt;
    d.header(x, y, w, "Bluetooth", Some((bt.powered, Target::BtToggle)));
    let mut ry = y + 52.0;
    if !bt.present {
        d.text(
            "Aucun adaptateur Bluetooth.",
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
        return;
    }
    if !bt.powered {
        d.text(
            "Le Bluetooth est désactivé.",
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
        return;
    }
    if bt.devices.is_empty() {
        d.text(
            "Recherche d'appareils…",
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
        return;
    }
    let s = d.s;
    for (i, dev) in bt.devices.iter().take(MAX_ROWS).enumerate() {
        let target = Target::Device(i);
        d.hover_bg(x, ry, w, LIST_ROW, &target);
        d.circle(
            x + 18.0,
            ry + LIST_ROW / 2.0,
            13.0,
            bt_glyph(&dev.icon),
            dev.connected,
            false,
        );
        d.text(
            &dev.name,
            x + 40.0,
            ry,
            LIST_ROW,
            13.0,
            dev.connected,
            theme::palette().text,
            w - 130.0,
        );
        let state = if dev.connected {
            "Connecté"
        } else if dev.paired {
            "Jumelé"
        } else {
            "Disponible"
        };
        let sw = d.regular.measure(state, theme::FONT_SMALL);
        d.text(
            state,
            x + w - 8.0 - sw,
            ry,
            LIST_ROW,
            theme::FONT_SMALL,
            false,
            theme::palette().text_dim,
            sw + 4.0,
        );
        let _ = s;
        d.zone(x, ry, w, LIST_ROW, target);
        ry += LIST_ROW;
    }
}

/// Choix du thème : un aperçu de chaque palette (fond, texte, accent)
fn draw_theme(d: &mut Drawer, x: f32, y: f32, w: f32) {
    d.header(x, y, w, "Apparence", None);
    let s = d.s;
    let current = theme::palette().id;
    let mut ry = y + 52.0;
    for (i, t) in theme::THEMES.iter().enumerate() {
        let target = Target::Theme(i);
        d.hover_bg(x, ry, w, LIST_ROW, &target);
        // Vignette : fond du thème, une ligne de texte et une pastille d'accent
        let (vx, vy, vw, vh) = (x + 6.0, ry + 7.0, 44.0, LIST_ROW - 14.0);
        d.canvas
            .fill_rounded(vx * s, vy * s, vw * s, vh * s, 6.0 * s, t.background);
        d.canvas.stroke_rounded(
            vx * s,
            vy * s,
            vw * s,
            vh * s,
            6.0 * s,
            s.max(1.0),
            theme::palette().panel_border(),
        );
        d.canvas.fill_rounded(
            (vx + 7.0) * s,
            (vy + vh / 2.0 - 2.0) * s,
            16.0 * s,
            4.0 * s,
            2.0 * s,
            t.text,
        );
        d.canvas
            .fill_circle((vx + vw - 11.0) * s, (vy + vh / 2.0) * s, 5.0 * s, t.accent);
        let chosen = t.id == current;
        d.text(
            t.name,
            x + 62.0,
            ry,
            LIST_ROW,
            13.0,
            chosen,
            theme::palette().text,
            w - 92.0,
        );
        if chosen {
            d.icons.icon(
                d.canvas,
                icon::CHECK,
                (x + w - 16.0) * s,
                (ry + LIST_ROW / 2.0) * s,
                15.0 * s,
                theme::palette().accent,
            );
        }
        d.zone(x, ry, w, LIST_ROW, target);
        ry += LIST_ROW;
    }
}

fn draw_sound(d: &mut Drawer, st: &Snapshot, volume: Option<f32>, x: f32, y: f32, w: f32) {
    d.header(x, y, w, "Son", None);
    let mut ry = y + 52.0;
    let v = volume.unwrap_or(st.sound.volume);
    d.slider(
        x + 4.0,
        ry + 8.0,
        w - 8.0,
        v,
        st.sound.muted && volume.is_none(),
    );
    ry += 56.0;
    let s = d.s;
    if st.sound.sinks.is_empty() {
        d.text(
            "Aucune sortie audio",
            x + 8.0,
            ry,
            LIST_ROW,
            13.0,
            false,
            theme::palette().text_dim,
            w,
        );
    }
    for (i, sink) in st.sound.sinks.iter().take(MAX_ROWS).enumerate() {
        let target = Target::Sink(i);
        d.hover_bg(x, ry, w, LIST_ROW, &target);
        d.circle(
            x + 18.0,
            ry + LIST_ROW / 2.0,
            13.0,
            icon::SPEAKER,
            sink.default,
            false,
        );
        d.text(
            &sink.name,
            x + 40.0,
            ry,
            LIST_ROW,
            13.0,
            sink.default,
            theme::palette().text,
            w - 70.0,
        );
        if sink.default {
            d.icons.icon(
                d.canvas,
                icon::CHECK,
                (x + w - 16.0) * s,
                (ry + LIST_ROW / 2.0) * s,
                15.0 * s,
                theme::palette().accent,
            );
        }
        d.zone(x, ry, w, LIST_ROW, target);
        ry += LIST_ROW;
    }
}

/// Lance un programme d'odysseus installé à côté de celui-ci.
fn spawn_sibling(name: &str) {
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)));
    if let Some(path) = path {
        let _ = std::process::Command::new(path).spawn();
    }
}

impl LayerShellHandler for Control {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if w > 0 {
            self.width = w;
        }
        if h > 0 {
            self.height = h;
        }
        self.configured = true;
        self.draw();
    }
}

impl CompositorHandler for Control {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            self.draw();
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.render();
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl PointerHandler for Control {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    if self.dragging {
                        // Le curseur suit la souris au pixel près ; le son, par pas de 1 %
                        if let Some(v) = self.slider_value(x)
                            && self.volume != Some(v)
                        {
                            let step = |v: f32| (v * 100.0).round();
                            if self.volume.is_none_or(|old| step(old) != step(v)) {
                                self.send(Act::Volume(v));
                            }
                            self.volume = Some(v);
                            self.draw();
                        }
                        continue;
                    }
                    let hover = self.zone_at(x, y);
                    if hover != self.hover {
                        self.hover = hover;
                        self.draw();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if self.hover.take().is_some() {
                        self.draw();
                    }
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => self.click(x, y),
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } if self.dragging => {
                    self.dragging = false;
                    // La valeur relâchée reste affichée (pas de retour à l'ancienne)
                    if let Some(v) = self.volume.take() {
                        self.state.sound.volume = v;
                        self.state.sound.muted = false;
                    }
                    self.send(Act::Refresh);
                    self.draw();
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for Control {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    /// Clic ailleurs : une autre surface prend le clavier, le panneau se ferme.
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.exit = true;
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(event);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl SeatHandler for Control {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|control, _, event| control.key(event)),
            );
            self.keyboard = keyboard.ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for Control {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Control {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Control {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Control);
delegate_output!(Control);
delegate_seat!(Control);
delegate_keyboard!(Control);
delegate_pointer!(Control);
delegate_shm!(Control);
delegate_layer!(Control);
delegate_registry!(Control);
