//! odysseus-roue : roue de raccourcis qui s'ouvre autour du pointeur
//! (Super+R, clic du milieu sur le bureau, Super+clic du milieu partout).
//!
//! Huit places en cercle, choisies dans Réglages > Roue : applications (un
//! point signale celles qui sont ouvertes ; on ramène alors leur fenêtre),
//! actions d'applications, entrées proposées par les applis, dossiers. Un
//! dossier ouvre un second anneau autour de la roue, à son survol ou à son
//! choix. Clic sur une place, ou chiffres 1 à 8, flèches et Entrée ; Échap
//! referme le dossier puis la roue, comme un clic au centre ou à côté. Une
//! place vide ouvre Réglages > Roue.
//!
//! Animations, toutes pilotées par le temps et cadencées par le compositeur
//! (rappels d'image) : le disque s'ouvre et les icônes sortent du centre l'une
//! après l'autre ; au survol, le halo grandit et l'icône s'avance ; l'anneau
//! d'un dossier se déploie, ses icônes glissant de la place du dossier à la
//! leur ; un choix lance aussitôt, puis la roue se replie en fondu.
//!
//! Arguments : `--x X --y Y` (position du pointeur sur l'écran, donnée par le
//! compositeur) ; sans eux, la roue s'ouvre là où le pointeur entre.
//! Sous-commandes pour les applis : voir `cli`.

mod cli;

use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    apps::{self, App},
    draw::{Canvas, Image, Text},
    roue::{self, Item, SLOTS, Slot},
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
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

// Géométrie, en pixels logiques
const R_DISC: f32 = 148.0;
const R_ITEMS: f32 = 100.0;
const R_HOVER: f32 = 34.0;
const ICON: f32 = 40.0;
const R_CENTER: f32 = 36.0;
/// Second anneau (dossier ouvert) : rayon des icônes et bord extérieur
const R_RING_ITEMS: f32 = 196.0;
const R_RING: f32 = 242.0;
/// Écart minimal entre la roue et le bord de l'écran
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
/// Fondu de l'étiquette quand elle change
const LABEL_FADE: Duration = Duration::from_millis(90);
/// L'icône survolée s'avance vers l'extérieur
const LIFT: f32 = 5.0;
/// Éclaircissement d'un niveau de la roue par rapport à celui qui l'entoure
const LIGHTER: f32 = 0.10;
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

const ICON_COLORS: [u32; 6] = [0x8aadf4, 0xc6a0f6, 0xa6e3a1, 0xf9e2af, 0xf5a3b8, 0x8bd5ca];

/// Une place de la roue
struct Place {
    item: Option<Item>,
    /// Fenêtres de l'appli (identifiant, active) ; vide : pas ouverte
    windows: Vec<(u64, bool)>,
    icon: Option<Option<Image>>,
    /// Même icône, à la taille des aperçus d'un dossier
    mini: Option<Option<Image>>,
    /// Dossier : son nom et ses places remplies (une place vide s'il n'en a pas)
    folder: Option<(String, Vec<Place>)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Center,
    Slot(usize),
    /// Place du dossier ouvert
    Inner(usize),
    Outside,
}

struct Roue {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
    icons: Text,
    places: Vec<Place>,
    /// Centre voulu (position du pointeur), avant mise à l'écart des bords
    anchor: Option<(f32, f32)>,
    hover: Option<Target>,
    /// Dossier ouvert (place de la roue)
    open: Option<usize>,
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
    /// Survol des places de la roue, du dossier ouvert et du centre (0 → 1)
    glow: [f32; SLOTS],
    inner_glow: [f32; SLOTS],
    center_glow: f32,
    /// Déploiement de l'anneau (0 → 1) et dossier qu'il montre (gardé pendant
    /// qu'il se replie)
    ring_t: f32,
    ring_folder: Option<usize>,
    /// Étiquette affichée et depuis quand
    label: Option<(String, Instant)>,
    /// Repli en cours depuis…
    closing: Option<Instant>,
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

/// Fenêtres ouvertes, d'après le compositeur : (identifiant, app_id, active)
fn windows() -> Vec<(u64, String, bool)> {
    let Ok(json) = common::ipc::request(common::ipc::GET_WINDOWS, "") else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str(&json) else {
        return Vec::new();
    };
    list.iter()
        .map(|w| {
            (
                w["id"].as_u64().unwrap_or(0),
                w["app_id"].as_str().unwrap_or_default().to_string(),
                w["focused"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

fn places(apps: &[App]) -> Vec<Place> {
    let catalog = roue::catalog(apps);
    let wins = windows();
    let place = |slot: Option<&Slot>| {
        let item = slot.and_then(|s| catalog.iter().find(|it| &it.slot == s).cloned());
        let windows = item
            .as_ref()
            .and_then(|it| it.app)
            .map(|ai| {
                wins.iter()
                    .filter(|(_, app_id, _)| apps::for_window(app_id, apps) == Some(ai))
                    .map(|(id, _, focused)| (*id, *focused))
                    .collect()
            })
            .unwrap_or_default();
        Place {
            item,
            windows,
            icon: None,
            mini: None,
            folder: None,
        }
    };
    roue::load(apps)
        .iter()
        .map(|slot| match slot.as_ref().and_then(Slot::folder) {
            Some(f) => {
                let mut inner: Vec<Place> = f.filled().map(|s| place(Some(s))).collect();
                if inner.is_empty() {
                    inner.push(place(None));
                }
                Place {
                    folder: Some((f.name.clone(), inner)),
                    ..place(None)
                }
            }
            None => place(slot.as_ref()),
        })
        .collect()
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

    let mut event_loop: EventLoop<'static, Roue> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let apps = apps::load();
    let anchor = arg_f32(&args, "--x").zip(arg_f32(&args, "--y"));
    let mut roue = Roue {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 8, &shm).expect("pool shm"),
        shm,
        layer,
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        places: places(&apps),
        anchor,
        hover: None,
        open: None,
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
        inner_glow: [0.0; SLOTS],
        center_glow: 0.0,
        ring_t: 0.0,
        ring_folder: None,
        label: None,
        closing: None,
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
}

impl Roue {
    /// Centre de la roue, écarté des bords de l'écran
    fn center(&self) -> Option<(f32, f32)> {
        let (x, y) = self.anchor?;
        let (w, h) = (self.width as f32, self.height as f32);
        // Place pour le second anneau s'il y a un dossier
        let outer = if self.places.iter().any(|p| p.folder.is_some()) {
            R_RING
        } else {
            R_DISC
        };
        let m = outer + EDGE;
        // Place pour le nom sous la roue
        let below = 48.0;
        Some((
            x.clamp(m.min(w / 2.0), (w - m).max(w / 2.0)),
            y.clamp(m.min(h / 2.0), (h - m - below).max(h / 2.0)),
        ))
    }

    /// Angle (degrés depuis le haut) et nombre des places du dossier ouvert
    fn ring(&self) -> Option<(f32, usize)> {
        let i = self.open?;
        let n = self.places.get(i)?.folder.as_ref()?.1.len();
        Some((i as f32 * 360.0 / SLOTS as f32, n))
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
        // Angle depuis le haut, sens des aiguilles d'une montre
        let a = (dy.atan2(dx).to_degrees() + 90.0).rem_euclid(360.0);
        let step = 360.0 / SLOTS as f32;
        if d > R_DISC {
            // Dans le second anneau : la place du dossier la plus proche
            let Some((base, n)) = self.ring().filter(|_| d <= R_RING) else {
                return Target::Outside;
            };
            let first = base - (n as f32 - 1.0) / 2.0 * step;
            let off = (a - first + step / 2.0).rem_euclid(360.0);
            let j = (off / step) as usize;
            return if j < n {
                Target::Inner(j)
            } else {
                Target::Outside
            };
        }
        Target::Slot(((a + step / 2.0) / step) as usize % SLOTS)
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

    /// Choix d'une place de la roue : un dossier s'ouvre, le reste se lance
    fn activate(&mut self, i: usize) {
        if self.places.get(i).is_some_and(|p| p.folder.is_some()) {
            self.open = Some(i);
            self.hover = Some(Target::Inner(0));
            return self.draw();
        }
        if let Some(place) = self.places.get(i) {
            Self::launch(place);
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

    /// Choix d'une place du dossier ouvert
    fn activate_inner(&mut self, j: usize) {
        let place = self
            .open
            .and_then(|i| self.places.get(i))
            .and_then(|p| p.folder.as_ref())
            .and_then(|f| f.1.get(j));
        if let Some(place) = place {
            Self::launch(place);
            self.begin_close();
        }
    }

    fn launch(place: &Place) {
        let Some(item) = place.item.as_ref() else {
            // Place vide : la remplir dans Réglages
            return Self::sibling("odysseus-reglages", &["roue"]);
        };
        // Appli ouverte : ramener sa fenêtre (la suivante, si elle est déjà active)
        if !place.windows.is_empty() {
            let next = place
                .windows
                .iter()
                .position(|w| w.1)
                .map(|f| (f + 1) % place.windows.len())
                .unwrap_or(0);
            let (id, focused) = place.windows[next];
            if !focused {
                let _ = common::ipc::command(&format!("focus {id}"));
            }
            return;
        }
        Self::spawn(&item.command);
    }

    fn set_hover(&mut self, t: Option<Target>) {
        // Survoler un dossier l'ouvre, survoler une autre place le referme
        let open = match t {
            Some(Target::Slot(i)) => self.places[i].folder.is_some().then_some(i),
            _ => self.open,
        };
        if t != self.hover || open != self.open {
            self.hover = t;
            self.open = open;
            self.draw();
        }
    }

    fn close_folder(&mut self) {
        if let Some(i) = self.open.take() {
            self.hover = Some(Target::Slot(i));
            self.draw();
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
        // Dossier ouvert : les touches vont à ses places
        if let Some((_, n)) = self.ring() {
            let current = match self.hover {
                Some(Target::Inner(j)) => Some(j),
                _ => None,
            };
            if let Some(j) = digit {
                return self.activate_inner(j);
            }
            match event.keysym {
                Keysym::Escape | Keysym::BackSpace => self.close_folder(),
                Keysym::Return | Keysym::KP_Enter | Keysym::space => match current {
                    Some(j) => self.activate_inner(j),
                    None => self.close_folder(),
                },
                Keysym::Right | Keysym::Down | Keysym::Tab => {
                    let j = current.map_or(0, |j| (j + 1) % n);
                    self.set_hover(Some(Target::Inner(j)));
                }
                Keysym::Left | Keysym::Up | Keysym::ISO_Left_Tab => {
                    let j = current.map_or(n - 1, |j| (j + n - 1) % n);
                    self.set_hover(Some(Target::Inner(j)));
                }
                _ => {}
            }
            return;
        }
        if let Some(i) = digit {
            return self.activate(i);
        }
        let current = match self.hover {
            Some(Target::Slot(i)) => Some(i),
            _ => None,
        };
        match event.keysym {
            Keysym::Escape => self.begin_close(),
            Keysym::Return | Keysym::KP_Enter | Keysym::space => match current {
                Some(i) => self.activate(i),
                None => self.begin_close(),
            },
            Keysym::Right | Keysym::Down | Keysym::Tab => {
                let i = current.map_or(0, |i| (i + 1) % SLOTS);
                self.set_hover(Some(Target::Slot(i)));
            }
            Keysym::Left | Keysym::Up | Keysym::ISO_Left_Tab => {
                let i = current.map_or(SLOTS - 1, |i| (i + SLOTS - 1) % SLOTS);
                self.set_hover(Some(Target::Slot(i)));
            }
            _ => {}
        }
    }

    /// Avance les suivis (survol, anneau) selon le temps écoulé ; vrai tant
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
            let on = self.hover == Some(Target::Slot(i)) || self.open == Some(i);
            follow(&mut self.glow[i], on as u8 as f32, HOVER_TAU);
            let on = self.hover == Some(Target::Inner(i));
            follow(&mut self.inner_glow[i], on as u8 as f32, HOVER_TAU);
        }
        let on = self.hover == Some(Target::Center);
        follow(&mut self.center_glow, on as u8 as f32, HOVER_TAU);
        // Anneau : un autre dossier ne se déploie qu'une fois le précédent replié
        let target = (self.open.is_some() && self.ring_folder == self.open) as u8 as f32;
        follow(&mut self.ring_t, target, RING_TAU);
        if self.ring_t <= 0.0 && self.ring_folder != self.open {
            self.ring_folder = self.open;
            self.inner_glow = [0.0; SLOTS];
            // Le suivant reste à déployer : l'animation continue (sinon il
            // attendrait le prochain mouvement de la souris)
            moving |= self.open.is_some();
        }
        let opening = self.opened.elapsed() < APPEAR.max(STAGGER * SLOTS as u32 + POP);
        let label = self
            .label
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() < LABEL_FADE);
        moving || opening || label || self.closing.is_some()
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let animating = self.step();
        let s = self.scale.max(1) as f32;
        let center = self.center();
        let (w, h) = (self.width * s as u32, self.height * s as u32);
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
        let elapsed = self.opened.elapsed();
        let ease = ease_out(progress(elapsed, APPEAR));
        // Repli : la roue se resserre un peu en s'effaçant
        let close = self
            .closing
            .map_or(0.0, |t| ease_out(progress(t.elapsed(), CLOSE)));
        // Voile léger sur le bureau
        c.fill(Rgba::hex(0x000000, 0.10 * ease));

        if let Some((cx, cy)) = center {
            let k = (0.9 + 0.1 * ease) * (1.0 - 0.06 * close);
            let (x, y, r) = (cx * s, cy * s, R_DISC * k * s);
            // Dossier ouvert : un anneau qui se déploie autour de la roue
            let rt = self.ring_t;
            let ro = (R_DISC + (R_RING - R_DISC) * rt) * k * s;
            if rt > 0.0 {
                c.shadow(
                    x - ro,
                    y - ro + 10.0 * s,
                    2.0 * ro,
                    2.0 * ro,
                    ro,
                    36.0 * s,
                    0.35 * rt,
                );
                c.fill_circle(x, y, ro, p.panel_border().with_alpha(0.10 * rt));
                c.fill_circle(x, y, ro - s, p.panel.with_alpha(0.97 * rt));
            }
            c.shadow(
                x - r,
                y - r + 10.0 * s,
                2.0 * r,
                2.0 * r,
                r,
                36.0 * s,
                0.45 * (1.0 - rt) * ease,
            );
            // Liseré du disque, effacé quand l'anneau se déploie : roue et anneau
            // ne font plus qu'un disque
            let border = p.panel_border();
            c.fill_circle(x, y, r, border.with_alpha(border.a * (1.0 - rt)));
            // Chaque niveau intérieur 10 % plus clair que celui qui l'entoure :
            // dossier ouvert, le disque de la roue se détache de l'anneau
            let disc = p.panel.mix(Rgba::hex(0xffffff, 1.0), LIGHTER * rt);
            c.fill_circle(x, y, r - s, disc.with_alpha(0.97));
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
            let fonts = (&self.icons, &self.bold);
            for i in 0..SLOTS {
                // Sortie depuis le centre, l'une après l'autre, avec un léger rebond
                let pop = progress(elapsed.saturating_sub(STAGGER * i as u32), POP);
                let g = self.glow[i];
                let radius = R_ITEMS * k * (0.45 + 0.55 * ease_out_back(pop)) + LIFT * g;
                let a = (-90.0 + i as f32 * 360.0 / SLOTS as f32).to_radians();
                let (ix, iy) = ((cx + radius * a.cos()) * s, (cy + radius * a.sin()) * s);
                let alpha = (pop * 1.6).min(1.0);
                if g > 0.0 {
                    c.fill_circle(
                        ix,
                        iy,
                        R_HOVER * (0.75 + 0.25 * g) * s,
                        p.accent.with_alpha(0.22 * g),
                    );
                }
                draw_place(
                    &mut c,
                    fonts,
                    &mut self.places[i],
                    ix,
                    iy,
                    s,
                    icon_px,
                    alpha,
                );
            }
            if let Some(f) = self.ring_folder
                && rt > 0.0
                && let Some((_, inner)) = self.places[f].folder.as_mut()
            {
                // Les icônes du dossier glissent de sa place à la leur, en éventail
                let n = inner.len();
                let base = f as f32 * 360.0 / SLOTS as f32;
                let mid = (n as f32 - 1.0) / 2.0;
                for (j, place) in inner.iter_mut().enumerate() {
                    let spread = if mid > 0.0 {
                        (j as f32 - mid).abs() / mid
                    } else {
                        0.0
                    };
                    let t = ((rt - 0.25 * spread) / 0.75).clamp(0.0, 1.0);
                    let t = ease_out(t);
                    let deg = base + (j as f32 - mid) * (360.0 / SLOTS as f32) * t;
                    let g = self.inner_glow[j];
                    let radius = (R_ITEMS + (R_RING_ITEMS - R_ITEMS) * t) * k + LIFT * g;
                    let a = (deg - 90.0).to_radians();
                    let (ix, iy) = ((cx + radius * a.cos()) * s, (cy + radius * a.sin()) * s);
                    if g > 0.0 {
                        c.fill_circle(
                            ix,
                            iy,
                            R_HOVER * (0.75 + 0.25 * g) * s,
                            p.accent.with_alpha(0.22 * g),
                        );
                    }
                    draw_place(&mut c, fonts, place, ix, iy, s, icon_px, t);
                }
            }

            // Nom de la place survolée, sous la roue (fondu quand il change)
            let describe = |place: &Place| match (&place.folder, place.item.as_ref()) {
                (Some((name, _)), _) => format!("{name} · dossier"),
                (None, Some(it)) if it.app.is_none() && !it.detail.is_empty() => {
                    format!("{} · {}", it.name, it.detail)
                }
                (None, Some(it)) => it.name.clone(),
                (None, None) => "Choisir dans Réglages".to_string(),
            };
            let text = match self.hover {
                Some(Target::Slot(i)) => Some(describe(&self.places[i])),
                Some(Target::Inner(j)) => self
                    .open
                    .and_then(|i| self.places[i].folder.as_ref())
                    .and_then(|f| f.1.get(j))
                    .map(describe),
                Some(Target::Center) => Some("Fermer".to_string()),
                _ => None,
            };
            if text.as_deref() != self.label.as_ref().map(|(l, _)| l.as_str()) {
                self.label = text.map(|t| (t, Instant::now()));
            }
            if let Some((label, since)) = self.label.as_ref() {
                let la = progress(since.elapsed(), LABEL_FADE);
                let px = 13.5 * s;
                let label = self.regular.truncate(label, px, 360.0 * s);
                let lw = self.regular.measure(&label, px) + 28.0 * s;
                let (lx, ly, lh) = (x - lw / 2.0, y + ro.max(r) + (8.0 + 4.0 * la) * s, 32.0 * s);
                c.fill_rounded(
                    lx,
                    ly,
                    lw,
                    lh,
                    lh / 2.0,
                    p.panel_border().with_alpha(0.10 * la),
                );
                c.fill_rounded(
                    lx + s,
                    ly + s,
                    lw - 2.0 * s,
                    lh - 2.0 * s,
                    lh / 2.0 - s,
                    p.panel.with_alpha(0.97 * la),
                );
                self.regular
                    .draw_centered(&mut c, &label, x, ly, lh, px, p.text.with_alpha(la));
            }
        }
        if close > 0.0 {
            c.fade(1.0 - close);
        }

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        // Animation en cours : la prochaine image au prochain rafraîchissement
        if animating && !self.frame_pending {
            surface.frame(&self.qh, surface.clone());
            self.frame_pending = true;
        }
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
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

/// Icône d'une place, centrée en (ix, iy) (pixels de l'écran), à l'opacité `alpha`
#[allow(clippy::too_many_arguments)]
fn draw_place(
    c: &mut Canvas,
    (icons, bold): (&Text, &Text),
    place: &mut Place,
    ix: f32,
    iy: f32,
    s: f32,
    icon_px: u32,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let p = theme::palette();
    let fa = |c: Rgba| c.with_alpha(c.a * alpha);
    let half = ICON / 2.0 * s;
    if let Some((_, inner)) = place.folder.as_mut() {
        // Dossier : une pastille avec ses premières icônes en petit
        c.fill_rounded(
            ix - half,
            iy - half,
            2.0 * half,
            2.0 * half,
            12.0 * s,
            fa(p.accent.with_alpha(0.20)),
        );
        let filled: Vec<&mut Place> = inner
            .iter_mut()
            .filter(|pl| pl.item.is_some())
            .take(4)
            .collect();
        if filled.is_empty() {
            icons.icon(c, icon::FOLDER, ix, iy, 20.0 * s, fa(p.accent));
            return;
        }
        // Grille de deux colonnes au plus, centrée dans la pastille
        // Aperçus chargés à leur taille exacte : nets, sans réduction au dessin
        let mini_px = (15.0 * s).round() as u32;
        let mini = mini_px as f32;
        let gap = 3.0 * s;
        let cols = filled.len().min(2) as f32;
        let rows = filled.len().div_ceil(2) as f32;
        let x0 = ix - (cols * mini + (cols - 1.0) * gap) / 2.0;
        let y0 = iy - (rows * mini + (rows - 1.0) * gap) / 2.0;
        for (n, pl) in filled.into_iter().enumerate() {
            let (col, row) = ((n % 2) as f32, (n / 2) as f32);
            let mx = x0 + col * (mini + gap);
            let my = y0 + row * (mini + gap);
            let Some(item) = pl.item.as_ref() else {
                continue;
            };
            match pl
                .mini
                .get_or_insert_with(|| common::icons::load(&item.icon, mini_px))
            {
                // Une icône pas carrée garde ses proportions, centrée
                Some(img) => {
                    let (w, h) = (img.width as f32, img.height as f32);
                    let (x, y) = (mx + (mini - w) / 2.0, my + (mini - h) / 2.0);
                    c.image_alpha(img, x, y, w, h, 0.0, alpha)
                }
                None => c.fill_circle(
                    mx + mini / 2.0,
                    my + mini / 2.0,
                    mini / 2.0,
                    fa(initial_color(&item.name)),
                ),
            }
        }
        return;
    }
    let Some(item) = place.item.as_ref() else {
        c.fill_circle(ix, iy, 18.0 * s, fa(p.ink(0.05)));
        icons.icon(c, icon::PLUS, ix, iy, 15.0 * s, fa(p.ink(0.35)));
        return;
    };
    let img = place
        .icon
        .get_or_insert_with(|| common::icons::load(&item.icon, icon_px));
    match img {
        // Taille exacte, centrée : nette (une icône pas carrée garde ses proportions)
        Some(img) => {
            let (w, h) = (img.width as f32, img.height as f32);
            c.image_alpha(img, ix - w / 2.0, iy - h / 2.0, w, h, 0.0, alpha)
        }
        None => {
            // Sans icône : l'initiale sur une pastille de couleur
            c.fill_circle(ix, iy, half, fa(initial_color(&item.name)));
            let initial: String = item.name.chars().take(1).collect::<String>().to_uppercase();
            bold.draw_centered(
                c,
                &initial,
                ix,
                iy - half,
                2.0 * half,
                18.0 * s,
                fa(Rgba::hex(0x1d1916, 1.0)),
            );
        }
    }
    // Action ou proposition : petit repère dans le coin
    if item.app.is_none() {
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 8.0 * s, fa(p.panel));
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 6.5 * s, fa(p.accent));
    }
    if !place.windows.is_empty() {
        c.fill_circle(ix, iy + half + 7.0 * s, 2.5 * s, fa(p.text_dim));
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
            for place in &mut self.places {
                place.icon = None;
                place.mini = None;
                for inner in place.folder.iter_mut().flat_map(|f| f.1.iter_mut()) {
                    inner.icon = None;
                    inner.mini = None;
                }
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
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // Sans position donnée par le compositeur : là où le pointeur entre
                    if self.anchor.is_none() {
                        self.anchor = Some((x, y));
                    }
                    let t = self.target(x, y);
                    if matches!(t, Target::Slot(_) | Target::Inner(_)) {
                        self.travelled = true;
                    }
                    let hover = (t != Target::Outside).then_some(t);
                    self.set_hover(hover);
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => match self.target(x, y) {
                    Target::Slot(i) => self.activate(i),
                    Target::Inner(j) => self.activate_inner(j),
                    _ => self.begin_close(),
                },
                // Clic du milieu maintenu, glissé vers une place puis relâché
                PointerEventKind::Release {
                    button: BTN_MIDDLE, ..
                } if self.travelled => match self.target(x, y) {
                    // Relâché sur un dossier : il reste ouvert
                    Target::Slot(i) if self.places[i].folder.is_none() => self.activate(i),
                    Target::Inner(j) => self.activate_inner(j),
                    _ => {}
                },
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
