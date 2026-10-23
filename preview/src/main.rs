//! Aperçu : visionneuse d'images d'odysseus-desktop.
//!
//! `apercu IMAGE` ouvre l'image et permet de parcourir celles du même dossier.
//! Clavier : ← → (précédente / suivante), Espace ou Échap (fermer), F (plein
//! écran), 0 (ajuster), 1 (taille réelle), + / − (zoom), R / Maj+R (pivoter),
//! C (recadrer, puis Entrée), I (informations), Suppr (corbeille),
//! Ctrl+S (enregistrer), Ctrl+Maj+S (enregistrer une copie), Ctrl+Z (annuler
//! la dernière mise à la corbeille).
//! Souris : molette (zoom vers le pointeur), glisser (se déplacer dans l'image
//! zoomée), double-clic (ajuster / taille réelle), glisser la barre du haut
//! (déplacer la fenêtre).

mod picture;
mod render;

use std::os::fd::AsFd;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba, icon},
};
use picture::{Picture, is_image};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction,
            channel::{self, Event as ChannelEvent},
            generic::Generic,
            timer::{TimeoutAction, Timer},
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
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

const TOOLBAR_H: f32 = 52.0;
const INFO_W: f32 = 300.0;
const BTN_LEFT: u32 = 0x110;
/// Barre d'outils visible ce temps après le dernier mouvement
const TOOLBAR_IDLE: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Prev,
    Next,
    RotateLeft,
    RotateRight,
    Crop,
    Info,
    Trash,
    Save,
}

/// Image demandée au fil de décodage, ou reçue
enum Loaded {
    Ok(Box<Picture>),
    Err(PathBuf, String),
}

struct Apercu {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    /// Retiré le temps d'un dessin (le tampon en dépend)
    pool: Option<SlotPool>,
    window: Window,
    loop_handle: LoopHandle<'static, Apercu>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    icons: Text,
    /// Images du dossier, et position de l'image courante
    list: Vec<PathBuf>,
    index: usize,
    current: Option<Picture>,
    error: Option<String>,
    /// Images voisines déjà décodées (navigation instantanée)
    prefetched: Vec<Picture>,
    loader: mpsc::Sender<PathBuf>,
    loading: Option<PathBuf>,
    /// Zoom : None = ajusté à la fenêtre ; sinon pixels du tampon par pixel d'image
    zoom: Option<f64>,
    /// Point de l'image au centre de la vue (zoom libre)
    center: (f64, f64),
    pointer_pos: (f64, f64),
    drag: Option<Drag>,
    last_click: Option<Instant>,
    toolbar_until: Instant,
    buttons: Vec<(f32, f32, f32, f32, Button)>,
    info: bool,
    /// Recadrage : rectangle en pixels d'image, en cours de tracé ou tracé
    cropping: bool,
    crop: Option<(f64, f64, f64, f64)>,
    message: Option<(String, Instant)>,
    /// Dernière image mise à la corbeille (Ctrl+Z la remet)
    last_trashed: Option<PathBuf>,
    fullscreen: bool,
    /// Boutons de la fenêtre (fermer, réduire, agrandir)
    chrome: common::chrome::Chrome,
    qh: QueueHandle<Apercu>,
    /// Image en attente d'affichage, et dessin demandé entre-temps
    frame_pending: bool,
    dirty: bool,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

#[derive(Clone, Copy)]
enum Drag {
    /// Déplacement de l'image : position du pointeur et centre au départ
    Pan((f64, f64), (f64, f64)),
    /// Tracé du recadrage : coin de départ en pixels d'image
    Crop((f64, f64)),
}

/// Images du dossier de `path`, par nom (sans tenir compte de la casse)
fn folder_images(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut list: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_image(p) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    list.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    list
}

fn main() {
    // Sans argument (lancé depuis le lanceur) : la première image de ~/Images
    let start = match std::env::args().nth(1) {
        Some(arg) => std::path::absolute(PathBuf::from(arg)).unwrap_or_default(),
        None => {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
            let dir = ["Images", "Pictures", "Téléchargements", "Downloads"]
                .iter()
                .map(|d| home.join(d))
                .find(|d| d.is_dir())
                .unwrap_or(home);
            // Une image fictive dans le dossier suffit à en lister le contenu
            folder_images(&dir.join("_"))
                .into_iter()
                .next()
                .unwrap_or_else(|| dir.join("_"))
        }
    };
    let mut list = folder_images(&start);
    if !list.contains(&start) {
        list.insert(0, start.clone());
    }
    let index = list.iter().position(|p| *p == start).unwrap_or(0);

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_app_id("odysseus-apercu");
    window.set_title(file_name(&start));
    window.set_min_size(Some((480, 320)));
    window.commit();

    let mut event_loop: EventLoop<'static, Apercu> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    // Décodage dans un fil séparé : l'interface ne se fige jamais
    let (loader, requests) = mpsc::channel::<PathBuf>();
    let (results_tx, results) = channel::channel::<Loaded>();
    std::thread::spawn(move || {
        while let Ok(path) = requests.recv() {
            let result = match Picture::load(&path) {
                Ok(p) => Loaded::Ok(Box::new(p)),
                Err(e) => Loaded::Err(path, e),
            };
            if results_tx.send(result).is_err() {
                break;
            }
        }
    });
    event_loop
        .handle()
        .insert_source(results, |event, _, app| {
            if let ChannelEvent::Msg(loaded) = event {
                app.on_loaded(loaded);
            }
        })
        .expect("canal de décodage");

    // Thème changé (centre de contrôle) : redessin avec la nouvelle palette
    if let Some(fd) = theme::watch() {
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, app| {
                theme::drain(fd.as_fd());
                if theme::reload() {
                    app.draw();
                }
                Ok(PostAction::Continue)
            },
        );
    }

    // Masquage de la barre d'outils et des messages
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_millis(500)),
            |_, _, app| {
                app.tick();
                TimeoutAction::ToDuration(Duration::from_millis(500))
            },
        )
        .expect("minuterie");

    let mut app = Apercu {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: Some(SlotPool::new(2048 * 1024 * 4, &shm).expect("pool shm")),
        shm,
        window,
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        seat: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        list,
        index,
        current: None,
        error: None,
        prefetched: Vec::new(),
        loader,
        loading: None,
        zoom: None,
        center: (0.0, 0.0),
        pointer_pos: (0.0, 0.0),
        drag: None,
        last_click: None,
        toolbar_until: Instant::now() + TOOLBAR_IDLE,
        buttons: Vec::new(),
        info: false,
        cropping: false,
        crop: None,
        message: None,
        last_trashed: None,
        fullscreen: false,
        chrome: common::chrome::Chrome::default(),
        qh: qh.clone(),
        frame_pending: false,
        dirty: false,
        width: 1200,
        height: 800,
        scale: 1,
        configured: false,
        exit: false,
    };
    app.request(start);

    while !app.exit {
        if let Err(e) = event_loop.dispatch(None, &mut app) {
            eprintln!("apercu : connexion au compositeur perdue ({e})");
            break;
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Taille lisible : 3,4 Mo
fn human_size(bytes: u64) -> String {
    let units = ["octets", "Ko", "Mo", "Go"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} octets")
    } else {
        format!("{:.1} {}", v, units[u]).replace('.', ",")
    }
}

impl Apercu {
    fn current_path(&self) -> Option<&PathBuf> {
        self.list.get(self.index)
    }

    /// Demande le décodage d'une image (déjà prête si elle a été préchargée).
    fn request(&mut self, path: PathBuf) {
        if let Some(pos) = self.prefetched.iter().position(|p| p.path == path) {
            let picture = self.prefetched.remove(pos);
            self.show(picture);
            return;
        }
        self.loading = Some(path.clone());
        let _ = self.loader.send(path);
    }

    fn on_loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Ok(picture) => {
                if Some(&picture.path) == self.current_path()
                    && self.current.as_ref().is_none_or(|c| c.path != picture.path)
                {
                    self.loading = None;
                    self.show(*picture);
                } else {
                    // Image voisine préchargée
                    self.prefetched.push(*picture);
                    if self.prefetched.len() > 2 {
                        self.prefetched.remove(0);
                    }
                }
            }
            Loaded::Err(path, e) => {
                if Some(&path) == self.current_path() {
                    self.loading = None;
                    self.current = None;
                    self.error = Some(format!(
                        "Impossible d'ouvrir « {} » : {e}",
                        file_name(&path)
                    ));
                    self.draw();
                }
            }
        }
    }

    fn show(&mut self, picture: Picture) {
        self.window.set_title(file_name(&picture.path));
        self.current = Some(picture);
        self.error = None;
        self.zoom = None;
        self.crop = None;
        self.cropping = false;
        self.draw();
        // Précharge la suivante (le sens de lecture habituel)
        if self.list.len() > 1 {
            let next = self.list[(self.index + 1) % self.list.len()].clone();
            if !self.prefetched.iter().any(|p| p.path == next) {
                let _ = self.loader.send(next);
            }
        }
    }

    fn go(&mut self, delta: i32) {
        if self.list.len() < 2 {
            return;
        }
        if self.current.as_ref().is_some_and(|c| c.edited) {
            self.notify("Image modifiée : Ctrl+S pour enregistrer, ou recommencez pour abandonner");
            if let Some(c) = self.current.as_mut() {
                c.edited = false;
            }
            return;
        }
        let n = self.list.len() as i32;
        self.index = ((self.index as i32 + delta).rem_euclid(n)) as usize;
        // Garde l'image quittée pour un retour instantané
        if let Some(old) = self.current.take() {
            self.prefetched.push(old);
            if self.prefetched.len() > 2 {
                self.prefetched.remove(0);
            }
        }
        let path = self.list[self.index].clone();
        self.request(path);
        self.draw();
    }

    fn notify(&mut self, text: &str) {
        self.message = Some((text.to_string(), Instant::now()));
        self.draw();
    }

    fn tick(&mut self) {
        let expired = self
            .message
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_millis(3000));
        let hide = self.toolbar_until <= Instant::now()
            && self.toolbar_until + Duration::from_millis(600) > Instant::now();
        if expired {
            self.message = None;
        }
        if expired || hide {
            self.draw();
        }
    }

    fn toolbar_visible(&self) -> bool {
        self.toolbar_until > Instant::now()
            || self.cropping
            || self.info
            || self.current.is_none()
            || self.pointer_pos.1 < TOOLBAR_H as f64 * 1.5
    }

    /// Zone d'affichage de l'image, en pixels logiques : sous la barre d'outils
    /// (sauf en plein écran) et à gauche du panneau d'infos.
    fn view(&self) -> (f64, f64) {
        let w = self.width as f64 - if self.info { INFO_W as f64 } else { 0.0 };
        (w.max(1.0), (self.height as f64 - self.view_top()).max(1.0))
    }

    /// Haut de la zone d'affichage, en pixels logiques
    fn view_top(&self) -> f64 {
        if self.fullscreen {
            0.0
        } else {
            TOOLBAR_H as f64
        }
    }

    /// Échelle d'affichage (pixels du tampon par pixel d'image)
    fn display_scale(&self) -> f64 {
        let Some(pic) = self.current.as_ref() else {
            return 1.0;
        };
        match self.zoom {
            Some(z) => z,
            None => {
                let s = self.scale.max(1) as f64;
                let (vw, vh) = self.view();
                // Ajustée, sans agrandir une petite image au-delà de sa taille réelle
                let fit = ((vw * s - 48.0 * s) / pic.width() as f64)
                    .min((vh * s - 48.0 * s) / pic.height() as f64);
                fit.clamp(0.01, 1.0)
            }
        }
    }

    /// Coin haut-gauche de l'image, en pixels du tampon
    fn origin(&self) -> (f64, f64) {
        let Some(pic) = self.current.as_ref() else {
            return (0.0, 0.0);
        };
        let s = self.scale.max(1) as f64;
        let (vw, vh) = self.view();
        let k = self.display_scale();
        let (iw, ih) = (pic.width() as f64 * k, pic.height() as f64 * k);
        let top = self.view_top() * s;
        if self.zoom.is_none() {
            return ((vw * s - iw) / 2.0, top + (vh * s - ih) / 2.0);
        }
        let (cx, cy) = self.center;
        let mut ox = vw * s / 2.0 - cx * k;
        let mut oy = vh * s / 2.0 - cy * k;
        // Image plus petite que la vue : centrée ; sinon bornée aux bords
        ox = if iw <= vw * s {
            (vw * s - iw) / 2.0
        } else {
            ox.clamp(vw * s - iw, 0.0)
        };
        oy = if ih <= vh * s {
            (vh * s - ih) / 2.0
        } else {
            oy.clamp(vh * s - ih, 0.0)
        };
        (ox, top + oy)
    }

    /// Point de l'image sous un point logique de la fenêtre
    fn to_image(&self, x: f64, y: f64) -> (f64, f64) {
        let s = self.scale.max(1) as f64;
        let (ox, oy) = self.origin();
        let k = self.display_scale();
        ((x * s - ox) / k, (y * s - oy) / k)
    }

    /// Zoom vers un point de la fenêtre (le point sous le pointeur reste fixe).
    fn zoom_at(&mut self, factor: f64, at: (f64, f64)) {
        if self.current.is_none() {
            return;
        }
        let s = self.scale.max(1) as f64;
        let before = self.to_image(at.0, at.1);
        let k = (self.display_scale() * factor).clamp(0.02, 32.0);
        self.zoom = Some(k);
        let (vw, vh) = self.view();
        // Centre tel que `before` reste sous le pointeur
        let top = self.view_top();
        self.center = (
            before.0 - (at.0 * s - vw * s / 2.0) / k,
            before.1 - ((at.1 - top) * s - vh * s / 2.0) / k,
        );
        self.draw();
    }

    fn set_zoom(&mut self, zoom: Option<f64>) {
        if let (Some(_), Some(pic)) = (zoom, self.current.as_ref()) {
            self.center = (pic.width() as f64 / 2.0, pic.height() as f64 / 2.0);
        }
        self.zoom = zoom;
        self.draw();
    }

    fn rotate(&mut self, clockwise: bool) {
        if let Some(pic) = self.current.as_mut() {
            pic.rotate(clockwise);
            self.zoom = None;
            self.crop = None;
            self.draw();
        }
    }

    fn apply_crop(&mut self) {
        let Some((x0, y0, x1, y1)) = self.crop else {
            self.cropping = false;
            self.draw();
            return;
        };
        if let Some(pic) = self.current.as_mut() {
            let (l, t) = (x0.min(x1).max(0.0), y0.min(y1).max(0.0));
            let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());
            if w >= 2.0 && h >= 2.0 {
                pic.crop(l as u32, t as u32, w as u32, h as u32);
            }
        }
        self.crop = None;
        self.cropping = false;
        self.zoom = None;
        self.draw();
    }

    fn save(&mut self, copy: bool) {
        let Some(pic) = self.current.as_mut() else {
            return;
        };
        let target = copy.then(|| picture::copy_name(&pic.path));
        match pic.save(target.as_deref()) {
            Ok(path) => {
                if copy {
                    // La copie rejoint la liste du dossier
                    self.list = folder_images(&path);
                    self.index = self.list.iter().position(|p| *p == path).unwrap_or(0);
                    if let Some(p) = self.current.as_mut() {
                        p.path = path.clone();
                        p.edited = false;
                    }
                }
                self.notify(&format!("Enregistrée : {}", file_name(&path)));
            }
            Err(e) => self.notify(&format!("Enregistrement impossible : {e}")),
        }
    }

    fn trash(&mut self) {
        let Some(path) = self.current_path().cloned() else {
            return;
        };
        match common::trash::trash(&path) {
            Ok(()) => {
                self.last_trashed = Some(path.clone());
                self.list.remove(self.index);
                self.current = None;
                self.prefetched.retain(|p| p.path != path);
                if self.list.is_empty() {
                    self.exit = true;
                    return;
                }
                self.index = self.index.min(self.list.len() - 1);
                let next = self.list[self.index].clone();
                self.request(next);
                self.notify(&format!(
                    "« {} » mise à la corbeille — Ctrl+Z pour annuler",
                    file_name(&path)
                ));
            }
            Err(e) => self.notify(&e),
        }
    }

    fn undo_trash(&mut self) {
        let Some(original) = self.last_trashed.take() else {
            return;
        };
        let item = common::trash::list()
            .into_iter()
            .find(|i| i.original == original);
        match item.map(|i| common::trash::restore(&i)) {
            Some(Ok(())) => {
                self.list = folder_images(&original);
                self.index = self.list.iter().position(|p| *p == original).unwrap_or(0);
                self.request(original.clone());
                self.notify(&format!("« {} » remise en place", file_name(&original)));
            }
            Some(Err(e)) => self.notify(&e),
            None => self.notify("Introuvable dans la corbeille"),
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if self.fullscreen {
            self.window.set_fullscreen(None);
        } else {
            self.window.unset_fullscreen();
        }
    }

    fn key(&mut self, event: KeyEvent) {
        self.toolbar_until = Instant::now() + TOOLBAR_IDLE;
        let ctrl = self.modifiers.ctrl;
        let shift = self.modifiers.shift;
        match event.keysym {
            Keysym::Escape => {
                if self.cropping {
                    self.cropping = false;
                    self.crop = None;
                    self.draw();
                } else if self.fullscreen {
                    self.toggle_fullscreen();
                } else {
                    self.exit = true;
                }
            }
            Keysym::space => self.exit = true,
            Keysym::Return | Keysym::KP_Enter if self.cropping => self.apply_crop(),
            Keysym::Left | Keysym::Page_Up => self.go(-1),
            Keysym::Right | Keysym::Page_Down => self.go(1),
            Keysym::Home => self.go(-(self.index as i32)),
            Keysym::End => self.go(self.list.len() as i32 - 1 - self.index as i32),
            Keysym::Delete | Keysym::KP_Delete => self.trash(),
            Keysym::F11 => self.toggle_fullscreen(),
            // Avec Ctrl, le texte produit est un caractère de contrôle : on lit la touche
            Keysym::s | Keysym::S if ctrl => self.save(shift),
            Keysym::z | Keysym::Z if ctrl => self.undo_trash(),
            Keysym::q | Keysym::Q | Keysym::w | Keysym::W if ctrl => self.exit = true,
            _ => {
                let text = event.utf8.unwrap_or_default();
                match (ctrl, text.to_lowercase().as_str()) {
                    (false, "f") => self.toggle_fullscreen(),
                    (false, "0") | (false, "à") => self.set_zoom(None),
                    (false, "1") | (false, "&") => {
                        self.set_zoom(Some(1.0));
                    }
                    (false, "+") | (false, "=") => {
                        let (vw, vh) = self.view();
                        self.zoom_at(1.25, (vw / 2.0, self.view_top() + vh / 2.0));
                    }
                    (false, "-") | (false, ")") => {
                        let (vw, vh) = self.view();
                        self.zoom_at(0.8, (vw / 2.0, self.view_top() + vh / 2.0));
                    }
                    (false, "r") => self.rotate(!shift),
                    (false, "c") => {
                        self.cropping = !self.cropping;
                        self.crop = None;
                        self.draw();
                    }
                    (false, "i") => {
                        self.info = !self.info;
                        self.draw();
                    }
                    _ => {}
                }
            }
        }
    }

    fn button_at(&self, x: f64, y: f64) -> Option<Button> {
        let (x, y) = (x as f32, y as f32);
        self.buttons
            .iter()
            .find(|(bx, by, bw, bh, _)| x >= *bx && x < bx + bw && y >= *by && y < by + bh)
            .map(|b| b.4)
    }

    fn press_button(&mut self, button: Button) {
        match button {
            Button::Prev => self.go(-1),
            Button::Next => self.go(1),
            Button::RotateLeft => self.rotate(false),
            Button::RotateRight => self.rotate(true),
            Button::Crop => {
                if self.cropping && self.crop.is_some() {
                    self.apply_crop();
                } else {
                    self.cropping = !self.cropping;
                    self.crop = None;
                    self.draw();
                }
            }
            Button::Info => {
                self.info = !self.info;
                self.draw();
            }
            Button::Trash => self.trash(),
            Button::Save => self.save(false),
        }
    }

    // --- Dessin ---------------------------------------------------------------

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
        if !self.configured {
            return;
        }
        self.dirty = false;
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
        let Some(mut pool) = self.pool.take() else {
            return;
        };
        let Ok((buffer, data)) =
            pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            self.pool = Some(pool);
            return;
        };
        let mut c = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        c.fill(theme::palette().background);
        let (vw, vh) = self.view();

        // Image
        if let Some(pic) = self.current.as_ref() {
            let (ox, oy) = self.origin();
            let k = self.display_scale();
            let top = self.view_top() * s as f64;
            let clip = render::Clip {
                x0: 0,
                y0: top as i32,
                x1: (vw * s as f64) as i32,
                y1: (top + vh * s as f64) as i32,
            };
            render::draw(c.buf, w, pic, k, ox, oy, clip);

            // Recadrage : l'extérieur du rectangle est assombri
            if self.cropping
                && let Some((x0, y0, x1, y1)) = self.crop
            {
                let (l, r) = (x0.min(x1), x0.max(x1));
                let (t, b) = (y0.min(y1), y0.max(y1));
                let px = |v: f64| (v * k) as f32;
                let (rl, rt) = (ox as f32 + px(l), oy as f32 + px(t));
                let (rr, rb) = (ox as f32 + px(r), oy as f32 + px(b));
                let dim = Rgba::hex(0x000000, 0.55);
                let vwf = vw as f32 * s;
                let topf = top as f32;
                let bottom = topf + vh as f32 * s;
                c.fill_rounded(0.0, topf, vwf, (rt - topf).max(0.0), 0.0, dim);
                c.fill_rounded(0.0, rb, vwf, (bottom - rb).max(0.0), 0.0, dim);
                c.fill_rounded(0.0, rt, rl.max(0.0), rb - rt, 0.0, dim);
                c.fill_rounded(rr, rt, (vwf - rr).max(0.0), rb - rt, 0.0, dim);
                c.stroke_rounded(
                    rl,
                    rt,
                    rr - rl,
                    rb - rt,
                    0.0,
                    2.0 * s,
                    Rgba::hex(0xffffff, 0.9),
                );
                let label = format!("{} × {}", (r - l) as u32, (b - t) as u32);
                // Dimensions sur une pastille sombre, lisibles sur toute image
                let lw = self.regular.measure(&label, theme::FONT_SMALL * s) + 16.0 * s;
                c.fill_rounded(
                    rl + 6.0 * s,
                    rb - 28.0 * s,
                    lw,
                    22.0 * s,
                    11.0 * s,
                    Rgba::hex(0x000000, 0.6),
                );
                self.regular.draw_in(
                    &mut c,
                    &label,
                    rl + 8.0 * s,
                    rb - 26.0 * s,
                    20.0 * s,
                    theme::FONT_SMALL * s,
                    theme::palette().text,
                );
            }
        } else {
            let text = match (&self.error, &self.loading) {
                (Some(e), _) => e.clone(),
                (None, Some(_)) => "Ouverture…".to_string(),
                _ => "Aucune image".to_string(),
            };
            self.regular.draw_centered(
                &mut c,
                &text,
                vw as f32 * s / 2.0,
                self.view_top() as f32 * s,
                vh as f32 * s,
                theme::FONT_SIZE * s,
                theme::palette().text_dim,
            );
        }

        // Panneau d'informations
        if self.info {
            self.draw_info(&mut c, s);
        }
        // Barre d'outils
        self.buttons.clear();
        if self.toolbar_visible() {
            self.draw_toolbar(&mut c, s, vw as f32);
        }
        // Message (pastille en bas)
        if let Some((text, _)) = self.message.as_ref() {
            let tw = self.regular.measure(text, 13.0 * s) + 32.0 * s;
            let (mx, my) = ((vw as f32 * s - tw) / 2.0, h as f32 - 64.0 * s);
            c.fill_rounded(mx, my, tw, 36.0 * s, 18.0 * s, theme::palette().panel);
            self.regular.draw_in(
                &mut c,
                text,
                mx + 16.0 * s,
                my,
                36.0 * s,
                13.0 * s,
                theme::palette().text,
            );
        }

        let surface = self.window.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(&self.qh, surface.clone());
        self.frame_pending = true;
        if buffer.attach_to(surface).is_ok() {
            self.window.commit();
        }
        self.pool = Some(pool);
    }

    /// Boutons de la fenêtre : fermer, réduire, agrandir (Alt : plein écran)
    fn chrome_action(&mut self, button: common::chrome::Button) {
        use common::chrome::Button as B;
        match button {
            B::Close => self.exit = true,
            B::Minimize => self.window.set_minimized(),
            B::Maximize if self.modifiers.alt || self.chrome.fullscreen => {
                if self.chrome.fullscreen {
                    self.window.unset_fullscreen();
                } else {
                    self.window.set_fullscreen(None);
                }
            }
            B::Maximize if self.chrome.maximized => self.window.unset_maximized(),
            B::Maximize => self.window.set_maximized(),
        }
    }

    fn draw_toolbar(&mut self, c: &mut Canvas, s: f32, vw: f32) {
        let bar = theme::palette().surface;
        c.fill_rounded(0.0, 0.0, vw * s, TOOLBAR_H * s, 0.0, bar);
        c.fill_rounded(
            0.0,
            TOOLBAR_H * s - s,
            vw * s,
            s,
            0.0,
            theme::palette().panel_border(),
        );
        let cy = TOOLBAR_H / 2.0;
        self.chrome.draw(c, cy, s);

        // Gauche, après les boutons de la fenêtre : précédente / suivante, nom, position
        let mut x = common::chrome::WIDTH;
        for (glyph, button) in [
            (icon::CHEVRON_LEFT, Button::Prev),
            (icon::CHEVRON_RIGHT, Button::Next),
        ] {
            let enabled = self.list.len() > 1;
            let color = if enabled {
                theme::palette().text
            } else {
                theme::palette().text_dim
            };
            self.icons
                .icon(c, glyph, (x + 14.0) * s, cy * s, 17.0 * s, color);
            self.buttons.push((x, cy - 16.0, 28.0, 32.0, button));
            x += 32.0;
        }
        x += 8.0;
        if let Some(path) = self.current_path() {
            let mut name = file_name(path);
            if self.current.as_ref().is_some_and(|p| p.edited) {
                name.push_str(" — modifiée");
            }
            let name = self
                .bold
                .truncate(&name, 14.0 * s, (vw - 420.0).max(80.0) * s);
            self.bold.draw_in(
                c,
                &name,
                x * s,
                0.0,
                TOOLBAR_H * s,
                14.0 * s,
                theme::palette().text,
            );
            x += self.bold.measure(&name, 14.0 * s) / s + 12.0;
            if self.list.len() > 1 {
                let pos = format!("{} / {}", self.index + 1, self.list.len());
                self.regular.draw_in(
                    c,
                    &pos,
                    x * s,
                    0.0,
                    TOOLBAR_H * s,
                    theme::FONT_SMALL * s,
                    theme::palette().text_dim,
                );
            }
        }

        // Droite : zoom, outils
        let mut rx = vw - 14.0;
        let mut tools = vec![
            (icon::INFO, Button::Info, self.info),
            (icon::TRASH, Button::Trash, false),
            (icon::CROP, Button::Crop, self.cropping),
            (icon::ROTATE_CW, Button::RotateRight, false),
            (icon::ROTATE_CCW, Button::RotateLeft, false),
        ];
        if self.current.as_ref().is_some_and(|p| p.edited) {
            tools.insert(0, (icon::SAVE, Button::Save, false));
        }
        for (glyph, button, active) in tools {
            rx -= 34.0;
            if active {
                c.fill_rounded(
                    rx * s,
                    (cy - 15.0) * s,
                    30.0 * s,
                    30.0 * s,
                    8.0 * s,
                    theme::palette().tile_hover(),
                );
            }
            let color = if active {
                theme::palette().accent
            } else {
                theme::palette().text
            };
            self.icons
                .icon(c, glyph, (rx + 15.0) * s, cy * s, 17.0 * s, color);
            self.buttons.push((rx, cy - 16.0, 30.0, 32.0, button));
        }
        if self.current.is_some() {
            let pct = format!("{} %", (self.display_scale() * 100.0).round());
            let pw = self.regular.measure(&pct, theme::FONT_SMALL * s) / s;
            rx -= pw + 16.0;
            self.regular.draw_in(
                c,
                &pct,
                rx * s,
                0.0,
                TOOLBAR_H * s,
                theme::FONT_SMALL * s,
                theme::palette().text_dim,
            );
        }
        if self.cropping {
            let hint = if self.crop.is_some() {
                "Entrée : recadrer · Échap : annuler"
            } else {
                "Tracez la zone à garder"
            };
            let hw = self.regular.measure(hint, theme::FONT_SMALL * s);
            let hx = (vw * s - hw) / 2.0;
            self.regular.draw_in(
                c,
                hint,
                hx,
                TOOLBAR_H * s,
                28.0 * s,
                theme::FONT_SMALL * s,
                theme::palette().text,
            );
        }
    }

    fn draw_info(&self, c: &mut Canvas, s: f32) {
        let x0 = self.width as f32 - INFO_W;
        c.fill_rounded(
            x0 * s,
            0.0,
            INFO_W * s,
            self.height as f32 * s,
            0.0,
            theme::palette().panel,
        );
        c.fill_rounded(
            x0 * s,
            0.0,
            s,
            self.height as f32 * s,
            0.0,
            theme::palette().panel_border(),
        );
        let Some(pic) = self.current.as_ref() else {
            return;
        };
        let mut y = TOOLBAR_H + 16.0;
        let x = x0 + 20.0;
        self.bold.draw_in(
            c,
            "Informations",
            x * s,
            y * s,
            24.0 * s,
            theme::FONT_TITLE * s,
            theme::palette().text,
        );
        y += 36.0;
        let mut rows = vec![
            (
                "Dimensions".to_string(),
                format!("{} × {} pixels", pic.width(), pic.height()),
            ),
            ("Taille".to_string(), human_size(pic.file_size)),
            (
                "Format".to_string(),
                pic.format
                    .map(|f| format!("{f:?}").to_uppercase())
                    .unwrap_or_else(|| "—".into()),
            ),
        ];
        if let Some(m) = pic.modified {
            let d: chrono::DateTime<chrono::Local> = m.into();
            rows.push((
                "Modifiée".to_string(),
                d.format("%d/%m/%Y %H:%M").to_string(),
            ));
        }
        rows.extend(pic.exif.iter().cloned());
        rows.push((
            "Dossier".to_string(),
            pic.path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        for (label, value) in rows {
            self.regular.draw_in(
                c,
                &label,
                x * s,
                y * s,
                18.0 * s,
                theme::FONT_SMALL * s,
                theme::palette().text_dim,
            );
            y += 18.0;
            for line in self.regular.wrap(&value, 13.0, INFO_W - 40.0, 3) {
                self.regular.draw_in(
                    c,
                    &line,
                    x * s,
                    y * s,
                    20.0 * s,
                    13.0 * s,
                    theme::palette().text,
                );
                y += 20.0;
            }
            y += 10.0;
        }
    }
}

impl WindowHandler for Apercu {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        if let (Some(w), Some(h)) = configure.new_size {
            self.width = w.get();
            self.height = h.get();
        }
        self.fullscreen = configure.is_fullscreen();
        self.chrome.configure(
            configure.is_activated(),
            configure.is_maximized(),
            configure.is_fullscreen(),
        );
        self.configured = true;
        self.draw();
    }
}

impl PointerHandler for Apercu {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let toolbar_was = self.toolbar_visible();
        let mut redraw = false;
        for event in events {
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = (x, y);
                    redraw |= self.chrome.motion(x as f32, y as f32, TOOLBAR_H / 2.0);
                    self.toolbar_until = Instant::now() + TOOLBAR_IDLE;
                    match self.drag {
                        Some(Drag::Pan(start, center)) => {
                            let k = self.display_scale() / self.scale.max(1) as f64;
                            self.center =
                                (center.0 - (x - start.0) / k, center.1 - (y - start.1) / k);
                            redraw = true;
                        }
                        Some(Drag::Crop(start)) => {
                            let p = self.to_image(x, y);
                            if let Some(pic) = self.current.as_ref() {
                                let (w, h) = (pic.width() as f64, pic.height() as f64);
                                self.crop =
                                    Some((start.0, start.1, p.0.clamp(0.0, w), p.1.clamp(0.0, h)));
                            }
                            redraw = true;
                        }
                        None => {}
                    }
                }
                PointerEventKind::Leave { .. } => redraw |= self.chrome.leave(),
                PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } => {
                    if self.toolbar_visible()
                        && let Some(b) = self.chrome.press(x as f32, y as f32, TOOLBAR_H / 2.0)
                    {
                        self.chrome_action(b);
                        continue;
                    }
                    if self.toolbar_visible() && y < TOOLBAR_H as f64 {
                        match self.button_at(x, y) {
                            Some(b) => self.press_button(b),
                            None => {
                                // Barre vide : déplacer la fenêtre
                                if let Some(seat) = self.seat.as_ref() {
                                    self.window.move_(seat, serial);
                                }
                            }
                        }
                        continue;
                    }
                    if self.cropping {
                        let p = self.to_image(x, y);
                        if let Some(pic) = self.current.as_ref() {
                            let p = (
                                p.0.clamp(0.0, pic.width() as f64),
                                p.1.clamp(0.0, pic.height() as f64),
                            );
                            self.drag = Some(Drag::Crop(p));
                            self.crop = Some((p.0, p.1, p.0, p.1));
                        }
                        continue;
                    }
                    let double = self
                        .last_click
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(350));
                    self.last_click = Some(Instant::now());
                    if double {
                        self.last_click = None;
                        if self.zoom.is_none() {
                            let factor = self.scale.max(1) as f64 / self.display_scale();
                            self.zoom_at(factor, (x, y));
                        } else {
                            self.set_zoom(None);
                        }
                        continue;
                    }
                    if self.zoom.is_some() {
                        let center = self.current_center();
                        self.drag = Some(Drag::Pan((x, y), center));
                    }
                }
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => {
                    if let Some(Drag::Crop(_)) = self.drag
                        && let Some((x0, y0, x1, y1)) = self.crop
                        && ((x1 - x0).abs() < 4.0 || (y1 - y0).abs() < 4.0)
                    {
                        self.crop = None;
                        redraw = true;
                    }
                    self.drag = None;
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = if vertical.discrete != 0 {
                        vertical.discrete as f64
                    } else {
                        vertical.absolute / 15.0
                    };
                    if steps != 0.0 {
                        self.zoom_at(1.15f64.powf(-steps), (x, y));
                    }
                }
                _ => {}
            }
        }
        if redraw || toolbar_was != self.toolbar_visible() {
            self.draw();
        }
    }
}

impl Apercu {
    /// Centre de la vue en pixels d'image (après bornage)
    fn current_center(&self) -> (f64, f64) {
        let (vw, vh) = self.view();
        self.to_image(vw / 2.0, self.view_top() + vh / 2.0)
    }
}

impl CompositorHandler for Apercu {
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

impl KeyboardHandler for Apercu {
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
        modifiers: Modifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl SeatHandler for Apercu {
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
        self.seat = Some(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app, _, event| app.key(event)),
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
            // Libéré une fois la distribution des événements finie : la
            // répétition des touches (smithay-client-toolkit) retire son
            // minuteur de calloop en disparaissant, ce qui plante si la
            // boucle est en train de distribuer (clavier débranché)
            self.loop_handle.insert_idle(move |_| k.release());
        }
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for Apercu {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Apercu {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Apercu {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Apercu);
delegate_output!(Apercu);
delegate_seat!(Apercu);
delegate_keyboard!(Apercu);
delegate_pointer!(Apercu);
delegate_shm!(Apercu);
delegate_xdg_shell!(Apercu);
delegate_xdg_window!(Apercu);
delegate_registry!(Apercu);
