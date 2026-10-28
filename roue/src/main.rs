//! odysseus-roue : roue de raccourcis qui s'ouvre autour du pointeur
//! (Super+R, clic du milieu sur le bureau, Super+clic du milieu partout).
//!
//! Huit places en cercle, choisies dans Réglages > Roue : applications (un
//! point signale celles qui sont ouvertes ; on ramène alors leur fenêtre),
//! actions d'applications, entrées proposées par les applis, dossiers, et le
//! statut (notifications, mises à jour, panneaux, icônes des applis).
//!
//! Des anneaux, jusqu'à trois : survoler une place qui a des enfants déploie
//! l'anneau suivant autour de la roue, en éventail depuis elle. Un dossier y
//! montre ses places, une appli ouverte ses fenêtres, le statut ses entrées,
//! une icône d'appli son menu (voir `node`). Clic sur une place, ou chiffres
//! 1 à 8, flèches et Entrée (dans l'anneau le plus extérieur ouvert) ; Échap
//! referme un anneau, puis la roue, comme un clic au centre ou à côté. La
//! molette fait tourner la roue. Une place vide ouvre Réglages > Roue.
//!
//! Animations, toutes pilotées par le temps et cadencées par le compositeur
//! (rappels d'image) : le disque s'ouvre et les icônes sortent du centre l'une
//! après l'autre ; au survol, le halo grandit et l'icône s'avance ; un anneau
//! se déploie, ses icônes glissant de la place qui l'ouvre à la leur ; un
//! choix lance aussitôt, puis la roue se replie en fondu.
//!
//! Arguments : `--x X --y Y` (position du pointeur sur l'écran, donnée par le
//! compositeur) ; sans eux, la roue s'ouvre là où le pointeur entre.
//! `--statut` : ouverte sur l'anneau du statut (point d'attention de la barre).
//! Sous-commandes pour les applis : voir `cli`.

mod cli;
mod node;

use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{
    apps,
    draw::{Canvas, Image, Text},
    roue::SLOTS,
    sni,
    theme::{self, Rgba, icon},
};
use node::{Act, Look, Node};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_subcompositor,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop,
            channel::{self, Event},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle, delegate_noop,
            globals::registry_queue_init,
            protocol::{
                wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_subsurface, wl_surface,
            },
        },
        protocols::wp::viewporter::client::{wp_viewport, wp_viewporter},
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
    subcompositor::SubcompositorState,
};

// Géométrie, en pixels logiques
const R_HOVER: f32 = 34.0;
const ICON: f32 = 40.0;
const R_CENTER: f32 = 36.0;
/// Par niveau (la roue, puis les anneaux) : rayon des icônes et bord extérieur
const ITEMS: [f32; LEVELS] = [100.0, 196.0, 290.0];
const OUTER: [f32; LEVELS] = [148.0, 242.0, 336.0];
/// Écart voulu entre deux places d'un anneau (degrés), et nombre au plus
const STEP: [f32; LEVELS] = [45.0, 45.0, 30.0];
const MAX_PLACES: [usize; LEVELS] = [SLOTS, 12, 18];
/// Niveaux : la roue et deux anneaux
const LEVELS: usize = 3;
/// Écart minimal entre la roue et le bord de l'écran (le premier anneau y
/// tient toujours ; le second peut déborder)
const EDGE: f32 = 16.0;
/// Apparition : la roue grandit un peu en s'ouvrant
const APPEAR: Duration = Duration::from_millis(180);
/// Sortie des icônes à l'ouverture : chacune, et décalage de l'une à la suivante
const POP: Duration = Duration::from_millis(150);
const STAGGER: Duration = Duration::from_millis(10);
/// Repli en fondu après un choix, ou à la fermeture
const CLOSE: Duration = Duration::from_millis(140);
/// Constantes de temps des suivis (survol, anneau), en secondes
const HOVER_TAU: f32 = 0.05;
const RING_TAU: f32 = 0.065;
/// Noms sous les icônes : taille, et largeur au plus (l'écart entre deux places)
const NAME_PX: f32 = 10.5;
const NAME_W: [f32; LEVELS] = [74.0, 116.0, 116.0];
/// Rotation de la roue à la molette
const SPIN_TAU: f32 = 0.07;
/// Défilement continu (pavé tactile) : pixels par cran de rotation
const SCROLL_STEP: f64 = 30.0;
/// L'icône survolée s'avance vers l'extérieur
const LIFT: f32 = 5.0;
/// Demi-côté de la sous-surface de la roue : le dernier anneau y tient
const HALF: f32 = OUTER[LEVELS - 1] + 8.0;
/// Éclaircissement d'un niveau de la roue par rapport à celui qui l'entoure
const LIGHTER: f32 = 0.10;
/// Après un clic sur une icône d'appli, le temps que le fil D-Bus le transmette
const TRAY_GRACE: Duration = Duration::from_millis(700);
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

const ICON_COLORS: [u32; 6] = [0x8aadf4, 0xc6a0f6, 0xa6e3a1, 0xf9e2af, 0xf5a3b8, 0x8bd5ca];
const RED: u32 = 0xe5484d;

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Center,
    /// Niveau (0 : la roue) et place dans son anneau
    Item(usize, usize),
    Outside,
}

/// Un anneau autour de la roue : la place qui l'a ouvert (chemin depuis la
/// roue, gardé pendant qu'il se replie), son déploiement et le survol de ses places
#[derive(Default)]
struct Ring {
    shown: Option<Vec<usize>>,
    t: f32,
    glow: Vec<f32>,
}

struct Roue {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    /// Plein écran (clics à côté, voile) ; le voile y est un seul pixel étiré
    /// par le viewport, la roue une sous-surface à sa taille : en 4K, tout
    /// redessiner à chaque image ne tiendrait pas le rythme
    layer: LayerSurface,
    wheel: wl_surface::WlSurface,
    wheel_sub: wl_subsurface::WlSubsurface,
    viewport: Option<wp_viewport::WpViewport>,
    /// Opacité du voile posé (et taille de l'écran) : on ne le repose qu'au changement
    veil: Option<(f32, u32, u32)>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
    icons: Text,
    /// Les huit places, et ce que leurs anneaux déploient
    nodes: Vec<Node>,
    /// Places dont l'anneau est ouvert, de la roue vers l'extérieur
    path: Vec<usize>,
    /// Centre voulu (position du pointeur), avant mise à l'écart des bords
    anchor: Option<(f32, f32)>,
    hover: Option<Target>,
    /// Le pointeur a quitté le centre depuis l'ouverture (geste « appuyer,
    /// glisser, relâcher » du clic du milieu)
    travelled: bool,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    opened: Instant,
    exit: bool,
    qh: QueueHandle<Roue>,
    /// Rappel d'image demandé au compositeur, pas encore reçu
    frame_pending: bool,
    last_frame: Instant,
    /// Survol des places de la roue et du centre (0 → 1)
    glow: [f32; SLOTS],
    center_glow: f32,
    /// Les anneaux, du premier au dernier
    rings: [Ring; LEVELS - 1],
    /// Repli en cours depuis…
    closing: Option<Instant>,
    /// Rotation de la roue (degrés, sens des aiguilles d'une montre) : affichée,
    /// et visée par la molette (un cran, une place)
    spin: f32,
    spin_target: f32,
    /// Défilement continu pas encore converti en crans
    scroll: f64,
    /// Dernière position du pointeur
    pointer_at: Option<(f32, f32)>,
    /// Icônes des applis : clics et demandes de menu au fil D-Bus
    tray: mpsc::Sender<sni::Command>,
    /// Ce qui attend que la roue soit partie : les panneaux (Notifications,
    /// Contrôle, Moniteur…) se ferment quand ils perdent le clavier, et la roue
    /// le garde jusqu'à la fin de son fondu ; un menu d'icône d'appli aussi
    after: Option<Act>,
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-roue.pid"))
}

/// Si une roue est déjà ouverte, la fermer et renvoyer vrai (bascule)
fn toggle_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|st| {
            st.rsplit(')')
                .next()
                .map(|r| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(true);
    if zombie || comm.trim() != "odysseus-roue" || pid == std::process::id() {
        return false;
    }
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    true
}

fn arg_f32(args: &[String], flag: &str) -> Option<f32> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

/// La place au bout d'un chemin (indices depuis la roue)
fn node_at<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(nodes.get(*first)?, |n, &j| n.children.get(j))
}

fn node_at_mut<'a>(nodes: &'a mut [Node], path: &[usize]) -> Option<&'a mut Node> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(nodes.get_mut(*first)?, |n, &j| n.children.get_mut(j))
}

/// Les places de l'anneau ouvert par le chemin `path` (la roue s'il est vide)
fn children<'a>(nodes: &'a [Node], path: &[usize]) -> &'a [Node] {
    if path.is_empty() {
        return nodes;
    }
    node_at(nodes, path).map_or(&[], |n| &n.children)
}

/// Éventail de l'anneau ouvert par `path` : angle de la place qui l'ouvre
/// (degrés depuis le haut, sans la rotation), écart entre places, nombre
fn fan(nodes: &[Node], path: &[usize]) -> (f32, f32, usize) {
    let level = path.len();
    let n = children(nodes, path)
        .len()
        .min(MAX_PLACES[level.min(LEVELS - 1)]);
    let step = STEP[level.min(LEVELS - 1)].min(360.0 / n.max(1) as f32);
    let base = match path.split_last() {
        None => 0.0,
        Some((&j, [])) => j as f32 * 360.0 / SLOTS as f32,
        Some((&j, parent)) => {
            let (b, st, m) = fan(nodes, parent);
            b + (j as f32 - (m as f32 - 1.0) / 2.0) * st
        }
    };
    (base, step, n)
}

/// Une place a-t-elle un anneau à elle (au niveau `level`) ?
fn opens(node: &Node, level: usize) -> bool {
    level + 1 < LEVELS && !node.children.is_empty()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first().filter(|a| !a.starts_with("--")) {
        let _ = first;
        std::process::exit(cli::run(&args));
    }
    if toggle_existing() {
        return;
    }
    if let Some(path) = pid_file() {
        let _ = std::fs::write(&path, std::process::id().to_string());
    }
    // Anneau et étiquettes translucides, sur le dessous flouté par le compositeur
    common::theme::use_vibrancy();

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("odysseus-roue"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    // Tout l'écran, barre comprise : un clic à côté de la roue la ferme
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .expect("wl_subcompositor indisponible");
    let (wheel_sub, wheel) = subcompositor.create_subsurface(layer.wl_surface().clone(), &qh);
    // Les clics traversent la roue jusqu'à la couche, qui fait les calculs
    if let Ok(empty) = Region::new(&compositor) {
        wheel.set_input_region(Some(empty.wl_region()));
    }
    let viewport = globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, ())
        .ok()
        .map(|v| v.get_viewport(layer.wl_surface(), &qh, ()));

    let mut event_loop: EventLoop<'static, Roue> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    // Icônes des applis (place Statut) : lues dans un fil, elles arrivent
    // quand elles sont prêtes ; la roue ne tient jamais le registre
    let (tray_tx, tray_rx) = channel::channel::<sni::Update>();
    let tray = sni::spawn(
        Box::new(move |u| {
            let _ = tray_tx.send(u);
        }),
        false,
    );
    event_loop
        .handle()
        .insert_source(tray_rx, |event, _, roue| match event {
            Event::Msg(sni::Update::Items(items)) => roue.tray_items(items),
            Event::Msg(sni::Update::Menu { key, entries }) => roue.tray_menu(&key, entries),
            Event::Closed => {}
        })
        .expect("source des icônes des applis");

    let apps = apps::load();
    let anchor = arg_f32(&args, "--x").zip(arg_f32(&args, "--y"));
    let nodes = node::build(&apps);
    // Ouverte depuis le point d'attention de la barre : sur l'anneau du statut
    let path = if args.iter().any(|a| a == "--statut") {
        nodes
            .iter()
            .position(|n| matches!(n.look, Look::Status))
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let mut roue = Roue {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 8, &shm).expect("pool shm"),
        shm,
        layer,
        wheel,
        wheel_sub,
        viewport,
        veil: None,
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        nodes,
        path,
        anchor,
        hover: None,
        travelled: false,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,
        opened: Instant::now(),
        exit: false,
        qh: qh.clone(),
        frame_pending: false,
        last_frame: Instant::now(),
        glow: [0.0; SLOTS],
        center_glow: 0.0,
        rings: Default::default(),
        closing: None,
        spin: 0.0,
        spin_target: 0.0,
        scroll: 0.0,
        pointer_at: None,
        tray,
        after: None,
    };

    while !roue.exit {
        if let Err(e) = event_loop.dispatch(None, &mut roue) {
            eprintln!("odysseus-roue : connexion au compositeur perdue ({e})");
            break;
        }
    }
    if let Some(path) = pid_file() {
        let _ = std::fs::remove_file(path);
    }
    // Ce qui attendait que la roue soit partie (connexion fermée : elle
    // n'a plus le clavier) ; un clic sur une icône d'appli part par le fil
    // D-Bus, qui a le temps de le transmettre
    let Some(act) = roue.after.take() else {
        return;
    };
    let tray = roue.tray.clone();
    drop(roue);
    drop(event_loop);
    match act {
        Act::Settings => Roue::sibling("odysseus-reglages", &["roue"]),
        Act::Sibling(name, args) => Roue::sibling(name, args),
        Act::Tray(key, x, y) => {
            let _ = tray.send(sni::Command::Activate { key, x, y });
            std::thread::sleep(TRAY_GRACE);
        }
        Act::TrayMenu(key, id) => {
            let _ = tray.send(sni::Command::MenuEvent { key, id });
            std::thread::sleep(TRAY_GRACE);
        }
        _ => {}
    }
}

impl Roue {
    /// Centre de la roue, écarté des bords de l'écran (le premier anneau y tient)
    fn center(&self) -> Option<(f32, f32)> {
        let (x, y) = self.anchor?;
        let (w, h) = (self.width as f32, self.height as f32);
        let any_ring = self.nodes.iter().any(|n| opens(n, 0));
        let m = if any_ring { OUTER[1] } else { OUTER[0] } + EDGE;
        Some((
            x.clamp(m.min(w / 2.0), (w - m).max(w / 2.0)),
            y.clamp(m.min(h / 2.0), (h - m).max(h / 2.0)),
        ))
    }

    fn node(&self, level: usize, j: usize) -> Option<&Node> {
        let mut p = self.path[..level.min(self.path.len())].to_vec();
        p.push(j);
        node_at(&self.nodes, &p)
    }

    fn target(&self, x: f32, y: f32) -> Target {
        let Some((cx, cy)) = self.center() else {
            return Target::Outside;
        };
        let (dx, dy) = (x - cx, y - cy);
        let d = (dx * dx + dy * dy).sqrt();
        if d <= R_CENTER {
            return Target::Center;
        }
        // Angle depuis le haut, sens des aiguilles d'une montre, sans la rotation
        let a = (dy.atan2(dx).to_degrees() + 90.0 - self.spin_target).rem_euclid(360.0);
        if d <= OUTER[0] {
            let step = 360.0 / SLOTS as f32;
            return Target::Item(0, ((a + step / 2.0) / step) as usize % SLOTS);
        }
        // Dans un anneau ouvert : la place de son éventail la plus proche
        let open = self.path.len().min(LEVELS - 1);
        for (level, &outer) in OUTER.iter().enumerate().take(open + 1).skip(1) {
            if d > outer {
                continue;
            }
            let (base, step, n) = fan(&self.nodes, &self.path[..level]);
            let first = base - (n as f32 - 1.0) / 2.0 * step;
            let j = ((a - first + step / 2.0).rem_euclid(360.0) / step) as usize;
            return if j < n {
                Target::Item(level, j)
            } else {
                Target::Outside
            };
        }
        Target::Outside
    }

    fn spawn(command: &str) {
        if let Err(e) = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .process_group(0)
            .spawn()
        {
            eprintln!("odysseus-roue : impossible de lancer « {command} » ({e})");
        }
    }

    /// Lance une application d'odysseus voisine de ce programme
    fn sibling(name: &str, args: &[&str]) {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(name)));
        if let Some(exe) = exe {
            let _ = std::process::Command::new(exe)
                .args(args)
                .process_group(0)
                .spawn();
        }
    }

    /// Choix d'une place : un dossier (ou le statut) ouvre son anneau, le reste
    /// agit, puis la roue se replie
    fn activate(&mut self, level: usize, j: usize) {
        let Some(node) = self.node(level, j) else {
            return;
        };
        let (act, has_ring) = (node.act.clone(), opens(node, level));
        match act {
            Act::Open if has_ring => {
                self.path.truncate(level);
                self.path.push(j);
                self.hover = Some(Target::Item(level + 1, 0));
                return self.draw();
            }
            Act::Open | Act::Nothing => return,
            Act::Run(command) => Self::spawn(&command),
            Act::App { command, windows } => {
                if windows.is_empty() {
                    Self::spawn(&command);
                } else {
                    // Ramener sa fenêtre : la suivante, si elle est déjà devant
                    let next = windows
                        .iter()
                        .position(|w| w.1)
                        .map(|f| (f + 1) % windows.len())
                        .unwrap_or(0);
                    let (id, focused) = windows[next];
                    if !focused {
                        let _ = common::ipc::command(&format!("focus {id}"));
                    }
                }
            }
            Act::Focus(id) => {
                let _ = common::ipc::command(&format!("focus {id}"));
            }
            Act::Settings | Act::Sibling(..) | Act::TrayMenu(..) => self.after = Some(act),
            Act::Tray(key, ..) => {
                let (x, y) = self.pointer_at.unwrap_or_default();
                self.after = Some(Act::Tray(key, x as i32, y as i32));
            }
        }
        self.begin_close();
    }

    /// Repli en fondu, puis sortie ; l'éventuel lancement est déjà fait
    fn begin_close(&mut self) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
            self.draw();
        }
    }

    /// Survol (souris ou clavier) : une place qui a un anneau l'ouvre (à la
    /// souris ; au clavier, seuls les dossiers et le statut), une autre referme
    /// les anneaux au-delà de la sienne
    fn set_hover(&mut self, t: Option<Target>, mouse: bool) {
        let mut path = self.path.clone();
        if let Some(Target::Item(level, j)) = t
            && let Some(node) = self.node(level, j)
        {
            path.truncate(level);
            let wanted = mouse || matches!(node.act, Act::Open);
            if opens(node, level) && wanted {
                path.push(j);
            }
            // Icône d'appli : son menu se lit maintenant, l'anneau s'ouvrira à son arrivée
            if let Some(key) = node.menu_key.clone().filter(|_| node.children.is_empty()) {
                let _ = self.tray.send(sni::Command::Menu(key));
            }
        }
        if t != self.hover || path != self.path {
            self.hover = t;
            self.path = path;
            self.draw();
        }
    }

    /// Molette : la roue tourne d'une place par cran ; ce qui arrive sous le
    /// pointeur est survolé tout de suite (un anneau s'y ouvre)
    fn spin_by(&mut self, steps: i32) {
        if steps == 0 {
            return;
        }
        self.spin_target += steps as f32 * 360.0 / SLOTS as f32;
        let hover = self
            .pointer_at
            .map(|(x, y)| self.target(x, y))
            .filter(|t| *t != Target::Outside);
        self.set_hover(hover, true);
        self.draw();
    }

    /// Referme l'anneau le plus extérieur ; la place qui l'ouvrait reste survolée
    fn close_ring(&mut self) {
        if let Some(j) = self.path.pop() {
            self.hover = Some(Target::Item(self.path.len(), j));
            self.draw();
        }
    }

    /// Icônes des applis lues (ou changées) : dans l'anneau du statut
    fn tray_items(&mut self, items: Vec<sni::Item>) {
        if let Some(status) = self
            .nodes
            .iter_mut()
            .find(|n| matches!(n.look, Look::Status))
        {
            node::set_tray(status, items);
            // Un anneau ouvert au-delà de ce qui existe encore se referme
            while !self.path.is_empty() && node_at(&self.nodes, &self.path).is_none() {
                self.path.pop();
            }
            self.draw();
        }
    }

    /// Menu d'une icône d'appli arrivé : il devient son anneau, ouvert si elle
    /// est toujours survolée
    fn tray_menu(&mut self, key: &str, entries: Vec<sni::MenuEntry>) {
        let Some(status) = self
            .nodes
            .iter_mut()
            .find(|n| matches!(n.look, Look::Status))
        else {
            return;
        };
        let Some(item) = status
            .children
            .iter_mut()
            .find(|c| c.menu_key.as_deref() == Some(key))
        else {
            return;
        };
        item.children = node::menu(key, entries);
        if let Some(Target::Item(level, j)) = self.hover
            && self.node(level, j).and_then(|n| n.menu_key.as_deref()) == Some(key)
        {
            self.set_hover(self.hover, true);
        }
    }

    fn key(&mut self, event: KeyEvent) {
        // En train de se replier : plus rien
        if self.closing.is_some() {
            return;
        }
        // Chiffres de la rangée du haut par code physique (AZERTY compris)
        let code = event.raw_code;
        let digit = if (2..=9).contains(&code) {
            Some((code - 2) as usize)
        } else {
            let k = event.keysym.raw();
            (Keysym::KP_1.raw()..=Keysym::KP_8.raw())
                .contains(&k)
                .then(|| (k - Keysym::KP_1.raw()) as usize)
        };
        // Les touches vont à l'anneau le plus extérieur ouvert
        let level = self.path.len();
        let n = fan(&self.nodes, &self.path).2;
        if let Some(j) = digit.filter(|&j| j < n) {
            return self.activate(level, j);
        }
        let current = match self.hover {
            Some(Target::Item(l, j)) if l == level => Some(j),
            _ => None,
        };
        match event.keysym {
            Keysym::Escape | Keysym::BackSpace if level > 0 => self.close_ring(),
            Keysym::Escape => self.begin_close(),
            Keysym::Return | Keysym::KP_Enter | Keysym::space => match current {
                Some(j) => self.activate(level, j),
                None if level > 0 => self.close_ring(),
                None => self.begin_close(),
            },
            Keysym::Right | Keysym::Down | Keysym::Tab if n > 0 => {
                let j = current.map_or(0, |j| (j + 1) % n);
                self.set_hover(Some(Target::Item(level, j)), false);
            }
            Keysym::Left | Keysym::Up | Keysym::ISO_Left_Tab if n > 0 => {
                let j = current.map_or(n - 1, |j| (j + n - 1) % n);
                self.set_hover(Some(Target::Item(level, j)), false);
            }
            _ => {}
        }
    }

    /// Avance les suivis (survol, anneaux) selon le temps écoulé ; vrai tant
    /// qu'une animation est en cours
    fn step(&mut self) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        let mut moving = false;
        let mut follow = |v: &mut f32, target: f32, tau: f32| {
            *v += (target - *v) * (1.0 - (-dt / tau).exp());
            if (*v - target).abs() < 0.004 {
                *v = target;
            } else {
                moving = true;
            }
        };
        for i in 0..SLOTS {
            let on = self.hover == Some(Target::Item(0, i)) || self.path.first() == Some(&i);
            follow(&mut self.glow[i], on as u8 as f32, HOVER_TAU);
        }
        let on = self.hover == Some(Target::Center);
        follow(&mut self.center_glow, on as u8 as f32, HOVER_TAU);
        // Rotation : suivie en fractions de tour, pour l'arrêt du suivi
        let mut turn = self.spin / 360.0;
        follow(&mut turn, self.spin_target / 360.0, SPIN_TAU);
        self.spin = turn * 360.0;
        // Anneaux : un autre ne se déploie qu'une fois le précédent replié, et
        // un anneau seulement quand celui d'en dessous est là
        let mut below_ready = true;
        let mut swapped = false;
        for (k, ring) in self.rings.iter_mut().enumerate() {
            let level = k + 1;
            let wanted = (self.path.len() >= level).then(|| self.path[..level].to_vec());
            let here = ring.shown.is_some() && ring.shown == wanted && below_ready;
            follow(&mut ring.t, here as u8 as f32, RING_TAU);
            if ring.t <= 0.0 && ring.shown != wanted && below_ready {
                ring.shown = wanted.clone();
                ring.glow.clear();
                // Le suivant reste à déployer : l'animation continue (sinon il
                // attendrait le prochain mouvement de la souris)
                swapped |= ring.shown.is_some();
            }
            let n = ring.shown.as_ref().map_or(0, |p| fan(&self.nodes, p).2);
            ring.glow.resize(n, 0.0);
            for (j, g) in ring.glow.iter_mut().enumerate() {
                let on = ring.shown.as_deref() == Some(&self.path[..level.min(self.path.len())])
                    && (self.hover == Some(Target::Item(level, j))
                        || self.path.get(level) == Some(&j));
                follow(g, on as u8 as f32, HOVER_TAU);
            }
            below_ready = ring.t > 0.5 && ring.shown == wanted;
        }
        let opening = self.opened.elapsed() < APPEAR.max(STAGGER * SLOTS as u32 + POP);
        moving || swapped || opening || self.closing.is_some()
    }

    /// Pose le voile s'il a changé : un pixel étiré à tout l'écran, ou à
    /// défaut de viewport un tampon plein écran
    fn set_veil(&mut self, alpha: f32) {
        let state = (alpha, self.width, self.height);
        if self.veil == Some(state) {
            return;
        }
        let s = self.scale.max(1) as u32;
        let (w, h) = match self.viewport {
            Some(_) => (1, 1),
            None => (self.width * s, self.height * s),
        };
        let Ok((buffer, data)) =
            self.pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        Canvas {
            buf: data,
            width: w,
            height: h,
        }
        .fill(Rgba::hex(0x000000, alpha));
        let surface = self.layer.wl_surface();
        match &self.viewport {
            Some(v) => {
                surface.set_buffer_scale(1);
                v.set_destination(self.width as i32, self.height as i32);
            }
            None => surface.set_buffer_scale(s as i32),
        }
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.veil = Some(state);
        }
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let animating = self.step();
        let s = self.scale.max(1) as f32;
        let p = theme::palette();
        let elapsed = self.opened.elapsed();
        let ease = ease_out(progress(elapsed, APPEAR));
        // Repli : la roue se resserre un peu en s'effaçant
        let close = self
            .closing
            .map_or(0.0, |t| ease_out(progress(t.elapsed(), CLOSE)));
        // Voile léger sur le bureau
        self.set_veil(0.10 * ease * (1.0 - close));

        // La roue, dans sa sous-surface, en coordonnées locales
        let wheel = self.center().map(|(cx, cy)| {
            let (ox, oy) = ((cx - HALF).round(), (cy - HALF).round());
            self.wheel_sub.set_position(ox as i32, oy as i32);
            (cx - ox, cy - oy)
        });
        let side = (2.0 * HALF * s).ceil() as u32;
        let Ok((buffer, data)) = self.pool.create_buffer(
            side as i32,
            side as i32,
            side as i32 * 4,
            wl_shm::Format::Argb8888,
        ) else {
            return;
        };
        let mut c = Canvas {
            buf: data,
            width: side,
            height: side,
        };
        c.clear();

        if let Some((cx, cy)) = wheel {
            let k = (0.9 + 0.1 * ease) * (1.0 - 0.06 * close);
            let (x, y) = (cx * s, cy * s);
            let ts: Vec<f32> = self.rings.iter().map(|r| r.t).collect();
            // Les anneaux, du plus extérieur au disque : chacun se déploie
            // depuis le bord du niveau d'en dessous ; chaque niveau intérieur
            // est un peu plus clair que celui qui l'entoure, et le liseré d'un
            // niveau s'efface quand le suivant s'ouvre (ils ne font plus qu'un)
            for level in (0..LEVELS).rev() {
                let t = if level == 0 { 1.0 } else { ts[level - 1] };
                if t <= 0.0 {
                    continue;
                }
                let outside: f32 = ts[level.min(LEVELS - 1)..].iter().sum();
                let next = ts.get(level).copied().unwrap_or(0.0);
                let r = if level == 0 {
                    OUTER[0]
                } else {
                    OUTER[level - 1] + (OUTER[level] - OUTER[level - 1]) * t
                } * k
                    * s;
                let border = p.panel_border();
                let ba = if level == 0 { border.a } else { 0.10 * t };
                c.fill_circle(x, y, r, border.with_alpha(ba * (1.0 - next)));
                let fill = p.panel.mix(Rgba::hex(0xffffff, 1.0), LIGHTER * outside);
                c.fill_circle(x, y, r - s, fill.with_alpha(p.panel.a * 0.97 * t));
            }
            // Centre : fermer
            c.fill_circle(
                x,
                y,
                R_CENTER * k * s,
                p.ink(0.07 + 0.05 * self.center_glow),
            );
            self.icons
                .icon(&mut c, icon::CLOSE, x, y, 16.0 * s, p.text_dim);

            let icon_px = (ICON * s).round() as u32;
            let fonts = Fonts {
                icons: &self.icons,
                bold: &self.bold,
                regular: &self.regular,
            };
            for i in 0..SLOTS.min(self.nodes.len()) {
                // Sortie depuis le centre, l'une après l'autre, avec un léger rebond
                let pop = progress(elapsed.saturating_sub(STAGGER * i as u32), POP);
                let g = self.glow[i];
                let radius = ITEMS[0] * k * (0.45 + 0.55 * ease_out_back(pop)) + LIFT * g;
                let a = (-90.0 + i as f32 * 360.0 / SLOTS as f32 + self.spin).to_radians();
                let (ix, iy) = ((cx + radius * a.cos()) * s, (cy + radius * a.sin()) * s);
                let alpha = (pop * 1.6).min(1.0);
                halo(&mut c, ix, iy, g, s);
                draw_node(
                    &mut c,
                    &fonts,
                    &mut self.nodes[i],
                    ix,
                    iy,
                    s,
                    icon_px,
                    alpha,
                    0,
                );
            }
            // Les places des anneaux glissent de la place qui les ouvre à la
            // leur, en éventail
            for (k_ring, ring) in self.rings.iter().enumerate() {
                let level = k_ring + 1;
                let Some(shown) = ring.shown.clone().filter(|_| ring.t > 0.0) else {
                    continue;
                };
                let (base, step, n) = fan(&self.nodes, &shown);
                let mid = (n as f32 - 1.0) / 2.0;
                let Some(parent) = node_at_mut(&mut self.nodes, &shown) else {
                    continue;
                };
                for (j, child) in parent.children.iter_mut().take(n).enumerate() {
                    let spread = if mid > 0.0 {
                        (j as f32 - mid).abs() / mid
                    } else {
                        0.0
                    };
                    let t = ease_out(((ring.t - 0.25 * spread) / 0.75).clamp(0.0, 1.0));
                    let deg = base + self.spin + (j as f32 - mid) * step * t;
                    let g = ring.glow.get(j).copied().unwrap_or(0.0);
                    let radius =
                        (ITEMS[level - 1] + (ITEMS[level] - ITEMS[level - 1]) * t) * k + LIFT * g;
                    let a = (deg - 90.0).to_radians();
                    let (ix, iy) = ((cx + radius * a.cos()) * s, (cy + radius * a.sin()) * s);
                    halo(&mut c, ix, iy, g, s);
                    draw_node(&mut c, &fonts, child, ix, iy, s, icon_px, t, level);
                }
            }
        }
        if close > 0.0 {
            c.fade(1.0 - close);
        }

        if wheel.is_some() && buffer.attach_to(&self.wheel).is_ok() {
            self.wheel.set_buffer_scale(self.scale);
            self.wheel.damage_buffer(0, 0, side as i32, side as i32);
            self.wheel.commit();
        }
        // Animation en cours : la prochaine image au prochain rafraîchissement
        let surface = self.layer.wl_surface();
        if animating && !self.frame_pending {
            surface.frame(&self.qh, surface.clone());
            self.frame_pending = true;
        }
        // Applique aussi la roue (sous-surface synchronisée)
        self.layer.commit();
        if self.closing.is_some_and(|t| t.elapsed() >= CLOSE) {
            self.exit = true;
        }
    }
}

/// Avancement 0 → 1 de `elapsed` sur `duration`
fn progress(elapsed: Duration, duration: Duration) -> f32 {
    (elapsed.as_secs_f32() / duration.as_secs_f32().max(1e-6)).min(1.0)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Dépasse un peu la cible avant d'y revenir (sortie des icônes)
fn ease_out_back(t: f32) -> f32 {
    let c = 1.4;
    1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2)
}

struct Fonts<'a> {
    icons: &'a Text,
    bold: &'a Text,
    regular: &'a Text,
}

/// Halo d'accent sous la place survolée (ou dont l'anneau est ouvert)
fn halo(c: &mut Canvas, ix: f32, iy: f32, g: f32, s: f32) {
    if g > 0.0 {
        let p = theme::palette();
        c.fill_circle(
            ix,
            iy,
            R_HOVER * (0.75 + 0.25 * g) * s,
            p.accent.with_alpha(0.22 * g),
        );
    }
}

/// Une place, centrée en (ix, iy) (pixels du tampon), à l'opacité `alpha` :
/// son icône, son badge, le point d'une appli ouverte, et son nom dessous
#[allow(clippy::too_many_arguments)]
fn draw_node(
    c: &mut Canvas,
    fonts: &Fonts,
    node: &mut Node,
    ix: f32,
    iy: f32,
    s: f32,
    icon_px: u32,
    alpha: f32,
    level: usize,
) {
    if alpha <= 0.0 {
        return;
    }
    let p = theme::palette();
    let fa = |c: Rgba| c.with_alpha(c.a * alpha);
    let half = ICON / 2.0 * s;
    let dim = if node.enabled { 1.0 } else { 0.4 };
    match &node.look {
        Look::Empty => {
            c.fill_circle(ix, iy, 18.0 * s, fa(p.ink(0.05)));
            fonts
                .icons
                .icon(c, icon::PLUS, ix, iy, 15.0 * s, fa(p.ink(0.35)));
            return;
        }
        Look::Label => {
            // Ligne de menu : son nom dans une pastille (une case cochée devant)
            let px = 11.5 * s;
            let check = if node.toggle == Some(true) {
                "✓ "
            } else {
                ""
            };
            let text = format!("{check}{}", node.name);
            let text = fonts.regular.truncate(&text, px, NAME_W[level] * s);
            let w = fonts.regular.measure(&text, px) + 20.0 * s;
            let h = 28.0 * s;
            c.fill_rounded(ix - w / 2.0, iy - h / 2.0, w, h, h / 2.0, fa(p.ink(0.08)));
            fonts.regular.draw_centered(
                c,
                &text,
                ix,
                iy - h / 2.0,
                h,
                px,
                fa(p.text.with_alpha(p.text.a * dim)),
            );
            return;
        }
        Look::Glyph(g) => {
            c.fill_rounded(
                ix - half,
                iy - half,
                2.0 * half,
                2.0 * half,
                12.0 * s,
                fa(p.ink(0.08)),
            );
            fonts.icons.icon(c, *g, ix, iy, 20.0 * s, fa(p.text));
        }
        Look::Folder | Look::Status => {
            let status = matches!(node.look, Look::Status);
            if status {
                // Le statut : liseré et teinte d'accent, glyphes de ses entrées
                c.fill_rounded(
                    ix - half - 1.5 * s,
                    iy - half - 1.5 * s,
                    2.0 * half + 3.0 * s,
                    2.0 * half + 3.0 * s,
                    13.5 * s,
                    fa(p.accent.with_alpha(0.55)),
                );
                c.fill_rounded(
                    ix - half,
                    iy - half,
                    2.0 * half,
                    2.0 * half,
                    12.0 * s,
                    fa(p.panel.mix(p.accent, 0.18)),
                );
            } else {
                c.fill_rounded(
                    ix - half,
                    iy - half,
                    2.0 * half,
                    2.0 * half,
                    12.0 * s,
                    fa(p.accent.with_alpha(0.20)),
                );
            }
            draw_preview(c, fonts, node, ix, iy, s, alpha);
        }
        Look::Icon(name) => {
            let name = name.clone();
            let img = node
                .icon
                .get_or_insert_with(|| common::icons::load(&name, icon_px));
            draw_image(c, fonts, img.as_ref(), &node.name, ix, iy, s, alpha);
        }
        Look::Image(img) => {
            // Icône d'appli tierce : à la taille des places
            match img {
                Some(img) => c.image_alpha(
                    img,
                    ix - half,
                    iy - half,
                    2.0 * half,
                    2.0 * half,
                    0.0,
                    alpha,
                ),
                None => draw_image(c, fonts, None, &node.name, ix, iy, s, alpha),
            }
        }
    }
    // Action ou proposition : petit repère dans le coin
    if node.marker {
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 8.0 * s, fa(p.panel));
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 6.5 * s, fa(p.accent));
    }
    if node.running {
        c.fill_circle(ix, iy + half + 4.5 * s, 2.5 * s, fa(p.text_dim));
    }
    if let Some(b) = &node.badge {
        draw_badge(c, fonts.bold, ix, iy, half, s, alpha, b);
    }
    // Le nom dessous, coupé à la largeur entre deux places voisines
    let px = NAME_PX * s;
    let name = fonts.regular.truncate(&node.name, px, NAME_W[level] * s);
    fonts.regular.draw_centered(
        c,
        &name,
        ix,
        iy + (ICON / 2.0 + 7.0) * s,
        13.0 * s,
        px,
        p.text.with_alpha(p.text.a * 0.9 * alpha),
    );
}

/// Image d'une appli à sa taille exacte, centrée (nette ; une icône pas
/// carrée garde ses proportions), ou son initiale sur une pastille de couleur
#[allow(clippy::too_many_arguments)]
fn draw_image(
    c: &mut Canvas,
    fonts: &Fonts,
    img: Option<&Image>,
    name: &str,
    ix: f32,
    iy: f32,
    s: f32,
    alpha: f32,
) {
    let half = ICON / 2.0 * s;
    match img {
        Some(img) => {
            let (w, h) = (img.width as f32, img.height as f32);
            c.image_alpha(img, ix - w / 2.0, iy - h / 2.0, w, h, 0.0, alpha)
        }
        None => {
            let color = initial_color(name);
            c.fill_circle(ix, iy, half, color.with_alpha(color.a * alpha));
            let initial: String = name.chars().take(1).collect::<String>().to_uppercase();
            fonts.bold.draw_centered(
                c,
                &initial,
                ix,
                iy - half,
                2.0 * half,
                18.0 * s,
                Rgba::hex(0x1d1916, alpha),
            );
        }
    }
}

/// Aperçu d'un dossier ou du statut : ses quatre premières places en petit
/// (icônes des applis, glyphes des entrées du statut), sur deux colonnes
fn draw_preview(
    c: &mut Canvas,
    fonts: &Fonts,
    node: &mut Node,
    ix: f32,
    iy: f32,
    s: f32,
    alpha: f32,
) {
    let p = theme::palette();
    let shown: Vec<&mut Node> = node
        .children
        .iter_mut()
        .filter(|n| !matches!(n.look, Look::Empty))
        .take(4)
        .collect();
    if shown.is_empty() {
        fonts.icons.icon(
            c,
            icon::FOLDER,
            ix,
            iy,
            20.0 * s,
            p.accent.with_alpha(p.accent.a * alpha),
        );
        return;
    }
    // Aperçus chargés à leur taille exacte : nets, sans réduction au dessin
    let mini_px = (15.0 * s).round() as u32;
    let mini = mini_px as f32;
    let gap = 3.0 * s;
    let cols = shown.len().min(2) as f32;
    let rows = shown.len().div_ceil(2) as f32;
    let x0 = ix - (cols * mini + (cols - 1.0) * gap) / 2.0;
    let y0 = iy - (rows * mini + (rows - 1.0) * gap) / 2.0;
    for (n, child) in shown.into_iter().enumerate() {
        let (col, row) = ((n % 2) as f32, (n / 2) as f32);
        let mx = x0 + col * (mini + gap);
        let my = y0 + row * (mini + gap);
        let (mcx, mcy) = (mx + mini / 2.0, my + mini / 2.0);
        match &child.look {
            Look::Glyph(g) => {
                fonts
                    .icons
                    .icon(c, *g, mcx, mcy, 13.0 * s, p.text.with_alpha(0.9 * alpha))
            }
            Look::Icon(name) => {
                let name = name.clone();
                match child
                    .mini
                    .get_or_insert_with(|| common::icons::load(&name, mini_px))
                {
                    // Une icône pas carrée garde ses proportions, centrée
                    Some(img) => {
                        let (w, h) = (img.width as f32, img.height as f32);
                        let (x, y) = (mx + (mini - w) / 2.0, my + (mini - h) / 2.0);
                        c.image_alpha(img, x, y, w, h, 0.0, alpha)
                    }
                    None => {
                        let color = initial_color(&child.name);
                        c.fill_circle(mcx, mcy, mini / 2.0, color.with_alpha(alpha));
                    }
                }
            }
            Look::Image(Some(img)) => c.image_alpha(img, mx, my, mini, mini, 0.0, alpha),
            _ => {
                let color = initial_color(&child.name);
                c.fill_circle(mcx, mcy, mini / 2.0, color.with_alpha(alpha));
            }
        }
    }
}

/// Badge en haut à droite : un nombre ou « ! » (rouge s'il est urgent), ou un
/// simple point
#[allow(clippy::too_many_arguments)]
fn draw_badge(
    c: &mut Canvas,
    bold: &Text,
    ix: f32,
    iy: f32,
    half: f32,
    s: f32,
    alpha: f32,
    badge: &node::Badge,
) {
    let p = theme::palette();
    let color = if badge.urgent {
        Rgba::hex(RED, 1.0)
    } else {
        p.accent
    };
    let br = if badge.text.is_empty() { 4.5 } else { 8.0 } * s;
    let (bx, by) = (ix + half * 0.85, iy - half * 0.85);
    c.fill_circle(bx, by, br + 1.5 * s, p.panel.with_alpha(alpha));
    c.fill_circle(bx, by, br, color.with_alpha(alpha));
    if !badge.text.is_empty() {
        bold.draw_centered(
            c,
            &badge.text,
            bx,
            by - br,
            2.0 * br,
            10.5 * s,
            Rgba::hex(0xffffff, alpha),
        );
    }
}

/// Couleur de pastille d'une appli sans icône, tirée de son nom
fn initial_color(name: &str) -> Rgba {
    let hash = name.bytes().map(|b| b as usize).sum::<usize>();
    Rgba::hex(ICON_COLORS[hash % ICON_COLORS.len()], 1.0)
}

impl CompositorHandler for Roue {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            for node in &mut self.nodes {
                node.forget_icons();
            }
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
    /// Image suivante d'une animation
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        self.draw();
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

impl LayerShellHandler for Roue {
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
        self.width = configure.new_size.0.max(1);
        self.height = configure.new_size.1.max(1);
        // L'ouverture s'anime à partir de la première image, pas du lancement
        if !self.configured {
            self.opened = Instant::now();
            self.last_frame = self.opened;
        }
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for Roue {
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
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => eprintln!("odysseus-roue : clavier indisponible ({e})"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => eprintln!("odysseus-roue : pointeur indisponible ({e})"),
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

impl KeyboardHandler for Roue {
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
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
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
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for Roue {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() || self.closing.is_some() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            self.pointer_at = Some((x, y));
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // Sans position donnée par le compositeur : là où le pointeur entre
                    if self.anchor.is_none() {
                        self.anchor = Some((x, y));
                    }
                    let t = self.target(x, y);
                    if matches!(t, Target::Item(..)) {
                        self.travelled = true;
                    }
                    let hover = (t != Target::Outside).then_some(t);
                    self.set_hover(hover, true);
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => match self.target(x, y) {
                    Target::Item(level, j) => self.activate(level, j),
                    _ => self.begin_close(),
                },
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = if vertical.discrete != 0 {
                        self.scroll = 0.0;
                        vertical.discrete
                    } else {
                        self.scroll += vertical.absolute;
                        let n = (self.scroll / SCROLL_STEP).trunc();
                        self.scroll -= n * SCROLL_STEP;
                        n as i32
                    };
                    self.spin_by(steps);
                }
                // Clic du milieu maintenu, glissé vers une place puis relâché
                PointerEventKind::Release {
                    button: BTN_MIDDLE, ..
                } if self.travelled => {
                    // Relâché sur un dossier (ou le statut) : il reste ouvert
                    if let Target::Item(level, j) = self.target(x, y)
                        && self
                            .node(level, j)
                            .is_some_and(|n| !matches!(n.act, Act::Open))
                    {
                        self.activate(level, j);
                    }
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Roue {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Roue {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Roue {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Roue);
delegate_output!(Roue);
delegate_seat!(Roue);
delegate_keyboard!(Roue);
delegate_pointer!(Roue);
delegate_shm!(Roue);
delegate_layer!(Roue);
delegate_registry!(Roue);
delegate_subcompositor!(Roue);
delegate_noop!(Roue: ignore wp_viewporter::WpViewporter);
delegate_noop!(Roue: ignore wp_viewport::WpViewport);
