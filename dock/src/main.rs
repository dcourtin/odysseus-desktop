//! Dock : surface layer-shell ancrée en bas de l'écran, au-dessus des fenêtres
//! sans leur réserver de place.
//!
//! - **Favoris** à gauche (~/.config/odysseus/dock, un identifiant d'appli
//!   par ligne), puis, après un séparateur, les **applis ouvertes** qui n'en
//!   sont pas, et la **corbeille**.
//! - Un point sous une appli ouverte ; un anneau quand elle a des fenêtres
//!   réduites, qui « rentrent » dans son icône.
//! - Clic : lancer, ou ramener la fenêtre (une seule), ou choisir parmi ses
//!   fenêtres (plusieurs). Clic du milieu : nouvelle fenêtre. Clic droit :
//!   fenêtres, nouvelle fenêtre, garder ou retirer du Dock, quitter.
//! - Masquage (Réglages > Dock) : intelligent (le Dock s'efface quand une
//!   fenêtre passe dessous), toujours, ou jamais. Pousser la souris contre le
//!   bord du bas le fait revenir.
//!
//! Les fenêtres viennent de l'IPC du compositeur (GET_WINDOWS), relu deux fois
//! par seconde.

use std::{
    os::{fd::AsFd, unix::process::CommandExt},
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    apps::App,
    draw::{Canvas, Image, Text},
    settings::Settings,
    theme::{self, icon},
};
use serde_json::Value;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
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

const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Marges (pixels logiques) : sous le Dock, dans le Dock, entre les icônes
const MARGIN: f32 = 10.0;
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
/// Place au-dessus du Dock pour le nom de l'appli survolée
const TOOLTIP_ROOM: f32 = 44.0;
/// Bande du bas qui fait revenir le Dock masqué
const EDGE: i32 = 4;
const MENU_ROW: f32 = 30.0;
const MENU_W: f32 = 260.0;
const SLIDE: Duration = Duration::from_millis(180);
/// Applis gardées par défaut, si elles existent
const DEFAULT_PINNED: [&str; 5] = [
    "odysseus-files",
    "firefox",
    "foot",
    "odysseus-notes",
    "odysseus-reglages",
];

/// Une fenêtre, d'après le compositeur
#[derive(Clone, Debug, PartialEq)]
struct Win {
    id: u64,
    app_id: String,
    title: String,
    minimized: bool,
    focused: bool,
    workspace: i64,
    rect: (f32, f32, f32, f32),
}

/// Une case du Dock : une appli (connue ou non) et ses fenêtres
struct Item {
    key: String,
    app: Option<App>,
    pinned: bool,
    windows: Vec<Win>,
    icon: Option<Image>,
}

impl Item {
    fn name(&self) -> String {
        self.app.as_ref().map_or_else(
            || {
                self.windows
                    .first()
                    .map(|w| w.app_id.clone())
                    .unwrap_or_else(|| self.key.clone())
            },
            |a| a.name.clone(),
        )
    }
}

/// Entrée d'un menu
#[derive(Clone, PartialEq)]
enum MenuAction {
    Focus(u64),
    New,
    Pin,
    Unpin,
    Quit,
}

struct Menu {
    item: usize,
    rows: Vec<(String, MenuAction, bool)>,
    hover: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Item(usize),
    Trash,
}

struct Dock {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    shm: Shm,
    /// Sorti le temps d'un dessin (le dessin emprunte le Dock)
    pool: Option<SlotPool>,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Dock>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
    icons: Text,
    settings: Settings,
    apps: Vec<App>,
    pinned: Vec<String>,
    items: Vec<Item>,
    windows: Vec<Win>,
    active_ws: i64,
    /// Taille logique de l'écran (pour savoir si une fenêtre passe sous le Dock)
    output: (f32, f32),
    hover: Option<Target>,
    inside: bool,
    menu: Option<Menu>,
    /// Montré (1) ou masqué (0), et animation en cours (départ, valeur de départ)
    shown: f32,
    slide: Option<(Instant, f32, f32)>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
}

fn config_path() -> Option<PathBuf> {
    theme::config_path().map(|p| p.with_file_name("dock"))
}

/// Favoris enregistrés, sinon ceux par défaut qui existent
fn load_pinned(apps: &[App]) -> Vec<String> {
    match config_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
        None => DEFAULT_PINNED
            .iter()
            .filter(|id| apps.iter().any(|a| a.id == **id))
            .map(|id| id.to_string())
            .collect(),
    }
}

fn save_pinned(pinned: &[String]) {
    if let Some(path) = config_path() {
        let text = format!(
            "# Applis gardées dans le Dock (identifiants des fichiers .desktop), dans l'ordre\n{}\n",
            pinned.join("\n")
        );
        let _ = std::fs::write(path, text);
    }
}

/// Appli d'une fenêtre : par identifiant, classe de fenêtre ou commande
fn app_index(app_id: &str, apps: &[App]) -> Option<usize> {
    let key = app_id.to_lowercase();
    if key.is_empty() {
        return None;
    }
    let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_lowercase();
    apps.iter()
        .position(|a| a.id.to_lowercase() == key)
        .or_else(|| apps.iter().position(|a| a.wm_class.to_lowercase() == key))
        .or_else(|| apps.iter().position(|a| last(&a.id) == key))
        .or_else(|| {
            apps.iter().position(|a| {
                a.exec
                    .split_whitespace()
                    .next()
                    .and_then(|c| c.rsplit('/').next())
                    .is_some_and(|c| c.to_lowercase() == key)
            })
        })
}

fn parse_windows(json: &str) -> Vec<Win> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    list.iter()
        .map(|w| {
            let f = |k: &str| w[k].as_f64().unwrap_or(0.0) as f32;
            Win {
                id: w["id"].as_u64().unwrap_or(0),
                app_id: w["app_id"].as_str().unwrap_or_default().to_string(),
                title: w["title"].as_str().unwrap_or_default().to_string(),
                minimized: w["minimized"].as_bool().unwrap_or(false),
                focused: w["focused"].as_bool().unwrap_or(false),
                workspace: w["workspace"].as_i64().unwrap_or(0),
                rect: (f("x"), f("y"), f("width"), f("height")),
            }
        })
        .collect()
}

fn spawn(command: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .process_group(0)
        .spawn();
}

fn launch(app: &App) {
    if app.terminal {
        let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
        spawn(&format!("{term} -e {}", app.exec));
    } else {
        spawn(&app.exec);
    }
}

fn create_layer(
    compositor: &CompositorState,
    layer_shell: &LayerShell,
    qh: &QueueHandle<Dock>,
) -> LayerSurface {
    let surface = compositor.create_surface(qh);
    let layer =
        layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("odysseus-dock"), None);
    layer.set_anchor(Anchor::BOTTOM);
    layer.set_size(400, 100);
    layer.set_exclusive_zone(0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();
    layer
}

fn main() {
    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<Dock> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");
    let layer = create_layer(&compositor, &layer_shell, &qh);

    let mut event_loop: EventLoop<'static, Dock> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    // Thème, réglages ou favoris changés : couleurs, taille, masquage, cases suivent
    if let Some(fd) = theme::watch() {
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, dock| {
                theme::drain(fd.as_fd());
                theme::reload();
                dock.settings = Settings::load();
                // Favoris modifiés à la main (~/.config/odysseus/dock) : suivis aussi
                dock.pinned = load_pinned(&dock.apps);
                dock.rebuild();
                Ok(PostAction::Continue)
            },
        );
    }
    // Applis installées, retirées ou modifiées (entrées .desktop, icônes) : le Dock suit
    let applications = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("applications"));
    if let Some(fd) = applications.and_then(|d| common::watch::existing(&d)) {
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, dock| {
                common::watch::drain(fd.as_fd());
                dock.apps = common::apps::load();
                // Icônes relues (une icône a pu changer sans changer de chemin)
                dock.items.clear();
                dock.rebuild();
                Ok(PostAction::Continue)
            },
        );
    }
    // Fenêtres : relues deux fois par seconde
    event_loop
        .handle()
        .insert_source(Timer::immediate(), |_, _, dock| {
            dock.poll();
            TimeoutAction::ToDuration(Duration::from_millis(500))
        })
        .expect("minuterie");

    let apps = common::apps::load();
    let pinned = load_pinned(&apps);
    let load = |p: &str| Text::load(p).expect("police");
    let mut dock = Dock {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: Some(SlotPool::new(512 * 1024 * 4, &shm).expect("pool shm")),
        compositor,
        shm,
        layer,
        loop_handle: event_loop.handle(),
        pointer: None,
        regular: load(theme::FONT_REGULAR),
        bold: load(theme::FONT_BOLD),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        settings: Settings::load(),
        apps,
        pinned,
        items: Vec::new(),
        windows: Vec::new(),
        active_ws: 1,
        output: (2560.0, 1440.0),
        hover: None,
        inside: false,
        menu: None,
        shown: 1.0,
        slide: None,
        width: 400,
        height: 100,
        scale: 1,
        configured: false,
    };
    dock.read_output();
    dock.rebuild();
    // Garde le gestionnaire de couches vivant pour la durée du programme
    let _layer_shell = layer_shell;
    loop {
        if let Err(e) = event_loop.dispatch(None, &mut dock) {
            eprintln!("odysseus-dock : connexion perdue ({e})");
            break;
        }
    }
}

impl Dock {
    fn icon_size(&self) -> f32 {
        self.settings.dock_size as f32
    }

    fn panel_h(&self) -> f32 {
        self.icon_size() + 2.0 * PAD + 6.0
    }

    /// Largeur du panneau : icônes, séparateur, corbeille
    fn panel_w(&self) -> f32 {
        let n = self.items.len() as f32 + 1.0;
        2.0 * PAD + n * self.icon_size() + (n - 1.0) * GAP + 2.0 * GAP + 1.0
    }

    fn menu_h(&self) -> f32 {
        self.menu
            .as_ref()
            .map_or(0.0, |m| m.rows.len() as f32 * MENU_ROW + 12.0)
    }

    /// Taille voulue de la surface (le menu ouvert l'agrandit vers le haut)
    fn wanted_size(&self) -> (u32, u32) {
        let w = self.panel_w().max(MENU_W + 16.0) + 32.0;
        let h = MARGIN + self.panel_h() + TOOLTIP_ROOM + self.menu_h();
        (w.ceil() as u32, h.ceil() as u32)
    }

    fn read_output(&mut self) {
        let Ok(json) = common::ipc::request(common::ipc::GET_OUTPUTS, "") else {
            return;
        };
        if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&json)
            && let Some(o) = list.first()
        {
            let scale = o["scale"].as_f64().unwrap_or(1.0).max(0.5) as f32;
            let w = o["current_mode"]["width"].as_f64().unwrap_or(0.0) as f32;
            let h = o["current_mode"]["height"].as_f64().unwrap_or(0.0) as f32;
            if w > 0.0 && h > 0.0 {
                self.output = (w / scale, h / scale);
            }
        }
    }

    /// Relit les fenêtres ; reconstruit le Dock s'il a changé
    fn poll(&mut self) {
        let windows = common::ipc::request(common::ipc::GET_WINDOWS, "")
            .map(|j| parse_windows(&j))
            .unwrap_or_default();
        if let Ok(json) = common::ipc::request(common::ipc::GET_WORKSPACES, "")
            && let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&json)
            && let Some(ws) = list.iter().find(|w| w["focused"] == true)
        {
            self.active_ws = ws["num"].as_i64().unwrap_or(1);
        }
        if windows != self.windows {
            self.windows = windows;
            self.rebuild();
        } else {
            self.update_visibility();
        }
    }

    /// Cases du Dock : favoris, puis applis ouvertes
    fn rebuild(&mut self) {
        let mut items: Vec<Item> = Vec::new();
        let old = std::mem::take(&mut self.items);
        let take_icon = |key: &str, app: Option<&App>, size: u32| -> Option<Image> {
            old.iter()
                .find(|i| i.key == key)
                .and_then(|i| {
                    i.icon.as_ref().map(|img| Image {
                        width: img.width,
                        height: img.height,
                        data: img.data.clone(),
                    })
                })
                .or_else(|| app.and_then(|a| common::icons::load(&a.icon, size)))
        };
        let px = (self.icon_size() * self.scale.max(1) as f32) as u32;
        for id in &self.pinned {
            let app = self.apps.iter().find(|a| &a.id == id).cloned();
            if app.is_none() {
                continue;
            }
            let icon = take_icon(id, app.as_ref(), px);
            items.push(Item {
                key: id.clone(),
                app,
                pinned: true,
                windows: Vec::new(),
                icon,
            });
        }
        for w in &self.windows {
            let (key, app) = match app_index(&w.app_id, &self.apps) {
                Some(i) => (self.apps[i].id.clone(), Some(self.apps[i].clone())),
                None => (format!("fenetre:{}", w.app_id), None),
            };
            match items.iter_mut().find(|i| i.key == key) {
                Some(item) => item.windows.push(w.clone()),
                None => {
                    let icon = take_icon(&key, app.as_ref(), px);
                    items.push(Item {
                        key,
                        app,
                        pinned: false,
                        windows: vec![w.clone()],
                        icon,
                    });
                }
            }
        }
        self.items = items;
        if self
            .menu
            .as_ref()
            .is_some_and(|m| m.item >= self.items.len())
        {
            self.menu = None;
        }
        self.resize();
        self.update_visibility();
        self.draw();
    }

    fn resize(&mut self) {
        let (w, h) = self.wanted_size();
        if (w, h) != (self.width, self.height) {
            self.layer.set_size(w, h);
            self.layer.commit();
        }
    }

    /// Rectangle du panneau dans la surface (pixels logiques)
    fn panel_rect(&self) -> (f32, f32, f32, f32) {
        let (pw, ph) = (self.panel_w(), self.panel_h());
        let x = (self.width as f32 - pw) / 2.0;
        let y = self.height as f32 - MARGIN - ph + (1.0 - self.shown) * (ph + MARGIN + 2.0);
        (x, y, pw, ph)
    }

    /// Une fenêtre du bureau affiché passe-t-elle sous le Dock ?
    fn overlapped(&self) -> bool {
        let (pw, ph) = (self.panel_w(), self.panel_h());
        let x0 = (self.output.0 - pw) / 2.0;
        let y0 = self.output.1 - MARGIN - ph;
        self.windows.iter().any(|w| {
            let (x, y, ww, wh) = w.rect;
            !w.minimized
                && w.workspace == self.active_ws
                && x < x0 + pw
                && x + ww > x0
                && y + wh > y0
                && y < y0 + ph
        })
    }

    fn should_show(&self) -> bool {
        if self.inside || self.menu.is_some() {
            return true;
        }
        match self.settings.dock_hide.as_str() {
            "jamais" => true,
            "toujours" => false,
            _ => !self.overlapped(),
        }
    }

    /// Montre ou masque le Dock, en glissant
    fn update_visibility(&mut self) {
        let target = if self.should_show() { 1.0 } else { 0.0 };
        let current_target = self.slide.map(|(_, _, t)| t).unwrap_or(self.shown);
        if (current_target - target).abs() < f32::EPSILON {
            return;
        }
        self.slide = Some((Instant::now(), self.shown, target));
        let _ = self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_millis(16)),
            |_, _, dock| {
                let Some((start, from, to)) = dock.slide else {
                    return TimeoutAction::Drop;
                };
                let t = (start.elapsed().as_secs_f32() / SLIDE.as_secs_f32()).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                dock.shown = from + (to - from) * eased;
                if t >= 1.0 {
                    dock.shown = to;
                    dock.slide = None;
                }
                dock.draw();
                if dock.slide.is_some() {
                    TimeoutAction::ToDuration(Duration::from_millis(16))
                } else {
                    TimeoutAction::Drop
                }
            },
        );
    }

    /// Ce qui est sous le pointeur (pixels logiques de la surface)
    fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        let (px, py, _, ph) = self.panel_rect();
        if y < py || y > py + ph {
            return None;
        }
        let size = self.icon_size();
        let mut cx = px + PAD;
        for i in 0..self.items.len() {
            if x >= cx - GAP / 2.0 && x < cx + size + GAP / 2.0 {
                return Some(Target::Item(i));
            }
            cx += size + GAP;
        }
        cx += 2.0 * GAP + 1.0 - GAP;
        (x >= cx - GAP / 2.0 && x < cx + size + GAP / 2.0).then_some(Target::Trash)
    }

    /// Abscisse du centre d'une case
    fn center_of(&self, target: Target) -> f32 {
        let (px, _, _, _) = self.panel_rect();
        let size = self.icon_size();
        match target {
            Target::Item(i) => px + PAD + i as f32 * (size + GAP) + size / 2.0,
            Target::Trash => {
                px + PAD + self.items.len() as f32 * (size + GAP) + 2.0 * GAP + 1.0 + size / 2.0
            }
        }
    }

    /// Rectangle du menu ouvert (au-dessus de sa case)
    fn menu_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let menu = self.menu.as_ref()?;
        let h = self.menu_h();
        let cx = self.center_of(Target::Item(menu.item));
        let x = (cx - MENU_W / 2.0).clamp(8.0, self.width as f32 - MENU_W - 8.0);
        let (_, py, _, _) = self.panel_rect();
        Some((x, py - 10.0 - h, MENU_W, h))
    }

    fn open_menu(&mut self, i: usize, full: bool) {
        let Some(item) = self.items.get(i) else {
            return;
        };
        let mut rows = Vec::new();
        for w in &item.windows {
            let title = if w.title.is_empty() {
                item.name()
            } else {
                w.title.clone()
            };
            let label = if w.minimized {
                format!("{title}  (réduite)")
            } else {
                title
            };
            rows.push((label, MenuAction::Focus(w.id), false));
        }
        if full {
            if item.app.is_some() {
                rows.push(("Nouvelle fenêtre".into(), MenuAction::New, !rows.is_empty()));
                if item.pinned {
                    rows.push(("Retirer du Dock".into(), MenuAction::Unpin, false));
                } else {
                    rows.push(("Garder dans le Dock".into(), MenuAction::Pin, false));
                }
            }
            if !item.windows.is_empty() {
                rows.push(("Quitter".into(), MenuAction::Quit, true));
            }
        }
        if rows.is_empty() {
            return;
        }
        self.menu = Some(Menu {
            item: i,
            rows,
            hover: None,
        });
        self.resize();
        self.draw();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.resize();
            self.update_visibility();
            self.draw();
        }
    }

    fn menu_action(&mut self, item: usize, action: MenuAction) {
        let Some(it) = self.items.get(item) else {
            return;
        };
        match action {
            MenuAction::Focus(id) => {
                let _ = common::ipc::command(&format!("focus {id}"));
            }
            MenuAction::New => {
                if let Some(app) = it.app.as_ref() {
                    launch(app);
                }
            }
            MenuAction::Pin => {
                self.pinned.push(it.key.clone());
                save_pinned(&self.pinned);
            }
            MenuAction::Unpin => {
                let key = it.key.clone();
                self.pinned.retain(|k| *k != key);
                save_pinned(&self.pinned);
            }
            MenuAction::Quit => {
                for w in &it.windows {
                    let _ = common::ipc::command(&format!("close {}", w.id));
                }
            }
        }
        self.menu = None;
        self.rebuild();
    }

    fn click(&mut self, x: f32, y: f32, button: u32) {
        // Menu ouvert : choisir une ligne, ou le fermer
        if let (Some((mx, my, mw, mh)), Some(menu)) = (self.menu_rect(), self.menu.as_ref()) {
            if x >= mx && x < mx + mw && y >= my && y < my + mh {
                let row = ((y - my - 6.0) / MENU_ROW).floor();
                if row >= 0.0
                    && let Some((_, action, _)) = menu.rows.get(row as usize).cloned()
                {
                    let item = menu.item;
                    self.menu_action(item, action);
                }
                return;
            }
            let same = matches!(self.target_at(x, y), Some(Target::Item(i)) if i == menu.item);
            self.close_menu();
            if same {
                return;
            }
        }
        match self.target_at(x, y) {
            Some(Target::Trash) => {
                if let Some(dir) = common::trash::files_dir() {
                    let files = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("files")))
                        .unwrap_or_else(|| "files".into());
                    let _ = std::process::Command::new(files)
                        .arg(dir)
                        .process_group(0)
                        .spawn();
                }
            }
            Some(Target::Item(i)) => {
                let Some(item) = self.items.get(i) else {
                    return;
                };
                match button {
                    BTN_RIGHT => self.open_menu(i, true),
                    BTN_MIDDLE => {
                        if let Some(app) = item.app.as_ref() {
                            launch(app);
                        }
                    }
                    _ => match item.windows.as_slice() {
                        [] => {
                            if let Some(app) = item.app.as_ref() {
                                launch(app);
                            }
                        }
                        [w] => {
                            let _ = common::ipc::command(&format!("focus {}", w.id));
                        }
                        _ => self.open_menu(i, false),
                    },
                }
            }
            None => {}
        }
    }

    fn set_input_region(&self) {
        let Ok(region) = Region::new(&self.compositor) else {
            return;
        };
        let (px, py, pw, ph) = self.panel_rect();
        if self.shown > 0.5 || self.inside {
            region.add(px as i32, py as i32, pw.ceil() as i32, ph.ceil() as i32);
            if let Some((mx, my, mw, mh)) = self.menu_rect() {
                region.add(mx as i32, my as i32, mw as i32, mh as i32);
            }
        } else {
            // Masqué : une fine bande au bas de l'écran le fait revenir
            region.add(px as i32, self.height as i32 - EDGE, pw.ceil() as i32, EDGE);
        }
        self.layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        if let Some(mut pool) = self.pool.take() {
            self.draw_with(&mut pool);
            self.pool = Some(pool);
        }
    }

    fn draw_with(&mut self, pool: &mut SlotPool) {
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
        let Ok((buffer, data)) =
            pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
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
        let (px, py, pw, ph) = self.panel_rect();
        if self.shown > 0.01 {
            c.fill_rounded(
                px * s,
                py * s,
                pw * s,
                ph * s,
                18.0 * s,
                p.panel.with_alpha(0.92),
            );
            c.stroke_rounded(
                px * s,
                py * s,
                pw * s,
                ph * s,
                18.0 * s,
                s,
                p.panel_border(),
            );
            let size = self.icon_size();
            let iy = py + PAD;
            for (i, item) in self.items.iter().enumerate() {
                let cx = self.center_of(Target::Item(i));
                let x = cx - size / 2.0;
                let hovered = self.hover == Some(Target::Item(i));
                if hovered {
                    c.fill_rounded(
                        (x - 4.0) * s,
                        (iy - 4.0) * s,
                        (size + 8.0) * s,
                        (size + 8.0) * s,
                        12.0 * s,
                        p.ink(0.08),
                    );
                }
                match item.icon.as_ref() {
                    Some(img) => c.image(img, x * s, iy * s, size * s, size * s, 0.0),
                    None => {
                        // Appli sans icône : l'initiale dans une pastille
                        c.fill_rounded(x * s, iy * s, size * s, size * s, 12.0 * s, p.subtle);
                        let initial: String = item
                            .name()
                            .chars()
                            .take(1)
                            .collect::<String>()
                            .to_uppercase();
                        self.bold.draw_centered(
                            &mut c,
                            &initial,
                            cx * s,
                            iy * s,
                            size * s,
                            size * 0.45 * s,
                            p.text,
                        );
                    }
                }
                // Ouverte : un point ; des fenêtres réduites : un anneau
                let dy = (py + ph - 5.0) * s;
                if item.windows.iter().any(|w| w.minimized) {
                    c.stroke_rounded(
                        cx * s - 3.0 * s,
                        dy - 3.0 * s,
                        6.0 * s,
                        6.0 * s,
                        3.0 * s,
                        1.2 * s,
                        p.text_soft(),
                    );
                } else if !item.windows.is_empty() {
                    c.fill_circle(cx * s, dy, 2.5 * s, p.text_soft());
                }
            }
            // Séparateur, puis la corbeille
            let sx = self.center_of(Target::Trash) - size / 2.0 - GAP - 1.0;
            c.fill_rounded(
                sx * s,
                (iy + 6.0) * s,
                s,
                (size - 12.0) * s,
                0.0,
                p.ink(0.15),
            );
            let tx = self.center_of(Target::Trash);
            if self.hover == Some(Target::Trash) {
                c.fill_rounded(
                    (tx - size / 2.0 - 4.0) * s,
                    (iy - 4.0) * s,
                    (size + 8.0) * s,
                    (size + 8.0) * s,
                    12.0 * s,
                    p.ink(0.08),
                );
            }
            c.fill_rounded(
                (tx - size / 2.0) * s,
                iy * s,
                size * s,
                size * s,
                12.0 * s,
                p.ink(0.08),
            );
            self.icons.icon(
                &mut c,
                icon::TRASH,
                tx * s,
                (iy + size / 2.0) * s,
                size * 0.45 * s,
                p.text_soft(),
            );

            // Nom de la case survolée, au-dessus
            if self.menu.is_none()
                && let Some(target) = self.hover
            {
                let name = match target {
                    Target::Item(i) => self.items.get(i).map(Item::name).unwrap_or_default(),
                    Target::Trash => "Corbeille".into(),
                };
                let fpx = 12.5;
                let tw = self.regular.measure(&name, fpx * s) / s + 20.0;
                let cx = self.center_of(target);
                let x = (cx - tw / 2.0).clamp(4.0, self.width as f32 - tw - 4.0);
                let y = py - 36.0;
                c.fill_rounded(
                    x * s,
                    y * s,
                    tw * s,
                    26.0 * s,
                    13.0 * s,
                    p.panel.with_alpha(0.95),
                );
                c.stroke_rounded(
                    x * s,
                    y * s,
                    tw * s,
                    26.0 * s,
                    13.0 * s,
                    s,
                    p.panel_border(),
                );
                self.regular.draw_centered(
                    &mut c,
                    &name,
                    (x + tw / 2.0) * s,
                    y * s,
                    26.0 * s,
                    fpx * s,
                    p.text,
                );
            }
        }

        // Menu
        if let (Some((mx, my, mw, mh)), Some(menu)) = (self.menu_rect(), self.menu.as_ref()) {
            c.fill_rounded(
                mx * s,
                my * s,
                mw * s,
                mh * s,
                10.0 * s,
                p.panel.with_alpha(0.97),
            );
            c.stroke_rounded(
                mx * s,
                my * s,
                mw * s,
                mh * s,
                10.0 * s,
                s,
                p.panel_border(),
            );
            for (k, (label, _, separated)) in menu.rows.iter().enumerate() {
                let ry = my + 6.0 + k as f32 * MENU_ROW;
                if *separated {
                    c.fill_rounded(
                        (mx + 10.0) * s,
                        (ry - 0.5) * s,
                        (mw - 20.0) * s,
                        s,
                        0.0,
                        p.ink(0.10),
                    );
                }
                if menu.hover == Some(k) {
                    c.fill_rounded(
                        (mx + 4.0) * s,
                        ry * s,
                        (mw - 8.0) * s,
                        MENU_ROW * s,
                        6.0 * s,
                        p.accent.with_alpha(0.85),
                    );
                }
                let color = if menu.hover == Some(k) {
                    p.on_accent
                } else {
                    p.text
                };
                let text = self.regular.truncate(label, 13.0 * s, (mw - 28.0) * s);
                self.regular.draw_in(
                    &mut c,
                    &text,
                    (mx + 14.0) * s,
                    ry * s,
                    MENU_ROW * s,
                    13.0 * s,
                    color,
                );
            }
        }

        self.set_input_region();
        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

impl CompositorHandler for Dock {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            // Icônes rechargées à la nouvelle échelle
            self.items.clear();
            self.rebuild();
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

impl OutputHandler for Dock {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.read_output();
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.read_output();
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Dock {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        std::process::exit(0);
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
        if w > 0 && h > 0 {
            self.width = w;
            self.height = h;
        }
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for Dock {
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
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Dock {
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
                PointerEventKind::Enter { .. } => {
                    self.inside = true;
                    self.update_visibility();
                    self.hover = self.target_at(x, y);
                    self.draw();
                }
                PointerEventKind::Leave { .. } => {
                    self.inside = false;
                    self.hover = None;
                    self.menu = None;
                    self.resize();
                    // Un instant avant de se masquer : le temps de revenir
                    let _ = self.loop_handle.insert_source(
                        Timer::from_duration(Duration::from_millis(400)),
                        |_, _, dock| {
                            dock.update_visibility();
                            TimeoutAction::Drop
                        },
                    );
                    self.draw();
                }
                PointerEventKind::Motion { .. } => {
                    let hover = self.target_at(x, y);
                    let menu_hover = self.menu_rect().and_then(|(mx, my, mw, mh)| {
                        (x >= mx && x < mx + mw && y >= my + 6.0 && y < my + mh - 6.0)
                            .then(|| ((y - my - 6.0) / MENU_ROW).floor() as usize)
                    });
                    let changed = hover != self.hover
                        || self.menu.as_ref().is_some_and(|m| m.hover != menu_hover);
                    self.hover = hover;
                    if let Some(m) = self.menu.as_mut() {
                        m.hover = menu_hover;
                    }
                    if changed {
                        self.draw();
                    }
                }
                PointerEventKind::Press { button, .. } => self.click(x, y, button),
                _ => {}
            }
        }
    }
}

impl ShmHandler for Dock {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Dock {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Dock);
delegate_output!(Dock);
delegate_shm!(Dock);
delegate_seat!(Dock);
delegate_pointer!(Dock);
delegate_layer!(Dock);
delegate_registry!(Dock);
