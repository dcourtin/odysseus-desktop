//! Barre du haut : surface layer-shell ancrée en haut de l'écran. À gauche le
//! nom de la machine et les bureaux (clic pour y aller, molette pour passer au
//! suivant). À droite : l'Assistant, le moniteur système (jauge), le centre de contrôle
//! (réseau, son, Bluetooth, Réglages…), la cloche des notifications, puis la
//! date et l'heure (clic : centre de notifications). Avant eux, les icônes des
//! applications (zone de notification StatusNotifierItem, menu au clic droit).

mod sway;
mod tray;

use std::os::fd::AsFd;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use common::draw::{Canvas, Text};
use common::theme::{self, icon};
use smithay_client_toolkit::reexports::client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
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

const JOURS: [&str; 7] = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."];
const MOIS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
    "déc.",
];
/// Codes evdev des boutons de la souris
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Menu d'une icône de la zone de notification : largeur du panneau, marge
/// pour l'ombre, hauteur des lignes (en logique)
const MENU_MARGIN: f32 = 14.0;
const MENU_ROW: f32 = 30.0;
const MENU_SEPARATOR: f32 = 9.0;
const MENU_PAD: f32 = 6.0;

/// Menu ouvert : sa propre surface, au-dessus de tout, qui prend le clavier
struct TrayMenu {
    layer: LayerSurface,
    key: String,
    entries: Vec<tray::MenuEntry>,
    /// Ligne survolée ou choisie au clavier
    hover: Option<usize>,
    /// Taille logique de la surface (ombre comprise)
    width: u32,
    height: u32,
    configured: bool,
}

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
    keyboard: Option<wl_keyboard::WlKeyboard>,
    bold: Text,
    regular: Text,
    icons: Text,
    /// Zones cliquables de droite (début, fin) : icônes d'état et horloge
    status_zone: (f64, f64),
    monitor_zone: (f64, f64),
    /// Partages d'écran en cours (témoin cliquable : un clic les arrête)
    sharing: u64,
    share_zone: (f64, f64),
    assistant_zone: (f64, f64),
    /// Empreinte de la dernière image envoyée (rien n'est envoyé si elle n'a pas changé)
    last_frame: Option<u64>,
    /// Mises à jour à décider (rapport d'odysseus-veille), et l'icône qui les annonce
    updates: Updates,
    update_zone: (f64, f64),
    /// Compteurs du processeur à la mesure précédente (charge de l'icône du moniteur)
    cpu_prev: (u64, u64),
    bell_zone: (f64, f64),
    clock_zone: (f64, f64),
    /// Icônes des applications, leurs zones cliquables (début, fin, clé)
    tray: Vec<tray::Item>,
    tray_zones: Vec<(f64, f64, String)>,
    tray_cmd: std::sync::mpsc::Sender<tray::Command>,
    menu: Option<TrayMenu>,
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
    // Posée sur le flou du compositeur : palette translucide si le flou est actif
    common::theme::use_vibrancy();
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
    let (tray_tx, tray_rx) = channel::channel::<tray::Update>();
    let tray_cmd = tray::spawn(tray_tx);

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
        keyboard: None,
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        status_zone: (0.0, 0.0),
        monitor_zone: (0.0, 0.0),
        sharing: 0,
        share_zone: (0.0, 0.0),
        assistant_zone: (0.0, 0.0),
        last_frame: None,
        updates: Updates::default(),
        update_zone: (0.0, 0.0),
        cpu_prev: (0, 0),
        bell_zone: (0.0, 0.0),
        clock_zone: (0.0, 0.0),
        tray: Vec::new(),
        tray_zones: Vec::new(),
        tray_cmd,
        menu: None,
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

    // Zone de notification : icônes et menus venus du fil D-Bus
    handle
        .insert_source(tray_rx, |event, _, bar| match event {
            Event::Msg(tray::Update::Items(items)) => {
                bar.tray = items;
                bar.draw();
            }
            Event::Msg(tray::Update::Menu { key, entries }) => bar.open_menu(key, entries),
            Event::Closed => {}
        })
        .expect("canal de la zone de notification");

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
/// Mises à jour à décider, relues quand le rapport ou la liste des versions
/// écartées change (un coup d'œil aux dates, à chaque seconde)
#[derive(Default)]
struct Updates {
    stamps: (Option<std::time::SystemTime>, Option<std::time::SystemTime>),
    pending: usize,
    security: bool,
}

impl Updates {
    fn refresh(&mut self) {
        use common::updates;
        let stamp = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let stamps = (
            stamp(updates::report_path()),
            stamp(updates::ignored_path()),
        );
        if stamps == self.stamps {
            return;
        }
        self.stamps = stamps;
        let list = updates::load()
            .map(|r| updates::pending(&r))
            .unwrap_or_default();
        self.pending = list.len();
        self.security = list.iter().any(|u| u.level == updates::Level::Security);
    }
}

fn panel_open(name: &str) -> bool {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from) else {
        return false;
    };
    if name == "notifications" {
        return dir.join("odysseus-notifications.ouvert").exists();
    }
    // Fichier propre à l'écran (Assistant, Réglages), sinon commun
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    std::fs::read_to_string(dir.join(format!("odysseus-{name}.{display}.pid")))
        .or_else(|_| std::fs::read_to_string(dir.join(format!("odysseus-{name}.pid"))))
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
            self.sharing = ipc.screencast().unwrap_or(0);
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

    fn click(&mut self, x: f64, button: u32) {
        // Un clic dans la barre ferme le menu ouvert (sur la même icône : seulement le fermer)
        let menu_key = self.menu.take().map(|m| m.key);
        if let Some((x0, x1, key)) = self
            .tray_zones
            .iter()
            .find(|(x0, x1, _)| x >= *x0 && x < *x1)
            .cloned()
        {
            if menu_key.as_deref() == Some(key.as_str()) {
                self.draw();
                return;
            }
            let (cx, cy) = (((x0 + x1) / 2.0) as i32, self.height as i32);
            let cmd = match button {
                BTN_RIGHT => tray::Command::Menu(key),
                BTN_MIDDLE => tray::Command::Secondary { key, x: cx, y: cy },
                _ => tray::Command::Activate { key, x: cx, y: cy },
            };
            let _ = self.tray_cmd.send(cmd);
            self.draw();
            return;
        }
        if menu_key.is_some() {
            self.draw();
        }
        if button != BTN_LEFT {
            return;
        }
        let bell = x >= self.bell_zone.0 && x < self.bell_zone.1;
        if bell || (x >= self.clock_zone.0 && x < self.clock_zone.1) {
            spawn_sibling("odysseus-notify", &["--center"]);
            return;
        }
        if x >= self.assistant_zone.0 && x < self.assistant_zone.1 {
            spawn_sibling("odysseus-assistant", &[]);
            return;
        }
        if self.updates.pending > 0 && x >= self.update_zone.0 && x < self.update_zone.1 {
            spawn_sibling("odysseus-reglages", &["mises-a-jour"]);
            return;
        }
        if self.sharing > 0 && x >= self.share_zone.0 && x < self.share_zone.1 {
            self.sway_command("screencast stop");
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
        // Assistant (agent choisi dans Réglages > Agent)
        let ax = mx - step;
        let color = highlight(&mut canvas, ax, cy, sf, panel_open("assistant"));
        self.icons
            .icon(&mut canvas, icon::SPARKLES, ax, cy, icon_px, color);
        self.assistant_zone = (
            ((ax - step / 2.0) / sf) as f64,
            ((ax + step / 2.0) / sf) as f64,
        );
        // Témoin : un agent capture l'écran en ce moment (odysseus-mcp)
        if agent_watching() {
            let ex = ax - step;
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

        // Témoin : l'écran (ou une fenêtre) est partagé ; un clic arrête le partage
        let mut left = ax - step - if agent_watching() { step } else { 0.0 };
        self.share_zone = (0.0, 0.0);
        if self.sharing > 0 {
            let danger = theme::palette().danger;
            let label = "Partage";
            let fpx = 12.0 * sf;
            let tw = self.bold.measure(label, fpx);
            let pw = 10.0 * sf + icon_px + 6.0 * sf + tw + 12.0 * sf;
            let x1 = left + step / 2.0 - 4.0 * sf;
            let x0 = x1 - pw;
            canvas.fill_rounded(
                x0,
                cy - 11.0 * sf,
                pw,
                22.0 * sf,
                11.0 * sf,
                danger.with_alpha(0.22),
            );
            let icx = x0 + 10.0 * sf + icon_px / 2.0;
            self.icons
                .icon(&mut canvas, icon::SCREEN_SHARE, icx, cy, icon_px, danger);
            self.bold.draw_in(
                &mut canvas,
                label,
                icx + icon_px / 2.0 + 6.0 * sf,
                cy - 11.0 * sf,
                22.0 * sf,
                fpx,
                danger,
            );
            self.share_zone = ((x0 / sf) as f64, (x1 / sf) as f64);
            left = x0 - step / 2.0;
        }

        // Mises à jour à décider : l'icône n'apparaît qu'alors (rouge : sécurité)
        self.updates.refresh();
        let mut tx = left - 6.0 * sf;
        self.update_zone = (0.0, 0.0);
        if self.updates.pending > 0 {
            let color = if self.updates.security {
                theme::palette().danger
            } else {
                theme::palette().accent
            };

            self.icons
                .icon(&mut canvas, icon::ARROW_DOWN_CIRCLE, tx, cy, icon_px, color);
            self.update_zone = (
                ((tx - step / 2.0) / sf) as f64,
                ((tx + step / 2.0) / sf) as f64,
            );
            tx -= step;
        }

        // Icônes des applications, à gauche des icônes du bureau
        let size = 16.0 * sf;
        self.tray_zones.clear();
        let menu_key = self.menu.as_ref().map(|m| m.key.clone());
        for item in self.tray.iter().rev() {
            highlight(
                &mut canvas,
                tx,
                cy,
                sf,
                menu_key.as_deref() == Some(item.key.as_str()),
            );
            match &item.icon {
                Some(img) => canvas.image(img, tx - size / 2.0, cy - size / 2.0, size, size, 0.0),
                None => {
                    // Sans icône : l'initiale du titre dans une pastille
                    let p = theme::palette();
                    canvas.fill_circle(tx, cy, size / 2.0, p.subtle);
                    let initial: String = item
                        .title
                        .chars()
                        .take(1)
                        .collect::<String>()
                        .to_uppercase();
                    let fpx = 10.0 * sf;
                    let iw = self.bold.measure(&initial, fpx);
                    self.bold.draw_in(
                        &mut canvas,
                        &initial,
                        tx - iw / 2.0,
                        cy - size / 2.0,
                        size,
                        fpx,
                        p.text,
                    );
                }
            }
            if item.attention {
                canvas.fill_circle(
                    tx + 6.0 * sf,
                    cy - 6.0 * sf,
                    3.5 * sf,
                    theme::palette().accent,
                );
            }
            self.tray_zones.push((
                ((tx - step / 2.0) / sf) as f64,
                ((tx + step / 2.0) / sf) as f64,
                item.key.clone(),
            ));
            tx -= step;
        }

        // Redessinée chaque seconde (horloge, jauge) : l'image n'est envoyée que
        // si elle a changé, sinon le compositeur recomposerait l'écran pour rien
        let digest = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (w, h, self.scale).hash(&mut hasher);
            canvas.buf.hash(&mut hasher);
            hasher.finish()
        };
        if self.last_frame == Some(digest) {
            return;
        }
        self.last_frame = Some(digest);
        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        buffer.attach_to(surface).expect("attache du tampon");
        self.layer.commit();
    }
}

/// Menu de la zone de notification
impl Bar {
    fn open_menu(&mut self, key: String, entries: Vec<tray::MenuEntry>) {
        self.close_menu();
        let Some(&(x0, x1, _)) = self.tray_zones.iter().find(|(_, _, k)| *k == key) else {
            return;
        };
        let px = theme::FONT_SIZE;
        let text_w = entries
            .iter()
            .map(|e| self.regular.measure(&e.label, px) + e.depth as f32 * 14.0)
            .fold(0.0f32, f32::max);
        let check = if entries.iter().any(|e| e.toggle.is_some()) {
            22.0
        } else {
            0.0
        };
        let panel_w = (text_w + 2.0 * 14.0 + check + 4.0).clamp(180.0, 380.0);
        let panel_h: f32 = 2.0 * MENU_PAD
            + entries
                .iter()
                .map(|e| {
                    if e.separator {
                        MENU_SEPARATOR
                    } else {
                        MENU_ROW
                    }
                })
                .sum::<f32>();
        let (w, h) = (
            (panel_w + 2.0 * MENU_MARGIN).ceil() as u32,
            (panel_h + 2.0 * MENU_MARGIN).ceil() as u32,
        );
        // Centré sous l'icône, sans sortir de l'écran
        let cx = (x0 + x1) / 2.0;
        let right = (self.width as f64 - cx - w as f64 / 2.0)
            .clamp(0.0, (self.width.saturating_sub(w)) as f64);
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("odysseus-tray-menu"),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
        layer.set_size(w, h);
        layer.set_margin(0, right as i32, 0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.commit();
        self.menu = Some(TrayMenu {
            layer,
            key,
            entries,
            hover: None,
            width: w,
            height: h,
            configured: false,
        });
        self.draw();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.draw();
        }
    }

    /// Ligne sous le point (coordonnées logiques de la surface du menu)
    fn menu_row_at(&self, y: f64) -> Option<usize> {
        let menu = self.menu.as_ref()?;
        let mut top = MENU_MARGIN + MENU_PAD;
        for (i, e) in menu.entries.iter().enumerate() {
            let h = if e.separator {
                MENU_SEPARATOR
            } else {
                MENU_ROW
            };
            if (y as f32) >= top && (y as f32) < top + h {
                return (!e.separator && !e.header && e.enabled).then_some(i);
            }
            top += h;
        }
        None
    }

    fn menu_pointer(&mut self, event: &PointerEvent) {
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                let row = self.menu_row_at(event.position.1);
                if let Some(menu) = self.menu.as_mut()
                    && menu.hover != row
                {
                    menu.hover = row;
                    self.draw_menu();
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(menu) = self.menu.as_mut()
                    && menu.hover.take().is_some()
                {
                    self.draw_menu();
                }
            }
            PointerEventKind::Press {
                button: BTN_LEFT | BTN_RIGHT,
                ..
            } => {
                if let Some(row) = self.menu_row_at(event.position.1) {
                    self.choose(row);
                }
            }
            _ => {}
        }
    }

    fn choose(&mut self, row: usize) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        if let Some(e) = menu.entries.get(row) {
            let _ = self.tray_cmd.send(tray::Command::MenuEvent {
                key: menu.key.clone(),
                id: e.id,
            });
        }
        self.draw();
    }

    fn menu_key(&mut self, event: KeyEvent) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        let choosable: Vec<usize> = menu
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.separator && !e.header && e.enabled)
            .map(|(i, _)| i)
            .collect();
        let pos = menu
            .hover
            .and_then(|h| choosable.iter().position(|&i| i == h));
        match event.keysym {
            Keysym::Escape => self.close_menu(),
            Keysym::Down | Keysym::Up if !choosable.is_empty() => {
                let n = choosable.len();
                let next = match (pos, event.keysym == Keysym::Down) {
                    (None, true) => 0,
                    (None, false) => n - 1,
                    (Some(p), true) => (p + 1) % n,
                    (Some(p), false) => (p + n - 1) % n,
                };
                menu.hover = Some(choosable[next]);
                self.draw_menu();
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                if let Some(row) = menu.hover {
                    self.choose(row);
                }
            }
            _ => {}
        }
    }

    fn draw_menu(&mut self) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        if !menu.configured {
            return;
        }
        let s = self.scale.max(1) as f32;
        let (w, h) = (menu.width * s as u32, menu.height * s as u32);
        let Ok((buffer, data)) =
            self.pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut c = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        c.clear();
        let p = theme::palette();
        let (x0, y0) = (MENU_MARGIN * s, MENU_MARGIN * s);
        let (pw, ph) = (w as f32 - 2.0 * x0, h as f32 - 2.0 * y0);
        let r = 10.0 * s;
        c.shadow(x0, y0 + 4.0 * s, pw, ph, r, 18.0 * s, 0.45);
        c.fill_rounded(x0, y0, pw, ph, r, p.panel.with_alpha(p.panel.a * 0.98));
        c.stroke_rounded(x0, y0, pw, ph, r, s.max(1.0), p.panel_border());
        let px = theme::FONT_SIZE * s;
        let mut top = y0 + MENU_PAD * s;
        for (i, e) in menu.entries.iter().enumerate() {
            if e.separator {
                let y = top + MENU_SEPARATOR * s / 2.0;
                c.fill_rect(x0 + 12.0 * s, y, pw - 24.0 * s, s.max(1.0), p.ink(0.08));
                top += MENU_SEPARATOR * s;
                continue;
            }
            let row_h = MENU_ROW * s;
            if menu.hover == Some(i) {
                c.fill_rounded(
                    x0 + MENU_PAD * s,
                    top,
                    pw - 2.0 * MENU_PAD * s,
                    row_h,
                    6.0 * s,
                    p.accent.with_alpha(0.22),
                );
            }
            let indent = e.depth as f32 * 14.0 * s;
            // Colonne des coches seulement si le menu en a
            let check = if menu.entries.iter().any(|e| e.toggle.is_some()) {
                22.0 * s
            } else {
                0.0
            };
            let tx = x0 + 14.0 * s + check + indent;
            if e.toggle == Some(true) {
                self.icons.icon(
                    &mut c,
                    icon::CHECK,
                    x0 + 14.0 * s + 8.0 * s + indent,
                    top + row_h / 2.0,
                    13.0 * s,
                    p.accent,
                );
            }
            let color = if e.header || !e.enabled {
                p.text_dim
            } else if menu.hover == Some(i) {
                p.accent
            } else {
                p.text
            };
            let font = if e.header { &self.bold } else { &self.regular };
            let label = font.truncate(&e.label, px, pw - (tx - x0) - 12.0 * s);
            font.draw_in(&mut c, &label, tx, top, row_h, px, color);
            top += row_h;
        }
        let surface = menu.layer.wl_surface();
        surface.set_buffer_scale(self.scale.max(1));
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            menu.layer.commit();
        }
    }
}

impl KeyboardHandler for Bar {
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
    /// Clic ailleurs : une autre surface prend le clavier, le menu se ferme.
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self
            .menu
            .as_ref()
            .is_some_and(|m| m.layer.wl_surface() == surface)
        {
            self.close_menu();
        }
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.menu_key(event);
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
        _: Modifiers,
        _: u32,
    ) {
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
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self
            .menu
            .as_ref()
            .is_some_and(|m| m.layer.wl_surface() == layer.wl_surface())
        {
            self.close_menu();
            return;
        }
        self.layer = create_layer(&self.compositor, &self.layer_shell, &self.qh);
        self.configured = false;
        self.last_frame = None;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(menu) = self.menu.as_mut()
            && menu.layer.wl_surface() == layer.wl_surface()
        {
            menu.configured = true;
            self.draw_menu();
            return;
        }
        let (w, h) = configure.new_size;
        if w > 0 {
            self.width = w;
        }
        if h > 0 {
            self.height = h;
        }
        self.configured = true;
        // Une configuration appelle toujours une image en réponse
        self.last_frame = None;
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
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
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
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
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
            if self
                .menu
                .as_ref()
                .is_some_and(|m| &event.surface == m.layer.wl_surface())
            {
                self.menu_pointer(event);
                continue;
            }
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Press { button, .. } => self.click(event.position.0, button),
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
delegate_keyboard!(Bar);
delegate_shm!(Bar);
delegate_layer!(Bar);
delegate_registry!(Bar);
