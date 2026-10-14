//! Barre du haut : surface layer-shell ancrée en haut de l'écran. À gauche le
//! nom de la machine et les bureaux (clic pour y aller, molette pour passer au
//! suivant). À droite : le moniteur système (jauge), le centre de contrôle
//! (réseau, son, Bluetooth, Réglages…), la cloche des notifications, puis la
//! date et l'heure (clic : centre de notifications).

mod sway;

use std::os::fd::AsFd;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use common::draw::{Canvas, Text};
use common::theme::{self, icon};
use smithay_client_toolkit::reexports::client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            channel::{self, Event},
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
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

const JOURS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];
const MOIS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];
/// Code evdev du bouton gauche
const BTN_LEFT: u32 = 0x110;

struct Bar {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    /// Gardés pour recréer la surface si le compositeur la ferme (écran perdu, changement de tty)
    compositor: CompositorState,
    layer_shell: LayerShell,
    qh: QueueHandle<Bar>,
    pointer: Option<wl_pointer::WlPointer>,
    bold: Text,
    icons: Text,
    /// Zones cliquables de droite (début, fin) : icônes d'état et horloge
    status_zone: (f64, f64),
    monitor_zone: (f64, f64),
    /// Compteurs du processeur à la mesure précédente (charge de l'icône du moniteur)
    cpu_prev: (u64, u64),
    bell_zone: (f64, f64),
    clock_zone: (f64, f64),
    /// Connexion IPC pour les requêtes et commandes (None hors de sway)
    ipc: Option<sway::Ipc>,
    workspaces: Vec<sway::Workspace>,
    /// Zones cliquables des bureaux, en coordonnées logiques : (début, fin, nom)
    pills: Vec<(f64, f64, String)>,
    /// Taille logique donnée par le compositeur
    width: u32,
    height: u32,
    /// Facteur d'échelle entier du tampon (2 pour un écran à 1.5 : sway réduit, le texte reste net)
    scale: i32,
    configured: bool,
    exit: bool,
}

fn main() {
    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let layer = create_layer(&compositor, &layer_shell, &qh);

    let pool = SlotPool::new(4096 * theme::BAR_HEIGHT as usize * 4, &shm).expect("pool shm");

    let mut ipc = match sway::Ipc::connect() {
        Ok(ipc) => Some(ipc),
        Err(e) => {
            eprintln!("shell-bar : bureaux indisponibles ({e})");
            None
        }
    };
    let workspaces = ipc
        .as_mut()
        .and_then(|i| i.workspaces().ok())
        .unwrap_or_default();
    let watch_sway = ipc.is_some();

    let mut bar = Bar {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        shm,
        pool,
        layer,
        compositor,
        layer_shell,
        qh: qh.clone(),
        pointer: None,
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        status_zone: (0.0, 0.0),
        monitor_zone: (0.0, 0.0),
        cpu_prev: (0, 0),
        bell_zone: (0.0, 0.0),
        clock_zone: (0.0, 0.0),
        ipc,
        workspaces,
        pills: Vec::new(),
        width: 0,
        height: theme::BAR_HEIGHT,
        scale: 1,
        configured: false,
        exit: false,
    };

    let mut event_loop: EventLoop<Bar> = EventLoop::try_new().expect("boucle d'événements");
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .expect("source Wayland");

    // Réveil aligné sur le début de chaque seconde pour l'horloge
    handle
        .insert_source(Timer::immediate(), |_, _, bar| {
            bar.draw();
            let ms = 1000 - Local::now().timestamp_subsec_millis().min(999) as u64;
            TimeoutAction::ToDuration(Duration::from_millis(ms))
        })
        .expect("minuterie");

    // Thème changé (centre de contrôle) : redessin avec la nouvelle palette
    if let Some(fd) = theme::watch() {
        let _ = handle.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, bar| {
                theme::drain(fd.as_fd());
                if theme::reload() {
                    bar.draw();
                }
                Ok(PostAction::Continue)
            },
        );
    }

    // Panneaux ouverts ou fermés (marqueurs dans XDG_RUNTIME_DIR) : l'icône
    // correspondante s'allume ou s'éteint aussitôt
    if let Some(fd) = std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|d| common::watch::dir(std::path::Path::new(&d)))
    {
        let _ = handle.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, bar| {
                common::watch::drain(fd.as_fd());
                bar.draw();
                Ok(PostAction::Continue)
            },
        );
    }

    // Événements « workspace » de sway, lus dans un fil séparé (lecture bloquante)
    if watch_sway {
        let (tx, rx) = channel::channel::<()>();
        std::thread::spawn(move || {
            if let Err(e) = sway::watch(tx) {
                eprintln!("shell-bar : abonnement sway perdu ({e})");
            }
        });
        handle
            .insert_source(rx, |event, _, bar| {
                if let Event::Msg(()) = event {
                    bar.refresh_workspaces();
                }
            })
            .expect("canal sway");
    }

    while !bar.exit {
        // Compositeur arrêté ou connexion perdue : on s'arrête proprement
        if let Err(e) = event_loop.dispatch(None, &mut bar) {
            eprintln!("shell-bar : connexion au compositeur perdue ({e})");
            break;
        }
    }
}

/// Un agent regarde l'écran : marqueur posé par odysseus-mcp à chaque capture.
/// Il ne vaut que 6 s (même si l'agent s'arrête avant de l'enlever) ; la barre
/// se redessine chaque seconde, le témoin s'éteint donc de lui-même.
fn agent_watching() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|d| std::fs::metadata(std::path::Path::new(&d).join("odysseus-agent-ecran")).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(6))
}

/// Panneau d'odysseus ouvert : son processus vit (fichier .pid), ou, pour le
/// centre de notifications, son marqueur existe
fn panel_open(name: &str) -> bool {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from) else {
        return false;
    };
    if name == "notifications" {
        return dir.join("odysseus-notifications.ouvert").exists();
    }
    std::fs::read_to_string(dir.join(format!("odysseus-{name}.pid")))
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
        .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/stat")).ok())
        // Processus terminé mais pas encore ramassé : fermé
        .is_some_and(|stat| {
            !stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .trim_start()
                .starts_with('Z')
        })
}

/// Icône d'un panneau ouvert : pastille arrondie d'accent derrière ; renvoie
/// la couleur de l'icône
fn highlight(canvas: &mut Canvas, cx: f32, cy: f32, sf: f32, open: bool) -> theme::Rgba {
    let p = theme::palette();
    if !open {
        return p.text;
    }
    let (w, h) = (26.0 * sf, 22.0 * sf);
    canvas.fill_rounded(
        cx - w / 2.0,
        cy - h / 2.0,
        w,
        h,
        7.0 * sf,
        p.accent.with_alpha(0.22),
    );
    p.accent
}

/// « Ne pas déranger » actif, et notifications non vues (fichiers tenus par
/// odysseus-notify, relus à chaque dessin : une fois par seconde)
fn notification_state() -> (bool, bool) {
    let dnd = std::env::var_os("HOME").is_some_and(|h| {
        std::path::Path::new(&h)
            .join(".local/state/odysseus/dnd")
            .exists()
    });
    let unseen = std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|d| {
            std::fs::read_to_string(
                std::path::Path::new(&d).join("odysseus-notifications-non-vues"),
            )
            .ok()
        })
        .and_then(|t| t.trim().parse::<usize>().ok())
        .is_some_and(|n| n > 0);
    (dnd, unseen)
}

/// Lance un programme d'odysseus installé à côté de la barre.
fn spawn_sibling(name: &str, args: &[&str]) {
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)));
    match path {
        Some(path) if path.exists() => {
            match std::process::Command::new(path).args(args).spawn() {
                // Attente dans un fil : pas de processus zombie
                Ok(mut child) => {
                    std::thread::spawn(move || child.wait());
                }
                Err(e) => eprintln!("shell-bar : {name} ({e})"),
            }
        }
        _ => eprintln!("shell-bar : {name} introuvable"),
    }
}

fn create_layer(
    compositor: &CompositorState,
    layer_shell: &LayerShell,
    qh: &QueueHandle<Bar>,
) -> LayerSurface {
    let surface = compositor.create_surface(qh);
    let layer =
        layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("odysseus-bar"), None);
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, theme::BAR_HEIGHT);
    layer.set_exclusive_zone(theme::BAR_HEIGHT as i32);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();
    layer
}

impl Bar {
    fn refresh_workspaces(&mut self) {
        if let Some(ipc) = self.ipc.as_mut() {
            match ipc.workspaces() {
                Ok(list) => self.workspaces = list,
                Err(e) => eprintln!("shell-bar : lecture des bureaux ({e})"),
            }
        }
        self.draw();
    }

    fn sway_command(&mut self, cmd: &str) {
        if let Some(ipc) = self.ipc.as_mut()
            && let Err(e) = ipc.command(cmd)
        {
            eprintln!("shell-bar : commande « {cmd} » ({e})");
        }
    }

    fn click(&mut self, x: f64) {
        let bell = x >= self.bell_zone.0 && x < self.bell_zone.1;
        if bell || (x >= self.clock_zone.0 && x < self.clock_zone.1) {
            spawn_sibling("odysseus-notify", &["--center"]);
            return;
        }
        if x >= self.monitor_zone.0 && x < self.monitor_zone.1 {
            spawn_sibling("odysseus-moniteur", &[]);
            return;
        }
        if x >= self.status_zone.0 && x < self.status_zone.1 {
            spawn_sibling("odysseus-control", &[]);
            return;
        }
        let target = self
            .pills
            .iter()
            .find(|(x0, x1, _)| x >= *x0 && x < *x1)
            .map(|(_, _, name)| name.clone());
        if let Some(name) = target {
            self.sway_command(&format!("workspace \"{name}\""));
        }
    }

    /// Charge la plus forte entre processeur (depuis la mesure précédente) et
    /// mémoire, de 0 à 1 ; relue à chaque dessin (une fois par seconde).
    fn system_load(&mut self) -> f32 {
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let v: Vec<u64> = stat
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse().ok())
            .collect();
        let total: u64 = v.iter().take(8).sum();
        let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
        let (t0, i0) = std::mem::replace(&mut self.cpu_prev, (total, idle));
        let dt = total.saturating_sub(t0);
        let cpu = if t0 == 0 || dt == 0 {
            0.0
        } else {
            1.0 - idle.saturating_sub(i0) as f32 / dt as f32
        };
        let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kb = |key: &str| {
            info.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|r| r.trim_start_matches(':').split_whitespace().next())
                .and_then(|x| x.parse::<f32>().ok())
                .unwrap_or(0.0)
        };
        let mem = 1.0 - kb("MemAvailable") / kb("MemTotal").max(1.0);
        cpu.max(mem).clamp(0.0, 1.0)
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 {
            return;
        }
        let s = self.scale.max(1) as u32;
        let sf = s as f32;
        let load = self.system_load();
        let (w, h) = (self.width * s, self.height * s);
        let stride = w as i32 * 4;
        let (buffer, data) = self
            .pool
            .create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
            .expect("tampon shm");

        let mut canvas = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        canvas.fill(theme::palette().surface);

        let px = theme::FONT_SIZE * sf;
        let pad = theme::BAR_PADDING_X as f32 * sf;
        let now = Local::now();

        // Gauche : nom de la machine, puis les bureaux
        self.bold
            .draw(&mut canvas, "odysseus", pad, px, theme::palette().accent);
        let mut x = pad + self.bold.measure("odysseus", px) + (4 * theme::SPACE) as f32 * sf;

        let pill_h = theme::PILL_HEIGHT as f32 * sf;
        let pill_y = (h as f32 - pill_h) / 2.0;
        self.pills.clear();
        for ws in &self.workspaces {
            let text_w = self.bold.measure(&ws.name, px);
            let pill_w = (text_w + 2.0 * theme::PILL_PADDING_X as f32 * sf).max(pill_h);
            let (bg, fg) = if ws.urgent {
                (theme::palette().urgent, theme::palette().on_accent)
            } else if ws.focused {
                (theme::palette().accent, theme::palette().on_accent)
            } else {
                (theme::palette().subtle, theme::palette().text_dim)
            };
            canvas.fill_rounded(x, pill_y, pill_w, pill_h, theme::PILL_RADIUS * sf, bg);
            self.bold
                .draw(&mut canvas, &ws.name, x + (pill_w - text_w) / 2.0, px, fg);
            self.pills
                .push(((x / sf) as f64, ((x + pill_w) / sf) as f64, ws.name.clone()));
            x += pill_w + theme::PILL_GAP as f32 * sf;
        }

        // Droite : date et heure (« mer. 14 oct. 16:52 »), cliquables
        let clock = format!(
            "{} {} {}  {:02}:{:02}",
            JOURS[now.weekday().num_days_from_monday() as usize],
            now.day(),
            MOIS[now.month0() as usize],
            now.hour(),
            now.minute()
        );
        let clock_w = self.bold.measure(&clock, px);
        let clock_x = w as f32 - pad - clock_w;
        self.bold
            .draw(&mut canvas, &clock, clock_x, px, theme::palette().text);
        self.clock_zone = (((clock_x - 6.0 * sf) / sf) as f64, self.width as f64);

        // Cloche du centre de notifications (barrée en « Ne pas déranger »,
        // pastille quand des notifications n'ont pas été vues), puis centre de
        // contrôle et moniteur système à sa gauche
        let icon_px = 15.0 * sf;
        let step = 26.0 * sf;
        let cy = h as f32 / 2.0;
        let bx = clock_x - 14.0 * sf - step / 2.0;
        let (dnd, unseen) = notification_state();
        let bell_color = highlight(&mut canvas, bx, cy, sf, panel_open("notifications"));
        self.icons.icon(
            &mut canvas,
            if dnd { icon::BELL_OFF } else { icon::BELL },
            bx,
            cy,
            icon_px,
            bell_color,
        );
        if unseen && !dnd {
            canvas.fill_circle(
                bx + 6.0 * sf,
                cy - 6.0 * sf,
                3.5 * sf,
                theme::palette().accent,
            );
        }
        self.bell_zone = (
            ((bx - step / 2.0) / sf) as f64,
            ((clock_x - 6.0 * sf) / sf) as f64,
        );
        let ix = bx - step;
        let right = ix + step / 2.0;
        let color = highlight(&mut canvas, ix, cy, sf, panel_open("control"));
        self.icons
            .icon(&mut canvas, icon::CONTROLS, ix, cy, icon_px, color);
        // Moniteur système : jauge colorée selon la charge (processeur ou mémoire)
        let mx = ix - step;
        let color = if panel_open("moniteur") {
            highlight(&mut canvas, mx, cy, sf, true)
        } else if load >= 0.9 {
            theme::palette().danger
        } else if load >= 0.7 {
            theme::palette().accent
        } else {
            theme::palette().text
        };
        self.icons
            .icon(&mut canvas, icon::GAUGE, mx, cy, icon_px + sf, color);
        self.monitor_zone = (
            ((mx - step / 2.0) / sf) as f64,
            ((mx + step / 2.0) / sf) as f64,
        );
        // Témoin : un agent capture l'écran en ce moment (odysseus-mcp)
        if agent_watching() {
            let ex = mx - step;
            let danger = theme::palette().danger;
            canvas.fill_rounded(
                ex - 13.0 * sf,
                cy - 11.0 * sf,
                26.0 * sf,
                22.0 * sf,
                7.0 * sf,
                danger.with_alpha(0.22),
            );
            self.icons
                .icon(&mut canvas, icon::EYE, ex, cy, icon_px, danger);
        }
        let ix = ix - step;
        self.status_zone = (((ix + step / 2.0) / sf) as f64, (right / sf) as f64);

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        buffer.attach_to(surface).expect("attache du tampon");
        self.layer.commit();
    }
}

impl CompositorHandler for Bar {
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
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

impl LayerShellHandler for Bar {
    /// Surface fermée par le compositeur (sortie disparue…) : on en recrée une.
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = create_layer(&self.compositor, &self.layer_shell, &self.qh);
        self.configured = false;
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

impl SeatHandler for Bar {
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
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(e) => eprintln!("shell-bar : pointeur indisponible ({e})"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Bar {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => self.click(event.position.0),
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    let dir = if vertical.absolute > 0.0 {
                        "next_on_output"
                    } else {
                        "prev_on_output"
                    };
                    self.sway_command(&format!("workspace {dir}"));
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Bar {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Bar);
delegate_output!(Bar);
delegate_seat!(Bar);
delegate_pointer!(Bar);
delegate_shm!(Bar);
delegate_layer!(Bar);
delegate_registry!(Bar);
