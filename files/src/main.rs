//! Fichiers : explorateur d'odysseus-desktop. Barre latérale de favoris, barre
//! d'outils (précédent / suivant, chemin cliquable), liste nom · taille · date.
//!
//! Clavier : flèches, Pg.préc/Pg.suiv, Début/Fin, Entrée (ouvrir),
//! Retour arrière ou Alt+← (précédent), Alt+→ (suivant), Alt+↑ (dossier parent),
//! Ctrl+H (fichiers cachés), F5 (relire), Ctrl+Q (quitter), lettres (aller à),
//! Espace (aperçu d'une image), Suppr (corbeille), Ctrl+Z (annuler la mise à la
//! corbeille). Dans la corbeille : bouton « Remettre » ou Ctrl+Retour arrière
//! pour remettre l'élément à sa place ; double-clic l'ouvre comme ailleurs.
//! Souris : clic (sélection et panneau d'informations à droite), double-clic
//! pour ouvrir, clic sur un en-tête de colonne pour trier, molette, glisser un
//! élément vers une autre fenêtre ou sur un dossier, glisser la barre d'outils
//! pour déplacer la fenêtre. Ctrl+C copie le chemin, Ctrl+I montre ou masque
//! le panneau, Échap le ferme.

mod actions;
mod archive;
mod clip;
mod dnd;
mod edit;
mod field;
mod grid;
mod info;
mod model;
mod search;
mod undo;

use std::os::fd::AsFd;
use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Image, Text},
    theme::{self, Rgba, icon},
};
use info::Details;
use model::{Browser, SortKey, format_date, format_size, is_text};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{DataDeviceManagerState, data_device::DataDevice},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
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

// Mise en page, en pixels logiques (grille de 4)
const SIDEBAR_W: f32 = 208.0;
const TOOLBAR_H: f32 = 52.0;
/// Centre vertical des boutons de la fenêtre, à hauteur de la barre d'outils
const CHROME_Y: f32 = TOOLBAR_H / 2.0;
const HEADER_H: f32 = 28.0;
const ROW_H: f32 = 30.0;
/// Vue en icônes : marge entre la barre d'outils et la première rangée
const GRID_TOP: f32 = 12.0;
const STATUS_H: f32 = 28.0;
const SIZE_COL_W: f32 = 96.0;
const DATE_COL_W: f32 = 168.0;
/// Panneau d'informations, et largeur de fenêtre en dessous de laquelle il se cache
const INFO_W: f32 = 280.0;
const INFO_MIN_WINDOW: u32 = 860;
/// Distance au-delà de laquelle un appui sur une ligne devient un glisser
const DRAG_START: f64 = 6.0;
/// Durée de la confirmation d'une action sur son bouton
const CONFIRM_TIME: Duration = Duration::from_millis(1500);
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Zones cliquables, en coordonnées logiques de la fenêtre
/// Au-delà, ouvrir toute une sélection d'un coup est refusé
const MAX_OPEN: usize = 12;

#[derive(Clone, PartialEq)]
enum Hit {
    Row(usize),
    Place(PathBuf),
    /// Démonter un disque externe (point de montage)
    Eject(PathBuf),
    /// Vider la corbeille (second clic pour confirmer)
    EmptyTrash,
    /// Remettre l'élément sélectionné de la corbeille à sa place
    Restore,
    Crumb(PathBuf),
    Back,
    Forward,
    SortBy(SortKey),
    /// Panneau d'informations : fermer, et ses actions sur l'élément sélectionné
    CloseInfo,
    Open,
    CopyPath,
    CopyImage,
    Trash,
    /// Terminal ouvert dans ce dossier
    Terminal(PathBuf),
    /// Décompresser l'archive sélectionnée
    Extract,
    /// Ouvrir l'Assistant avec l'élément en pièce jointe
    AskAgent,
    /// Renommer l'élément (panneau d'informations)
    Rename,
    /// Nouveau dossier (barre d'outils)
    NewFolder,
    /// Champ du chemin tapé
    Goto,
    /// Liste ou icônes
    View(grid::View),
    /// Bouton de recherche, son champ, sa portée (sous-dossiers ou non)
    Search,
    SearchField,
    Scope(bool),
}

/// Glisser-déposer en cours depuis Fichiers
pub struct Drag {
    source: smithay_client_toolkit::data_device_manager::data_source::DragSource,
    paths: Vec<PathBuf>,
    /// Image qui suit le pointeur (gardée vivante pendant le glisser)
    _icon: wl_surface::WlSurface,
}

struct Files {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    qh: QueueHandle<Files>,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    /// Glisser-déposer : gestionnaire, périphérique de données du siège
    data_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    /// Appui sur une ligne (position, ligne, numéro d'événement) : un glisser possible
    press: Option<(f64, f64, usize, u32)>,
    drag: Option<Drag>,
    /// Dossier survolé pendant un glisser (surligné, destination du dépôt)
    drop_target: Option<PathBuf>,
    /// Panneau d'informations ouvert, et détails de l'élément affiché
    info: bool,
    details: Option<Details>,
    /// Bouton du panneau survolé, et action tout juste faite (confirmée un instant)
    hover: Option<Hit>,
    done: Option<(Hit, Instant)>,
    thumbs: channel::Sender<(PathBuf, Option<Image>)>,
    /// Fin d'une décompression (fil) : ce qui a été créé, ou l'erreur
    extracts: channel::Sender<Result<PathBuf, String>>,
    /// Copier-coller de fichiers : ce que cette fenêtre a mis dans le
    /// presse-papiers, fin d'un collage (fil), dernier numéro d'événement
    /// (clavier ou clic) pour prendre le presse-papiers
    clip: Option<clip::Clip>,
    pastes: channel::Sender<clip::Pasted>,
    serial: u32,
    loop_handle: LoopHandle<'static, Files>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    icons: Text,
    browser: Browser,
    /// Dernier état publié pour l'Assistant (dossier et sélection)
    published: String,
    places: Vec<(String, PathBuf)>,
    hits: Vec<((f64, f64, f64, f64), Hit)>,
    last_click: Option<(Instant, usize)>,
    typed: (String, Instant),
    /// Derniers éléments mis à la corbeille (Ctrl+Z les remet)
    /// Opérations à défaire (Ctrl+Z), la dernière en fin
    undo: Vec<undo::Op>,
    /// Clic sans touche sur un élément d'une sélection multiple : elle se
    /// réduit à lui au relâchement, sauf si l'appui devient un glisser
    collapse: Option<usize>,
    /// Boutons de la fenêtre (fermer, réduire, agrandir), et dernier clic dans
    /// la barre d'outils (double-clic : agrandir)
    chrome: common::chrome::Chrome,
    last_toolbar_click: Option<Instant>,
    /// Renommage sur place, sélection par zone, menu du clic droit (edit.rs)
    rename: Option<edit::Rename>,
    /// Chemin tapé (Ctrl+L), à la place du fil d'Ariane
    goto: Option<field::Field>,
    /// Recherche (Ctrl+F), et lots de résultats du fil des sous-dossiers
    search: Option<search::Search>,
    /// Liste ou icônes ; vignettes de la grille ; tuiles ou lignes affichées
    /// (pixels logiques), pour la sélection par zone
    view: grid::View,
    tiles: grid::Thumbs,
    item_rects: Vec<grid::ItemRect>,
    found: channel::Sender<search::Found>,
    band: Option<edit::Band>,
    menu: Option<edit::Menu>,
    /// Haut de la première ligne, et fin du nom de chaque ligne affichée
    /// (pixels logiques) : glisser depuis le nom déplace, ailleurs sélectionne
    rows_top: f64,
    name_ends: Vec<(usize, f64)>,
    /// Dossier affiché surveillé (inotify) : la liste suit les fichiers qui
    /// arrivent (captures d'écran, téléchargements) ou s'en vont
    watched: Option<(PathBuf, RegistrationToken)>,
    /// « Vider la corbeille » armé : un second clic confirme
    empty_armed: Option<Instant>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

/// Favoris : dossiers usuels qui existent, puis le système.
fn places(home: &Path) -> Vec<(String, PathBuf)> {
    let mut list = vec![("Accueil".to_string(), home.to_path_buf())];
    for (label, dir) in [
        ("Bureau", "Bureau"),
        ("Documents", "Documents"),
        ("Images", "Images"),
        ("Téléchargements", "Téléchargements"),
        ("Musique", "Musique"),
        ("Vidéos", "Vidéos"),
        ("Projet odysseus", "odysseus-desktop"),
    ] {
        let p = home.join(dir);
        if p.is_dir() {
            list.push((label.to_string(), p));
        }
    }
    if let Some(trash) = common::trash::files_dir() {
        let _ = std::fs::create_dir_all(&trash);
        let _ = std::fs::create_dir_all(trash.with_file_name("info"));
        list.push(("Corbeille".to_string(), trash));
    }
    list.push(("Racine".to_string(), PathBuf::from("/")));
    if Path::new("/sources").is_dir() {
        list.push(("Sources".to_string(), PathBuf::from("/sources")));
    }
    list
}

fn is_image_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff"
        )
    })
}

/// Lance un programme d'odysseus installé à côté de Fichiers (Aperçu…).
fn spawn_sibling(name: &str, arg: &Path) {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
    else {
        return;
    };
    if let Ok(mut child) = std::process::Command::new(exe)
        .arg(arg)
        .process_group(0)
        .spawn()
    {
        std::thread::spawn(move || child.wait());
    }
}

/// Disques externes montés pour l'utilisateur (/run/media/<utilisateur>/…).
fn devices() -> Vec<(String, PathBuf)> {
    let user = std::env::var("USER").unwrap_or_default();
    let prefix = format!("/run/media/{user}/");
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut list: Vec<(String, PathBuf)> = mounts
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        // /proc/mounts code les espaces en \040
        .map(|m| m.replace("\\040", " "))
        .filter(|m| m.starts_with(&prefix))
        .map(|m| (m[prefix.len()..].to_string(), PathBuf::from(m)))
        .collect();
    list.sort();
    list.dedup();
    list
}

/// État publié pour l'Assistant, propre à cet affichage Wayland
fn state_file() -> Option<PathBuf> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let display = display.rsplit('/').next().unwrap_or("").to_string();
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join(format!("odysseus-files.{display}.json")))
}

fn main() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    let start = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| home.clone());

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_app_id("odysseus-files");
    window.set_min_size(Some((640, 420)));
    window.commit();

    let mut event_loop: EventLoop<'static, Files> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    // Vignettes du panneau d'informations, décodées dans un fil
    let (thumbs, thumb_rx) = channel::channel::<(PathBuf, Option<Image>)>();
    let (extracts, extract_rx) = channel::channel::<Result<PathBuf, String>>();
    let (pastes, paste_rx) = channel::channel::<clip::Pasted>();
    let (found, found_rx) = channel::channel::<search::Found>();
    let (tile_tx, tile_rx) = channel::channel::<(PathBuf, Option<Image>)>();
    event_loop
        .handle()
        .insert_source(tile_rx, |event, _, files| {
            if let ChannelEvent::Msg((path, image)) = event {
                files.tiles.store(path, image);
                files.draw();
            }
        })
        .expect("canal des vignettes de la grille");
    event_loop
        .handle()
        .insert_source(found_rx, |event, _, files| {
            if let ChannelEvent::Msg(found) = event {
                files.found(found);
            }
        })
        .expect("canal de la recherche");
    event_loop
        .handle()
        .insert_source(paste_rx, |event, _, files| {
            if let ChannelEvent::Msg(done) = event {
                files.pasted(done);
            }
        })
        .expect("canal des collages");
    event_loop
        .handle()
        .insert_source(extract_rx, |event, _, files| {
            if let ChannelEvent::Msg(result) = event {
                files.extracted(result);
            }
        })
        .expect("canal des décompressions");
    event_loop
        .handle()
        .insert_source(thumb_rx, |event, _, files| {
            if let ChannelEvent::Msg((path, image)) = event
                && let Some(d) = files.details.as_mut()
                && d.path == path
            {
                d.thumbnail = image;
                files.draw();
            }
        })
        .expect("canal des vignettes");

    // Thème changé (centre de contrôle) : redessin avec la nouvelle palette
    if let Some(fd) = theme::watch() {
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, files| {
                theme::drain(fd.as_fd());
                if theme::reload() {
                    files.draw();
                }
                Ok(PostAction::Continue)
            },
        );
    }

    // Clé USB branchée ou retirée : la barre latérale suit (toutes les 2 s)
    let mut known = devices();
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(2)),
            move |_, _, files| {
                let now = devices();
                if now != known {
                    known = now;
                    files.draw();
                }
                TimeoutAction::ToDuration(Duration::from_secs(2))
            },
        )
        .expect("minuterie");

    let mut files = Files {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        data_manager: DataDeviceManagerState::bind(&globals, &qh).ok(),
        compositor,
        qh: qh.clone(),
        pool: SlotPool::new(2048 * 1024 * 4, &shm).expect("pool shm"),
        shm,
        window,
        data_device: None,
        press: None,
        drag: None,
        drop_target: None,
        info: false,
        details: None,
        hover: None,
        done: None,
        thumbs,
        extracts,
        clip: None,
        pastes,
        serial: 0,
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        seat: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        browser: Browser::new(start),
        published: String::new(),
        places: places(&home),
        hits: Vec::new(),
        last_click: None,
        typed: (String::new(), Instant::now()),
        undo: Vec::new(),
        collapse: None,
        chrome: common::chrome::Chrome::default(),
        last_toolbar_click: None,
        rename: None,
        goto: None,
        search: None,
        view: grid::View::load(),
        tiles: grid::Thumbs::new(tile_tx),
        item_rects: Vec::new(),
        found,
        band: None,
        menu: None,
        rows_top: 0.0,
        name_ends: Vec::new(),
        watched: None,
        empty_armed: None,
        width: 960,
        height: 620,
        scale: 1,
        configured: false,
        exit: false,
    };
    files.update_title();
    files.watch_cwd();

    while !files.exit {
        // Compositeur arrêté ou connexion perdue : on s'arrête proprement
        if let Err(e) = event_loop.dispatch(None, &mut files) {
            eprintln!("files : connexion au compositeur perdue ({e})");
            break;
        }
    }
}

impl Files {
    fn update_title(&self) {
        let name = self
            .browser
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into());
        self.window.set_title(format!("{name} — Fichiers"));
    }

    /// Rangées entièrement visibles (lignes de la liste, ou rangées de tuiles)
    fn visible_rows(&self) -> usize {
        match self.view {
            grid::View::List => {
                let list_h = self.height as f32 - TOOLBAR_H - HEADER_H - STATUS_H;
                (list_h / ROW_H).floor().max(1.0) as usize
            }
            grid::View::Icons => {
                let list_h = self.height as f32 - TOOLBAR_H - GRID_TOP - STATUS_H;
                (list_h / grid::TILE_H).floor().max(1.0) as usize
            }
        }
    }

    /// Éléments par rangée : 1 en liste, les colonnes de la grille sinon
    fn grid_cols(&self) -> usize {
        match self.view {
            grid::View::List => 1,
            grid::View::Icons => {
                let info = if self.info_visible() { INFO_W } else { 0.0 };
                grid::columns(self.width as f32 - SIDEBAR_W - info - 32.0)
            }
        }
    }

    /// Faire défiler pour que la sélection reste visible (par rangées).
    fn follow_selection(&mut self) {
        let rows = self.visible_rows();
        let cols = self.grid_cols();
        let b = &mut self.browser;
        let (row, first) = (b.selected / cols, b.scroll / cols);
        let first = if row < first {
            row
        } else if row >= first + rows {
            row + 1 - rows
        } else {
            first
        };
        b.scroll = first * cols;
    }

    /// Ctrl+1 : icônes, Ctrl+2 : liste (comme Cmd+1 / Cmd+2 du Finder)
    fn set_view(&mut self, view: grid::View) {
        if self.view != view {
            self.view = view;
            view.save();
        }
        self.follow_selection();
        self.draw();
    }

    fn navigated(&mut self) {
        self.menu = None;
        self.goto = None;
        self.left_search_dir();
        self.watch_cwd();
        self.update_title();
        self.follow_selection();
        self.draw();
    }

    /// Surveille le dossier affiché (et lui seul) ; sa liste se relit d'elle-même
    fn watch_cwd(&mut self) {
        let cwd = self.browser.cwd.clone();
        if self.watched.as_ref().is_some_and(|(p, _)| *p == cwd) {
            return;
        }
        if let Some((_, token)) = self.watched.take() {
            self.loop_handle.remove(token);
        }
        let Some(fd) = common::watch::existing(&cwd) else {
            return;
        };
        let token = self.loop_handle.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, files| {
                common::watch::drain(fd.as_fd());
                // Le message en cours (« mis à la corbeille — Ctrl+Z ») reste affiché
                let message = files.browser.message.take();
                files.browser.reload();
                if files.browser.message.is_none() {
                    files.browser.message = message;
                }
                files.draw();
                Ok(PostAction::Continue)
            },
        );
        if let Ok(token) = token {
            self.watched = Some((cwd, token));
        }
    }

    fn open_selected(&mut self) {
        if self.browser.multiple() {
            return self.open_many();
        }
        let Some(entry) = self.browser.entries.get(self.browser.selected) else {
            return;
        };
        let path = entry.path.clone();
        if entry.is_dir {
            self.browser.open_dir(&path);
            self.navigated();
            return;
        }
        if is_image_path(&path) {
            spawn_sibling("apercu", &path);
            return;
        }
        // Double-clic sur une archive : décompression, comme sur macOS
        if archive::kind(&path).is_some() {
            return self.extract_selected();
        }
        let (name, size) = (entry.name.clone(), entry.size);
        self.open_file(&path, &name, size);
        self.draw();
    }

    /// Sélection multiple : chaque fichier s'ouvre (pas les dossiers ni les
    /// archives), dans la limite de MAX_OPEN
    fn open_many(&mut self) {
        let files: Vec<(PathBuf, String, u64)> = self
            .browser
            .selection()
            .iter()
            .filter(|e| !e.is_dir && archive::kind(&e.path).is_none())
            .map(|e| (e.path.clone(), e.name.clone(), e.size))
            .collect();
        if files.is_empty() {
            self.browser.message = Some("Aucun fichier à ouvrir dans la sélection".into());
        } else if files.len() > MAX_OPEN {
            self.browser.message = Some(format!(
                "{} fichiers : c'est beaucoup de fenêtres, ouvrez-en {MAX_OPEN} au plus",
                files.len()
            ));
        } else {
            for (path, name, size) in files {
                self.open_file(&path, &name, size);
            }
        }
        self.draw();
    }

    /// Ouvre un fichier : image dans Aperçu, texte dans l'éditeur
    fn open_file(&mut self, path: &Path, name: &str, size: u64) {
        if is_image_path(path) {
            spawn_sibling("apercu", path);
            return;
        }
        if is_text(path, size) {
            let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
            let result = std::process::Command::new(term)
                .arg("-e")
                .arg(editor)
                .arg(path)
                .process_group(0)
                .spawn();
            match result {
                // Attente dans un fil : pas de processus zombie
                Ok(mut child) => {
                    std::thread::spawn(move || child.wait());
                }
                Err(e) => {
                    self.browser.message = Some(format!("Impossible d'ouvrir l'éditeur : {e}"))
                }
            }
        } else {
            self.browser.message = Some(format!("Aucune application pour ouvrir « {name} »"));
        }
    }

    fn key(&mut self, event: KeyEvent) {
        if self.rename_key(&event) || self.goto_key(&event) || self.search_key(&event) {
            return;
        }
        if self.menu.take().is_some() && event.keysym == Keysym::Escape {
            return self.draw();
        }
        if event.keysym == Keysym::F2 {
            return self.start_rename();
        }
        if self.modifiers.ctrl
            && self.modifiers.shift
            && matches!(event.keysym, Keysym::N | Keysym::n)
        {
            return self.new_folder();
        }
        // Touches 1 et 2 de la rangée des chiffres (codes evdev), quelle que
        // soit la disposition
        if self.modifiers.ctrl && matches!(event.raw_code, 2 | 3) {
            let view = if event.raw_code == 2 {
                grid::View::Icons
            } else {
                grid::View::List
            };
            return self.set_view(view);
        }
        let n = self.browser.entries.len();
        let cols = self.grid_cols();
        let grid = self.view == grid::View::Icons;
        let page = self.visible_rows().saturating_sub(1).max(1) * cols;
        let alt = self.modifiers.alt;
        let ctrl = self.modifiers.ctrl;
        let cur = self.browser.selected;
        // Déplacement : Maj étend la sélection ; dans la grille, ← → d'une
        // case, ↑ ↓ d'une rangée
        let goto = match event.keysym {
            Keysym::Down if n > 0 && !alt && !ctrl => Some((cur + cols).min(n - 1)),
            Keysym::Up if !alt && !ctrl => Some(cur.saturating_sub(cols)),
            Keysym::Right if grid && n > 0 && !alt && !ctrl => Some((cur + 1).min(n - 1)),
            Keysym::Left if grid && !alt && !ctrl => Some(cur.saturating_sub(1)),
            Keysym::Next if n > 0 => Some((cur + page).min(n - 1)),
            Keysym::Prior => Some(cur.saturating_sub(page)),
            Keysym::Home => Some(0),
            Keysym::End => Some(n.saturating_sub(1)),
            _ => None,
        };
        if let Some(i) = goto {
            if self.modifiers.shift {
                self.browser.extend_to(i);
            } else {
                self.browser.select(i);
            }
            return self.navigated();
        }
        match event.keysym {
            // Alt+↑, ou Ctrl+↑ comme Cmd+↑ sur macOS : dossier parent
            Keysym::Up if alt || ctrl => self.browser.parent(),
            Keysym::a if ctrl => self.browser.select_all(),
            // Comme sur macOS : Entrée renomme, Espace ouvre (aperçu d'une
            // image), Ctrl+O ou Ctrl+↓ ouvrent aussi
            Keysym::Return | Keysym::KP_Enter if self.in_trash() => return self.open_selected(),
            Keysym::Return | Keysym::KP_Enter => return self.start_rename(),
            Keysym::o | Keysym::Down if ctrl => return self.open_selected(),
            Keysym::space => return self.open_selected(),
            Keysym::Left if alt => self.browser.back(),
            Keysym::Right if alt => self.browser.forward(),
            // Comme Cmd+Suppr sur macOS : remettre à sa place depuis la corbeille
            Keysym::BackSpace if ctrl && self.in_trash() => return self.restore_selected(),
            Keysym::BackSpace => self.browser.back(),
            Keysym::F5 => self.browser.reload(),
            Keysym::Delete | Keysym::KP_Delete => return self.trash_selected(),
            Keysym::z if ctrl => return self.undo(),
            Keysym::h if ctrl => self.browser.toggle_hidden(),
            Keysym::C | Keysym::c if ctrl && self.modifiers.shift => return self.copy_path(),
            Keysym::c if ctrl => return self.clip_selection(false),
            Keysym::x if ctrl => return self.clip_selection(true),
            Keysym::v if ctrl => return self.paste(),
            Keysym::d if ctrl => return self.duplicate(),
            Keysym::f if ctrl => return self.start_search(),
            Keysym::l if ctrl => return self.start_goto(),
            Keysym::t if ctrl => {
                let dir = self.browser.cwd.clone();
                return self.open_terminal(&dir);
            }
            Keysym::i if ctrl => self.info = !self.info,
            Keysym::Escape if self.browser.multiple() => {
                let cur = self.browser.selected;
                self.browser.select(cur);
            }
            Keysym::Escape if self.search.is_some() => return self.close_search(),
            Keysym::Escape if self.info => self.info = false,
            Keysym::q if ctrl => {
                self.exit = true;
                return;
            }
            _ => {
                // Recherche à la frappe : lettres tapées en moins d'une seconde
                if ctrl || alt || self.modifiers.logo {
                    return;
                }
                let Some(text) = event
                    .utf8
                    .filter(|t| t.chars().all(|c| !c.is_control()) && !t.is_empty())
                else {
                    return;
                };
                if self.typed.1.elapsed() > Duration::from_secs(1) {
                    self.typed.0.clear();
                }
                self.typed.0.push_str(&text);
                self.typed.1 = Instant::now();
                let prefix = self.typed.0.clone();
                self.browser.jump_to(&prefix);
            }
        }
        self.navigated();
    }

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits
            .iter()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .map(|(_, hit)| hit.clone())
    }

    fn click(&mut self, x: f64, y: f64, serial: u32) {
        match self.hit_at(x, y) {
            Some(Hit::Row(i)) => {
                if self.modifiers.ctrl {
                    self.browser.toggle(i);
                    self.last_click = None;
                    return self.draw();
                }
                if self.modifiers.shift {
                    self.browser.extend_to(i);
                    self.last_click = None;
                    return self.draw();
                }
                let double = self
                    .last_click
                    .is_some_and(|(t, row)| row == i && t.elapsed() < Duration::from_millis(400));
                if self.browser.multiple() && self.browser.is_picked(i) && !double {
                    // Peut-être le début d'un glisser de toute la sélection
                    self.collapse = Some(i);
                    self.last_click = Some((Instant::now(), i));
                    return;
                }
                if !(double && self.browser.multiple()) {
                    self.browser.select(i);
                }
                // En liste, un clic montre le panneau d'informations ; pas dans
                // la grille, qui changerait de colonnes sous le pointeur
                if self.view == grid::View::List {
                    self.info = true;
                }
                self.last_click = Some((Instant::now(), i));
                if double {
                    self.last_click = None;
                    self.open_selected();
                } else {
                    self.draw();
                }
            }
            Some(Hit::Place(path) | Hit::Crumb(path)) => {
                self.browser.open_dir(&path);
                self.navigated();
            }
            Some(Hit::Eject(path)) => self.eject(&path),
            Some(Hit::EmptyTrash) => self.empty_trash(),
            Some(Hit::Restore) => self.restore_selected(),
            Some(Hit::Back) => {
                self.browser.back();
                self.navigated();
            }
            Some(Hit::Forward) => {
                self.browser.forward();
                self.navigated();
            }
            Some(Hit::SortBy(key)) => {
                self.browser.sort_by(key);
                self.navigated();
            }
            Some(Hit::CloseInfo) => {
                self.info = false;
                self.draw();
            }
            Some(Hit::Open) => {
                self.open_selected();
                self.confirm(Hit::Open, Ok(()));
            }
            Some(Hit::CopyPath) => self.copy_path(),
            Some(Hit::CopyImage) => self.copy_image(),
            Some(Hit::Terminal(dir)) => self.open_terminal(&dir),
            Some(Hit::Extract) => self.extract_selected(),
            Some(Hit::AskAgent) => self.ask_agent(),
            Some(Hit::Rename) => self.start_rename(),
            Some(Hit::NewFolder) => self.new_folder(),
            Some(Hit::Goto) => {}
            Some(Hit::Search) => self.start_search(),
            Some(Hit::View(view)) => self.set_view(view),
            Some(Hit::SearchField) => self.draw(),
            Some(Hit::Scope(deep)) => self.set_scope(deep),
            Some(Hit::Trash) if self.in_trash() => self.restore_selected(),
            Some(Hit::Trash) => self.trash_selected(),
            None => {
                // Barre d'outils vide : double-clic agrandit, glisser déplace la fenêtre
                if y < TOOLBAR_H as f64 {
                    let double = self
                        .last_toolbar_click
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(400));
                    self.last_toolbar_click = Some(Instant::now());
                    if double {
                        self.last_toolbar_click = None;
                        self.chrome_action(common::chrome::Button::Maximize);
                    } else if let Some(seat) = self.seat.as_ref() {
                        self.window.move_(seat, serial);
                    }
                }
            }
        }
    }

    /// Boutons de la fenêtre : fermer, réduire, agrandir (Alt : plein écran)
    fn chrome_action(&mut self, button: common::chrome::Button) {
        use common::chrome::Button;
        match button {
            Button::Close => self.exit = true,
            Button::Minimize => self.window.set_minimized(),
            Button::Maximize if self.modifiers.alt || self.chrome.fullscreen => {
                if self.chrome.fullscreen {
                    self.window.unset_fullscreen();
                } else {
                    self.window.set_fullscreen(None);
                }
            }
            Button::Maximize if self.chrome.maximized => self.window.unset_maximized(),
            Button::Maximize => self.window.set_maximized(),
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.browser
            .entries
            .get(self.browser.selected)
            .map(|e| e.path.clone())
    }

    /// Ouvre l'Assistant avec la sélection en pièces jointes
    fn ask_agent(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("odysseus-assistant")));
        let mut cmd = exe.map(std::process::Command::new);
        if let Some(cmd) = cmd.as_mut() {
            for path in &paths {
                cmd.arg("--joindre").arg(path);
            }
        }
        if let Some(mut cmd) = cmd
            && let Ok(mut child) = cmd.process_group(0).spawn()
        {
            std::thread::spawn(move || child.wait());
            self.confirm(Hit::AskAgent, Ok(()));
        }
    }

    /// Pour l'Assistant appelé pendant que Fichiers est au premier plan
    /// (Super+A) : le dossier ouvert et la sélection, dans
    /// $XDG_RUNTIME_DIR/odysseus-files.<affichage>.json, réécrit quand ça change
    fn publish_state(&mut self) {
        let Some(path) = state_file() else {
            return;
        };
        // Une sélection voulue (Ctrl+clic, Maj+clic, Ctrl+A), pas la ligne du curseur
        let mut selection: Vec<String> = self
            .browser
            .picked
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        selection.sort();
        let text = serde_json::json!({
            "dossier": self.browser.cwd.to_string_lossy(),
            "selection": selection,
        })
        .to_string();
        if text == self.published {
            return;
        }
        self.published = text;
        let _ = std::fs::write(path, &self.published);
    }

    /// Chemins des éléments sélectionnés, dans l'ordre de la liste
    fn selected_paths(&self) -> Vec<PathBuf> {
        self.browser
            .selection()
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }

    /// Chemins complets dans le presse-papiers, un par ligne (wl-copy les
    /// garde après la fermeture)
    fn copy_path(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let text = paths
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        let result = wl_copy(&["--", &text], None);
        self.confirm(Hit::CopyPath, result);
    }

    /// Terminal (foot, ou $TERMINAL) ouvert dans un dossier
    fn open_terminal(&mut self, dir: &Path) {
        let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
        let result = std::process::Command::new(&term)
            .arg(format!("--working-directory={}", dir.display()))
            .current_dir(dir)
            .process_group(0)
            .spawn();
        match result {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
                self.confirm(Hit::Terminal(dir.to_path_buf()), Ok(()));
            }
            Err(e) => self.confirm(
                Hit::Terminal(dir.to_path_buf()),
                Err(format!("Terminal introuvable ({term}) : {e}")),
            ),
        }
    }

    /// Décompresse l'archive sélectionnée dans un fil ; la liste suit à la fin
    fn extract_selected(&mut self) {
        let archives: Vec<PathBuf> = self
            .selected_paths()
            .into_iter()
            .filter(|p| archive::kind(p).is_some())
            .collect();
        let [first, ..] = archives.as_slice() else {
            return;
        };
        self.browser.message = Some(if archives.len() == 1 {
            let name = first
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("Décompression de « {name} »…")
        } else {
            format!("Décompression de {} archives…", archives.len())
        });
        self.done = Some((Hit::Extract, Instant::now() + Duration::from_secs(3600)));
        let tx = self.extracts.clone();
        // Les unes après les autres, dans un seul fil
        std::thread::spawn(move || {
            for path in archives {
                let _ = tx.send(archive::extract(&path));
            }
        });
        self.draw();
    }

    fn extracted(&mut self, result: Result<PathBuf, String>) {
        self.done = None;
        match result {
            Ok(dest) => {
                self.browser.reload();
                if let Some(i) = self.browser.entries.iter().position(|e| e.path == dest) {
                    self.browser.select(i);
                }
                self.browser.message = None;
                self.navigated();
            }
            Err(e) => {
                self.browser.message = Some(format!("Décompression impossible : {e}"));
                self.draw();
            }
        }
    }

    /// L'image elle-même dans le presse-papiers, prête à coller (Ctrl+V)
    fn copy_image(&mut self) {
        let Some(path) = self.selected_path() else {
            return;
        };
        let result = wl_copy(&["--type", info::image_mime(&path)], Some(&path));
        self.confirm(Hit::CopyImage, result);
    }

    /// Action faite : le bouton le confirme un instant ; une erreur va dans la
    /// barre d'état.
    fn confirm(&mut self, button: Hit, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.done = Some((button, Instant::now()));
                let _ = self.loop_handle.insert_source(
                    Timer::from_duration(CONFIRM_TIME),
                    |_, _, files| {
                        if files
                            .done
                            .as_ref()
                            .is_some_and(|(_, at)| at.elapsed() >= CONFIRM_TIME)
                        {
                            files.done = None;
                            files.draw();
                        }
                        TimeoutAction::Drop
                    },
                );
            }
            Err(e) => self.browser.message = Some(e),
        }
        self.draw();
    }

    /// Survol des boutons du panneau : redessin seulement quand il change
    fn update_hover(&mut self, x: f64, y: f64) {
        let hover = self.hit_at(x, y).filter(|h| {
            matches!(
                h,
                Hit::Open
                    | Hit::CopyPath
                    | Hit::CopyImage
                    | Hit::Trash
                    | Hit::CloseInfo
                    | Hit::Terminal(_)
                    | Hit::Extract
                    | Hit::AskAgent
                    | Hit::Rename
                    | Hit::NewFolder
                    | Hit::Search
            )
        });
        if hover != self.hover {
            self.hover = hover;
            self.draw();
        }
    }

    fn in_trash(&self) -> bool {
        common::trash::files_dir().is_some_and(|t| self.browser.cwd == t)
    }

    fn trash_selected(&mut self) {
        if self.in_trash() {
            self.browser.message =
                Some("Déjà dans la corbeille : « Vider la corbeille » pour supprimer".into());
            return self.draw();
        }
        let picked: Vec<(PathBuf, String)> = self
            .browser
            .selection()
            .iter()
            .map(|e| (e.path.clone(), e.name.clone()))
            .collect();
        if picked.is_empty() {
            return;
        }
        let mut trashed = Vec::new();
        let mut error = None;
        for (path, _) in &picked {
            match common::trash::trash(path) {
                Ok(()) => trashed.push(path.clone()),
                Err(e) => error = Some(e),
            }
        }
        let count = trashed.len();
        self.record(undo::Op::Trashed(trashed));
        self.browser.reload();
        self.browser.message = Some(match (count, error) {
            (_, Some(e)) if count == 0 => e,
            (_, Some(e)) => format!("{count} élément(s) mis à la corbeille ; {e}"),
            (1, None) => format!(
                "« {} » mis à la corbeille — Ctrl+Z pour annuler",
                picked[0].1
            ),
            (n, None) => format!("{n} éléments mis à la corbeille — Ctrl+Z pour annuler"),
        });
        self.navigated();
    }

    /// Dans la corbeille : remet l'élément sélectionné à sa place.
    fn restore_selected(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let items = common::trash::list();
        let mut done = Vec::new();
        let mut error = None;
        for path in &paths {
            match items.iter().find(|i| &i.path == path) {
                Some(item) => match common::trash::restore(item) {
                    Ok(()) => done.push(item.original.clone()),
                    Err(e) => error = Some(e),
                },
                None => error = Some("Origine inconnue".into()),
            }
        }
        let message = match (done.as_slice(), error) {
            ([], Some(e)) => e,
            (d, Some(e)) => format!("{} élément(s) remis en place ; {e}", d.len()),
            ([one], None) => format!("Remis dans {}", one.display()),
            (d, None) => format!("{} éléments remis en place", d.len()),
        };
        self.browser.reload();
        self.browser.message = Some(message);
        self.navigated();
    }

    fn empty_trash(&mut self) {
        let armed = self
            .empty_armed
            .is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        if !armed {
            self.empty_armed = Some(Instant::now());
            return self.draw();
        }
        self.empty_armed = None;
        let message = match common::trash::empty() {
            Ok(n) => format!("Corbeille vidée ({n} élément(s))"),
            Err(e) => format!("Impossible de vider la corbeille : {e}"),
        };
        self.browser.reload();
        self.browser.message = Some(message);
        self.navigated();
    }

    /// Démonte un disque externe (systemd, autorisé par polkit) ; on quitte
    /// d'abord le dossier s'il est affiché.
    fn eject(&mut self, path: &Path) {
        if self.browser.cwd.starts_with(path) {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
            self.browser.open_dir(&home);
        }
        let result = std::process::Command::new("systemd-mount")
            .arg("--umount")
            .arg(path)
            .output();
        self.browser.message = match result {
            Ok(out) if out.status.success() => Some(format!(
                "« {} » peut être retiré",
                path.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            )),
            Ok(out) => Some(format!(
                "Éjection impossible : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => Some(format!("Éjection impossible : {e}")),
        };
        self.navigated();
    }

    /// Panneau visible : ouvert, une sélection, et la place de l'afficher
    fn info_visible(&self) -> bool {
        self.info && self.width >= INFO_MIN_WINDOW && !self.browser.entries.is_empty()
    }

    /// Détails de l'élément sélectionné, relus quand la sélection change ;
    /// la vignette d'une image est décodée dans un fil.
    fn refresh_details(&mut self) {
        let Some(path) = self.selected_path().filter(|_| self.info_visible()) else {
            return;
        };
        if self.details.as_ref().is_some_and(|d| d.path == path) {
            return;
        }
        self.details = Some(Details::read(&path));
        if info::is_image(&path) {
            let tx = self.thumbs.clone();
            let side = (INFO_W * 2.0) as u32;
            std::thread::spawn(move || {
                let image = info::thumbnail(&path, side, side);
                let _ = tx.send((path, image));
            });
        }
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        self.publish_state();
        self.refresh_details();
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
        let info_px = if self.info_visible() { INFO_W * s } else { 0.0 };
        // Bord droit de la liste (le panneau d'informations prend la suite)
        let right = w as f32 - info_px;
        let rows = self.visible_rows();
        // Le défilement tombe sur le début d'une rangée (la largeur a pu changer)
        let cols = self.grid_cols();
        self.browser.scroll -= self.browser.scroll % cols;
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
        c.fill(theme::palette().content);
        let mut hits = Vec::new();
        // Zone cliquable en pixels du tampon → coordonnées logiques
        let mut hit = |x: f32, y: f32, w: f32, h: f32, what: Hit| {
            hits.push((
                (
                    x as f64 / s as f64,
                    y as f64 / s as f64,
                    w as f64 / s as f64,
                    h as f64 / s as f64,
                ),
                what,
            ));
        };

        // Barre latérale
        let sw = SIDEBAR_W * s;
        c.fill_rounded(0.0, 0.0, sw, h as f32, 0.0, theme::palette().sidebar);
        c.fill_rounded(sw - s, 0.0, s, h as f32, 0.0, theme::palette().ink(0.06));
        self.chrome.draw(&mut c, CHROME_Y, s);
        self.regular.draw_in(
            &mut c,
            "FAVORIS",
            20.0 * s,
            TOOLBAR_H * s,
            24.0 * s,
            11.0 * s,
            theme::palette().text_dim,
        );
        let mut y = TOOLBAR_H * s + 28.0 * s;
        for (label, path) in &self.places {
            if label == "Racine" {
                y += 12.0 * s;
                self.regular.draw_in(
                    &mut c,
                    "SYSTÈME",
                    20.0 * s,
                    y,
                    24.0 * s,
                    11.0 * s,
                    theme::palette().text_dim,
                );
                y += 28.0 * s;
            }
            let row_h = 32.0 * s;
            let active = *path == self.browser.cwd;
            if self.drop_target.as_ref() == Some(path) {
                let a = theme::palette().accent;
                c.fill_rounded(
                    10.0 * s,
                    y,
                    sw - 20.0 * s,
                    row_h,
                    8.0 * s,
                    a.with_alpha(0.35),
                );
            } else if active {
                let mut sel = theme::palette().accent;
                sel.a = 0.18;
                c.fill_rounded(10.0 * s, y, sw - 20.0 * s, row_h, 8.0 * s, sel);
            }
            draw_folder_icon(
                &mut c,
                22.0 * s,
                y + 8.0 * s,
                16.0 * s,
                theme::palette().accent,
            );
            let color = if active {
                theme::palette().text
            } else {
                theme::palette().text_soft()
            };
            self.regular
                .draw_in(&mut c, label, 48.0 * s, y, row_h, 14.0 * s, color);
            hit(10.0 * s, y, sw - 20.0 * s, row_h, Hit::Place(path.clone()));
            y += row_h + 2.0 * s;
        }

        // Appareils : clés USB et disques externes, avec bouton d'éjection
        let devices = devices();
        if !devices.is_empty() {
            y += 12.0 * s;
            self.regular.draw_in(
                &mut c,
                "APPAREILS",
                20.0 * s,
                y,
                24.0 * s,
                11.0 * s,
                theme::palette().text_dim,
            );
            y += 28.0 * s;
        }
        for (label, path) in &devices {
            let row_h = 32.0 * s;
            let active = self.browser.cwd.starts_with(path);
            if active {
                let mut sel = theme::palette().accent;
                sel.a = 0.18;
                c.fill_rounded(10.0 * s, y, sw - 20.0 * s, row_h, 8.0 * s, sel);
            }
            let color = theme::palette().accent;
            self.icons.icon(
                &mut c,
                icon::HARD_DRIVE,
                30.0 * s,
                y + row_h / 2.0,
                16.0 * s,
                color,
            );
            let text = self.regular.truncate(label, 14.0 * s, sw - 92.0 * s);
            self.regular.draw_in(
                &mut c,
                &text,
                48.0 * s,
                y,
                row_h,
                14.0 * s,
                theme::palette().text_soft(),
            );
            hit(10.0 * s, y, sw - 50.0 * s, row_h, Hit::Place(path.clone()));
            self.icons.icon(
                &mut c,
                icon::EJECT,
                sw - 26.0 * s,
                y + row_h / 2.0,
                14.0 * s,
                theme::palette().text_dim,
            );
            hit(sw - 40.0 * s, y, 30.0 * s, row_h, Hit::Eject(path.clone()));
            y += row_h + 2.0 * s;
        }

        // Barre d'outils : précédent / suivant, chemin
        let x0 = sw;
        let mid = TOOLBAR_H * s / 2.0;
        for (i, (glyph, enabled, what)) in [
            ("‹", self.browser.can_back(), Hit::Back),
            ("›", self.browser.can_forward(), Hit::Forward),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = x0 + 16.0 * s + i as f32 * 34.0 * s;
            let size = 28.0 * s;
            c.fill_rounded(
                bx,
                mid - size / 2.0,
                size,
                size,
                8.0 * s,
                theme::palette().subtle,
            );
            let color = if enabled {
                theme::palette().text
            } else {
                theme::palette().text_dim.with_alpha(0.5)
            };
            let gw = self.bold.measure(glyph, 20.0 * s);
            self.bold.draw_in(
                &mut c,
                glyph,
                bx + (size - gw) / 2.0,
                mid - size / 2.0 - 2.0 * s,
                size,
                20.0 * s,
                color,
            );
            if enabled {
                hit(bx, mid - size / 2.0, size, size, what);
            }
        }
        // Chemin : chaque segment est cliquable ; on garde la fin si c'est trop long
        let mut crumbs: Vec<(String, PathBuf)> = Vec::new();
        let mut acc = PathBuf::new();
        for comp in self.browser.cwd.components() {
            acc.push(comp);
            let label = match comp {
                std::path::Component::RootDir => "/".to_string(),
                other => other.as_os_str().to_string_lossy().into_owned(),
            };
            crumbs.push((label, acc.clone()));
        }
        // À droite de la barre d'outils : recherche (Ctrl+F), nouveau dossier
        // (Ctrl+Maj+N), terminal dans le dossier affiché (Ctrl+T)
        let mut buttons = vec![(icon::TERMINAL, Hit::Terminal(self.browser.cwd.clone()))];
        if common::trash::files_dir().is_none_or(|t| self.browser.cwd != t) {
            buttons.push((icon::FOLDER_PLUS, Hit::NewFolder));
        }
        if self.search.is_none() {
            buttons.push((icon::SEARCH, Hit::Search));
        }
        let button = 28.0 * s;
        let buttons_left = right - 16.0 * s - buttons.len() as f32 * (button + 8.0 * s) + 8.0 * s;
        // Liste | icônes, à gauche des boutons
        let view_w = 2.0 * button + 4.0 * s;
        let view_x = buttons_left - 8.0 * s - view_w;
        c.fill_rounded(
            view_x,
            mid - button / 2.0,
            view_w,
            button,
            8.0 * s,
            theme::palette().subtle,
        );
        for (i, (glyph, view)) in [
            (icon::LIST, grid::View::List),
            (icon::LAYOUT_GRID, grid::View::Icons),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = view_x + 2.0 * s + i as f32 * button;
            let active = self.view == view;
            if active {
                c.fill_rounded(
                    bx,
                    mid - button / 2.0 + 2.0 * s,
                    button,
                    button - 4.0 * s,
                    6.0 * s,
                    theme::palette().tile_hover(),
                );
            }
            let color = if active {
                theme::palette().text
            } else {
                theme::palette().text_dim
            };
            self.icons
                .icon(&mut c, glyph, bx + button / 2.0, mid, 14.0 * s, color);
            hit(bx, mid - button / 2.0, button, button, Hit::View(view));
        }
        let mut buttons_left = view_x;
        // Corbeille : « Remettre » et « Vider » (second clic pour confirmer), à
        // gauche des boutons de la barre
        if common::trash::files_dir().is_some_and(|t| self.browser.cwd == t)
            && !self.browser.entries.is_empty()
        {
            let armed = self
                .empty_armed
                .is_some_and(|t| t.elapsed() < Duration::from_secs(4));
            let label = if armed {
                "Confirmer ?"
            } else {
                "Vider la corbeille"
            };
            let lw = self.regular.measure(label, 13.0 * s) + 24.0 * s;
            let bx = view_x - 8.0 * s - lw;
            let bh = 28.0 * s;
            let bg = if armed {
                theme::palette().danger
            } else {
                theme::palette().subtle
            };
            let fg = if armed {
                theme::palette().on_accent
            } else {
                theme::palette().text
            };
            c.fill_rounded(bx, mid - bh / 2.0, lw, bh, 8.0 * s, bg);
            self.regular.draw_in(
                &mut c,
                label,
                bx + 12.0 * s,
                mid - bh / 2.0,
                bh,
                13.0 * s,
                fg,
            );
            hit(bx, mid - bh / 2.0, lw, bh, Hit::EmptyTrash);
            // « Remettre » : l'élément sélectionné retourne à son emplacement
            let rl = "Remettre";
            let rw = self.regular.measure(rl, 13.0 * s) + 24.0 * s;
            let rx = bx - rw - 8.0 * s;
            c.fill_rounded(rx, mid - bh / 2.0, rw, bh, 8.0 * s, theme::palette().subtle);
            self.regular.draw_in(
                &mut c,
                rl,
                rx + 12.0 * s,
                mid - bh / 2.0,
                bh,
                13.0 * s,
                theme::palette().text,
            );
            hit(rx, mid - bh / 2.0, rw, bh, Hit::Restore);
            buttons_left = rx;
        }
        let search_w = if self.search.is_some() {
            (240.0 * s).min((right - x0) * 0.35)
        } else {
            0.0
        };
        let crumb_x0 = x0 + 100.0 * s;
        let avail = buttons_left - search_w - crumb_x0 - 24.0 * s;
        let sep_w = self.regular.measure("  ›  ", 14.0 * s);
        let widths: Vec<f32> = crumbs
            .iter()
            .map(|(l, _)| self.bold.measure(l, 14.0 * s))
            .collect();
        let mut first = 0;
        while first + 1 < crumbs.len()
            && widths[first..].iter().sum::<f32>() + sep_w * (crumbs.len() - first) as f32 > avail
        {
            first += 1;
        }
        let mut cx = crumb_x0;
        if let Some(field) = self.goto.as_ref() {
            let fh = 30.0 * s;
            let fw = avail + 40.0 * s;
            field.draw(
                &self.regular,
                &mut c,
                crumb_x0 - 8.0 * s,
                mid - fh / 2.0,
                fw,
                fh,
                s,
                "",
                true,
            );
            hit(crumb_x0 - 8.0 * s, mid - fh / 2.0, fw, fh, Hit::Goto);
            first = crumbs.len();
        }
        if first > 0 && first < crumbs.len() {
            self.regular.draw_in(
                &mut c,
                "…  ›  ",
                cx,
                0.0,
                TOOLBAR_H * s,
                14.0 * s,
                theme::palette().text_dim,
            );
            cx += self.regular.measure("…  ›  ", 14.0 * s);
        }
        let last = crumbs.len().saturating_sub(1);
        for (i, (label, path)) in crumbs.iter().enumerate().skip(first) {
            let color = if i == last {
                theme::palette().text
            } else {
                theme::palette().text_dim
            };
            let font = if i == last { &self.bold } else { &self.regular };
            font.draw_in(&mut c, label, cx, 0.0, TOOLBAR_H * s, 14.0 * s, color);
            let lw = font.measure(label, 14.0 * s);
            hit(
                cx - 4.0 * s,
                mid - 14.0 * s,
                lw + 8.0 * s,
                28.0 * s,
                Hit::Crumb(path.clone()),
            );
            cx += lw;
            if i != last && label != "/" {
                self.regular.draw_in(
                    &mut c,
                    "  ›  ",
                    cx,
                    0.0,
                    TOOLBAR_H * s,
                    14.0 * s,
                    theme::palette().text_dim,
                );
                cx += sep_w;
            }
        }
        if let Some(search) = self.search.as_ref() {
            let fh = 30.0 * s;
            let fx = buttons_left - 8.0 * s - search_w;
            search.field.draw(
                &self.regular,
                &mut c,
                fx,
                mid - fh / 2.0,
                search_w,
                fh,
                s,
                "Rechercher",
                search.focused,
            );
            hit(fx, mid - fh / 2.0, search_w, fh, Hit::SearchField);
        }
        for (i, (glyph, what)) in buttons.into_iter().enumerate() {
            let size = 28.0 * s;
            let tx = right - 16.0 * s - size - i as f32 * (size + 8.0 * s);
            let bg = if self.done.as_ref().is_some_and(|(h, _)| *h == what) {
                theme::palette().success.with_alpha(0.25)
            } else if self.hover.as_ref() == Some(&what) {
                theme::palette().tile_hover()
            } else {
                theme::palette().subtle
            };
            c.fill_rounded(tx, mid - size / 2.0, size, size, 8.0 * s, bg);
            self.icons.icon(
                &mut c,
                glyph,
                tx + size / 2.0,
                mid,
                15.0 * s,
                theme::palette().text,
            );
            hit(tx, mid - size / 2.0, size, size, what);
        }
        c.fill_rounded(
            x0,
            TOOLBAR_H * s - s,
            right - x0,
            s,
            0.0,
            theme::palette().ink(0.06),
        );

        // En-têtes de colonnes
        let list_x = x0 + 16.0 * s;
        let list_w = right - list_x - 16.0 * s;
        let date_x = list_x + list_w - DATE_COL_W * s;
        let size_x = date_x - SIZE_COL_W * s - 16.0 * s;
        let header_y = TOOLBAR_H * s;
        let sort = self.browser.sort;
        // En-têtes : seulement en liste
        if self.view == grid::View::List {
            for (label, x, key, col_w) in [
                (
                    "Nom",
                    list_x + 36.0 * s,
                    SortKey::Name,
                    size_x - list_x - 52.0 * s,
                ),
                ("Taille", size_x, SortKey::Size, SIZE_COL_W * s),
                ("Modifié", date_x, SortKey::Modified, DATE_COL_W * s),
            ] {
                let active = sort.key == key;
                let (font, color) = if active {
                    (&self.bold, theme::palette().text)
                } else {
                    (&self.regular, theme::palette().text_dim)
                };
                font.draw_in(&mut c, label, x, header_y, HEADER_H * s, 12.0 * s, color);
                if active {
                    let lw = font.measure(label, 12.0 * s);
                    let glyph = if sort.descending {
                        icon::CHEVRON_DOWN
                    } else {
                        icon::CHEVRON_UP
                    };
                    self.icons.icon(
                        &mut c,
                        glyph,
                        x + lw + 10.0 * s,
                        header_y + HEADER_H * s / 2.0,
                        12.0 * s,
                        color,
                    );
                }
                hit(x - 6.0 * s, header_y, col_w, HEADER_H * s, Hit::SortBy(key));
            }
        }

        // Liste
        let top = match self.view {
            grid::View::List => header_y + HEADER_H * s,
            grid::View::Icons => header_y + GRID_TOP * s,
        };
        let b = &self.browser;
        if b.entries.is_empty() && b.message.is_none() {
            let msg = match self.search.as_ref() {
                Some(s) if s.busy => "Recherche…",
                Some(s) if !s.field.text.trim().is_empty() => "Aucun résultat",
                _ => "Dossier vide",
            };
            let mw = self.regular.measure(msg, 15.0 * s);
            let list_h = h as f32 - top - STATUS_H * s;
            self.regular.draw_in(
                &mut c,
                msg,
                x0 + (right - x0 - mw) / 2.0,
                top,
                list_h,
                15.0 * s,
                theme::palette().text_dim,
            );
        }
        let mut name_ends = Vec::new();
        let mut item_rects = Vec::new();
        let mut missing = Vec::new();
        if self.view == grid::View::Icons {
            let tiles = grid::Tiles {
                regular: &self.regular,
                bold: &self.bold,
                browser: b,
                thumbs: &self.tiles,
                rename: self.rename.as_ref(),
                drop_target: self.drop_target.as_ref(),
            };
            let bottom = h as f32 - STATUS_H * s;
            (item_rects, missing) = tiles.draw(&mut c, x0, top, right, bottom, s, &mut hit);
            // Toute la tuile se saisit (glisser), pas seulement le nom
            name_ends.extend(item_rects.iter().map(|(i, _)| (*i, f64::INFINITY)));
        } else {
            for (row, idx) in (b.scroll..b.entries.len().min(b.scroll + rows)).enumerate() {
                let e = &b.entries[idx];
                let ry = top + row as f32 * ROW_H * s;
                let rh = ROW_H * s;
                if e.is_dir && self.drop_target.as_ref() == Some(&e.path) {
                    let a = theme::palette().accent.with_alpha(0.45);
                    c.fill_rounded(x0 + 8.0 * s, ry, right - x0 - 16.0 * s, rh, 8.0 * s, a);
                } else if b.is_picked(idx) {
                    let mut sel = theme::palette().accent;
                    sel.a = 0.25;
                    c.fill_rounded(x0 + 8.0 * s, ry, right - x0 - 16.0 * s, rh, 8.0 * s, sel);
                }
                if e.is_dir {
                    draw_folder_icon(
                        &mut c,
                        list_x + 6.0 * s,
                        ry + 7.0 * s,
                        16.0 * s,
                        theme::palette().accent,
                    );
                } else {
                    draw_file_icon(
                        &mut c,
                        list_x + 8.0 * s,
                        ry + 6.0 * s,
                        13.0 * s,
                        theme::palette().text_dim,
                    );
                }
                let name_max = size_x - (list_x + 36.0 * s) - 16.0 * s;
                let name = self.regular.truncate(&e.name, 14.0 * s, name_max);
                let renaming = self.rename.as_ref().filter(|r| r.path == e.path);
                if let Some(r) = renaming {
                    Files::draw_rename_field(
                        &self.regular,
                        r,
                        &mut c,
                        list_x + 36.0 * s,
                        ry,
                        name_max,
                        rh,
                        s,
                    );
                } else {
                    self.regular.draw_in(
                        &mut c,
                        &name,
                        list_x + 36.0 * s,
                        ry,
                        rh,
                        14.0 * s,
                        theme::palette().text,
                    );
                }
                let name_end = if renaming.is_some() {
                    list_x + 36.0 * s + name_max
                } else {
                    list_x + 36.0 * s + self.regular.measure(&name, 14.0 * s)
                };
                // Recherche dans les sous-dossiers : où se trouve l'élément
                if let Some(place) = e.place.as_ref().filter(|_| renaming.is_none()) {
                    let label = if place.is_empty() {
                        "ici"
                    } else {
                        place.as_str()
                    };
                    let room = size_x - 16.0 * s - (name_end + 12.0 * s);
                    if room > 40.0 * s {
                        let text = self.regular.truncate(label, 12.0 * s, room);
                        self.regular.draw_in(
                            &mut c,
                            &text,
                            name_end + 12.0 * s,
                            ry,
                            rh,
                            12.0 * s,
                            theme::palette().text_dim,
                        );
                    }
                }
                name_ends.push((idx, (name_end / s) as f64));
                let size = if e.is_dir {
                    "—".to_string()
                } else {
                    format_size(e.size)
                };
                self.regular.draw_in(
                    &mut c,
                    &size,
                    size_x,
                    ry,
                    rh,
                    13.0 * s,
                    theme::palette().text_dim,
                );
                self.regular.draw_in(
                    &mut c,
                    &format_date(e.modified),
                    date_x,
                    ry,
                    rh,
                    13.0 * s,
                    theme::palette().text_dim,
                );
                hit(x0 + 8.0 * s, ry, right - x0 - 16.0 * s, rh, Hit::Row(idx));
                item_rects.push((
                    idx,
                    (
                        (x0 / s + 8.0) as f64,
                        (ry / s) as f64,
                        ((right - x0) / s - 16.0) as f64,
                        ROW_H as f64,
                    ),
                ));
            }
        }

        // Barre d'état
        let sy = h as f32 - STATUS_H * s;
        c.fill_rounded(x0, sy, right - x0, s, 0.0, theme::palette().ink(0.06));
        let status = match &b.message {
            Some(m) => m.clone(),
            None if self.search.is_some() && !b.multiple() => {
                let s = self.search.as_ref().unwrap();
                let n = b.entries.len();
                let what = if s.field.text.trim().is_empty() {
                    String::new()
                } else {
                    format!("{n} résultat{}", plural(n))
                };
                if s.busy {
                    format!("{what} · recherche…")
                } else if s.capped {
                    format!("{what} · arrêtée là, précisez la recherche")
                } else {
                    what
                }
            }
            None if b.multiple() => {
                let sel = b.selection();
                let bytes: u64 = sel.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
                format!(
                    "{} éléments sélectionnés sur {} · {} · Échap pour désélectionner",
                    sel.len(),
                    b.entries.len(),
                    format_size(bytes)
                )
            }
            None => {
                let dirs = b.entries.iter().filter(|e| e.is_dir).count();
                let files = b.entries.len() - dirs;
                let hidden = if b.show_hidden {
                    " · cachés visibles"
                } else {
                    ""
                };
                format!(
                    "{dirs} dossier{} · {files} fichier{}{hidden}",
                    plural(dirs),
                    plural(files)
                )
            }
        };
        let color = if b.message.is_some() {
            theme::palette().urgent
        } else {
            theme::palette().text_dim
        };
        self.regular
            .draw_in(&mut c, &status, list_x, sy, STATUS_H * s, 12.0 * s, color);
        // Portée de la recherche, à droite de la barre d'état
        if let Some(search) = self.search.as_ref() {
            let mut sx = right - 16.0 * s;
            for (label, deep) in [("Sous-dossiers", true), ("Ce dossier", false)] {
                let lw = self.regular.measure(label, 12.0 * s) + 20.0 * s;
                sx -= lw;
                let ch = 20.0 * s;
                let cy = sy + (STATUS_H * s - ch) / 2.0;
                let active = search.deep == deep;
                if active {
                    c.fill_rounded(
                        sx,
                        cy,
                        lw,
                        ch,
                        ch / 2.0,
                        theme::palette().accent.with_alpha(0.25),
                    );
                }
                let color = if active {
                    theme::palette().text
                } else {
                    theme::palette().text_dim
                };
                self.regular
                    .draw_in(&mut c, label, sx + 10.0 * s, cy, ch, 12.0 * s, color);
                hit(sx, cy, lw, ch, Hit::Scope(deep));
                sx -= 6.0 * s;
            }
        }

        if let Some(d) = self.details.as_ref().filter(|_| info_px > 0.0) {
            let fonts = Fonts {
                regular: &self.regular,
                bold: &self.bold,
                icons: &self.icons,
                hover: self.hover.as_ref(),
                done: self
                    .done
                    .as_ref()
                    .filter(|(_, at)| at.elapsed() < CONFIRM_TIME)
                    .map(|(h, _)| h),
            };
            let in_trash = common::trash::files_dir().is_some_and(|t| self.browser.cwd == t);
            let multi = self.browser.multiple().then(|| {
                let sel = self.browser.selection();
                Multi {
                    count: sel.len(),
                    dirs: sel.iter().filter(|e| e.is_dir).count(),
                    bytes: sel.iter().filter(|e| !e.is_dir).map(|e| e.size).sum(),
                    archives: sel
                        .iter()
                        .filter(|e| archive::kind(&e.path).is_some())
                        .count(),
                    place: self.browser.cwd.to_string_lossy().into_owned(),
                }
            });
            draw_info(
                &fonts,
                d,
                multi.as_ref(),
                in_trash,
                &mut c,
                right,
                h as f32,
                s,
                &mut hit,
            );
        }

        // Dépôt dans le dossier affiché : la liste entière est entourée
        if self.drop_target.as_ref() == Some(&self.browser.cwd) {
            let a = theme::palette().accent;
            c.stroke_rounded(
                x0 + 6.0 * s,
                top,
                right - x0 - 12.0 * s,
                h as f32 - top - STATUS_H * s - 4.0 * s,
                10.0 * s,
                2.0 * s,
                a,
            );
        }
        self.name_ends = name_ends;
        self.item_rects = item_rects;
        self.tiles.want(missing);
        self.rows_top = (top / s) as f64;
        if let Some(band) = self.band.as_ref() {
            Files::draw_band(band, &mut c, s);
        }
        if let Some(menu) = self.menu.as_ref() {
            Files::draw_menu(&self.regular, menu, &mut c, s);
        }

        self.hits = hits;
        let surface = self.window.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.window.commit();
        }
    }
}

/// Polices du panneau d'informations
struct Fonts<'a> {
    regular: &'a Text,
    bold: &'a Text,
    icons: &'a Text,
    /// Bouton survolé, et bouton dont l'action vient d'être faite
    hover: Option<&'a Hit>,
    done: Option<&'a Hit>,
}

/// Résumé d'une sélection multiple, pour le panneau d'informations
struct Multi {
    count: usize,
    dirs: usize,
    /// Taille cumulée des fichiers (les dossiers ne sont pas parcourus)
    bytes: u64,
    archives: usize,
    place: String,
}

/// Panneau d'informations : aperçu, détails, actions rapides.
#[allow(clippy::too_many_arguments)]
fn draw_info(
    fonts: &Fonts,
    d: &Details,
    multi: Option<&Multi>,
    in_trash: bool,
    c: &mut Canvas,
    x0: f32,
    h: f32,
    s: f32,
    hit: &mut impl FnMut(f32, f32, f32, f32, Hit),
) {
    let p = theme::palette();
    let w = INFO_W * s;
    let pad = 16.0 * s;
    let inner = w - 2.0 * pad;
    let x = x0 + pad;
    c.fill_rounded(x0, 0.0, w, h, 0.0, p.sidebar);
    c.fill_rounded(x0, 0.0, s, h, 0.0, p.ink(0.06));

    // En-tête : titre et fermeture
    let mid = TOOLBAR_H * s / 2.0;
    fonts
        .bold
        .draw_in(c, "Informations", x, 0.0, TOOLBAR_H * s, 14.0 * s, p.text);
    let cx = x0 + w - pad - 10.0 * s;
    let close_color = if fonts.hover == Some(&Hit::CloseInfo) {
        c.fill_circle(cx, mid, 14.0 * s, p.tile_hover());
        p.text
    } else {
        p.text_dim
    };
    fonts
        .icons
        .icon(c, icon::CLOSE, cx, mid, 16.0 * s, close_color);
    hit(
        cx - 16.0 * s,
        mid - 16.0 * s,
        32.0 * s,
        32.0 * s,
        Hit::CloseInfo,
    );
    c.fill_rounded(x0, TOOLBAR_H * s - s, w, s, 0.0, p.ink(0.06));

    let mut y = TOOLBAR_H * s + pad;
    if let Some(m) = multi {
        return draw_multi_info(fonts, m, in_trash, c, x, y, inner, h, s, hit);
    }
    // Aperçu : la vignette d'une image, sinon une grande icône
    let box_h = 168.0 * s;
    c.fill_rounded(x, y, inner, box_h, 10.0 * s, p.ink(0.04));
    match d.thumbnail.as_ref() {
        Some(img) => {
            let k = (inner / img.width as f32).min(box_h / img.height as f32);
            let (iw, ih) = (img.width as f32 * k, img.height as f32 * k);
            c.image(
                img,
                x + (inner - iw) / 2.0,
                y + (box_h - ih) / 2.0,
                iw,
                ih,
                6.0 * s,
            );
        }
        None => {
            let size = 56.0 * s;
            let (ix, iy) = (x + (inner - size) / 2.0, y + (box_h - size * 1.2) / 2.0);
            if d.is_dir {
                draw_folder_icon(c, ix, iy, size, p.accent);
            } else {
                draw_file_icon(c, ix + size * 0.12, iy, size * 0.76, p.text_dim);
            }
        }
    }
    y += box_h + 14.0 * s;

    // Nom (sur plusieurs lignes) et nature
    for line in wrap_chars(fonts.bold, &d.name, 15.0 * s, inner, 3) {
        fonts
            .bold
            .draw_in(c, &line, x, y, 20.0 * s, 15.0 * s, p.text);
        y += 20.0 * s;
    }
    let summary = format!("{} · {}", d.kind, d.size);
    fonts
        .regular
        .draw_in(c, &summary, x, y + 2.0 * s, 18.0 * s, 12.5 * s, p.text_dim);
    y += 30.0 * s;
    c.fill_rounded(x, y, inner, s, 0.0, p.ink(0.06));
    y += 10.0 * s;

    // Détails : étiquette à gauche, valeur à droite (sur plusieurs lignes)
    let label_w = 92.0 * s;
    // Dates coupées entre les mots, chemins au caractère près
    let mut row = |label: &str, value: &str, lines: usize, y: &mut f32| {
        fonts
            .regular
            .draw_in(c, label, x, *y, 20.0 * s, 12.0 * s, p.text_dim);
        let value_w = inner - label_w;
        let wrapped = if value.contains(' ') {
            fonts.regular.wrap(value, 12.5 * s, value_w, lines)
        } else {
            wrap_chars(fonts.regular, value, 12.5 * s, value_w, lines)
        };
        for line in wrapped {
            fonts
                .regular
                .draw_in(c, &line, x + label_w, *y, 20.0 * s, 12.5 * s, p.text);
            *y += 18.0 * s;
        }
        *y += 6.0 * s;
    };
    if let Some((iw, ih)) = d.dimensions {
        row("Dimensions", &format!("{iw} × {ih} pixels"), 1, &mut y);
    }
    row("Modifié", &d.modified, 2, &mut y);
    if let Some(created) = d.created.as_ref() {
        row("Créé", created, 2, &mut y);
    }
    let place = d
        .path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    row("Emplacement", &place, 3, &mut y);
    y += 6.0 * s;

    // Actions rapides
    // Selon la nature de l'élément : archive, dossier, image…
    let mut actions = Vec::new();
    if archive::kind(&d.path).is_some() {
        actions.push((icon::PACKAGE_OPEN, "Décompresser ici", Hit::Extract, p.text));
    } else {
        actions.push((icon::EXTERNAL_LINK, "Ouvrir", Hit::Open, p.text));
    }
    if d.is_dir {
        actions.push((
            icon::TERMINAL,
            "Terminal dans ce dossier",
            Hit::Terminal(d.path.clone()),
            p.text,
        ));
    }
    if !in_trash {
        actions.push((icon::PENCIL, "Renommer", Hit::Rename, p.text));
    }
    actions.push((icon::COPY, "Copier le chemin", Hit::CopyPath, p.text));
    actions.push((icon::BOT, "Demander à l'Assistant", Hit::AskAgent, p.text));
    if info::is_image(&d.path) {
        actions.push((icon::IMAGE, "Copier l'image", Hit::CopyImage, p.text));
    }
    actions.push(if in_trash {
        (icon::UNDO, "Remettre à sa place", Hit::Trash, p.text)
    } else {
        (icon::TRASH, "Mettre à la corbeille", Hit::Trash, p.danger)
    });
    let bh = 32.0 * s;
    for (glyph, label, what, color) in actions {
        if y + bh > h - 48.0 * s {
            break;
        }
        // Fait : vert avec une coche et le libellé de confirmation ; survolé : plus clair
        let done = fonts.done == Some(&what);
        let (bg, glyph, label, color) = if done {
            let label = match what {
                Hit::CopyPath => "Chemin copié",
                Hit::CopyImage => "Image copiée",
                Hit::Extract => "Décompression…",
                Hit::Terminal(_) => "Terminal ouvert",
                Hit::AskAgent => "Assistant ouvert",
                _ => "Ouverture…",
            };
            (p.success.with_alpha(0.22), icon::CHECK, label, p.success)
        } else if fonts.hover == Some(&what) {
            (p.tile_hover(), glyph, label, color)
        } else {
            (p.tile(), glyph, label, color)
        };
        c.fill_rounded(x, y, inner, bh, 8.0 * s, bg);
        fonts
            .icons
            .icon(c, glyph, x + 18.0 * s, y + bh / 2.0, 14.0 * s, color);
        let label = fonts.regular.truncate(label, 13.0 * s, inner - 44.0 * s);
        fonts
            .regular
            .draw_in(c, &label, x + 36.0 * s, y, bh, 13.0 * s, color);
        hit(x, y, inner, bh, what);
        y += bh + 6.0 * s;
    }

    // Astuce, en bas
    let tip = "Glissez l'élément vers une autre fenêtre, ou sur un dossier pour l'y ranger.";
    let lines = fonts.regular.wrap(tip, 11.5 * s, inner, 3);
    let mut ty = h - pad - lines.len() as f32 * 16.0 * s;
    if ty > y {
        for line in lines {
            fonts
                .regular
                .draw_in(c, &line, x, ty, 16.0 * s, 11.5 * s, p.text_dim);
            ty += 16.0 * s;
        }
    }
}

/// Panneau d'une sélection multiple : pile d'icônes, résumé, actions par lot
#[allow(clippy::too_many_arguments)]
fn draw_multi_info(
    fonts: &Fonts,
    m: &Multi,
    in_trash: bool,
    c: &mut Canvas,
    x: f32,
    mut y: f32,
    inner: f32,
    h: f32,
    s: f32,
    hit: &mut impl FnMut(f32, f32, f32, f32, Hit),
) {
    let p = theme::palette();
    let box_h = 168.0 * s;
    c.fill_rounded(x, y, inner, box_h, 10.0 * s, p.ink(0.04));
    // Trois fiches décalées : « plusieurs »
    let size = 46.0 * s;
    let (cx, cy) = (x + inner / 2.0, y + box_h / 2.0);
    for k in 0..3 {
        let off = (k as f32 - 1.0) * 14.0 * s;
        let (ix, iy) = (cx - size / 2.0 + off, cy - size * 0.6 + off * 0.4);
        if k < m.dirs.min(3) {
            draw_folder_icon(c, ix, iy, size, p.accent);
        } else {
            draw_file_icon(c, ix + size * 0.12, iy, size * 0.76, p.text_dim);
        }
    }
    y += box_h + 14.0 * s;
    let title = format!("{} éléments", m.count);
    fonts
        .bold
        .draw_in(c, &title, x, y, 20.0 * s, 15.0 * s, p.text);
    y += 20.0 * s;
    let files = m.count - m.dirs;
    let mut parts = Vec::new();
    if m.dirs > 0 {
        parts.push(format!("{} dossier{}", m.dirs, plural(m.dirs)));
    }
    if files > 0 {
        parts.push(format!(
            "{files} fichier{} · {}",
            plural(files),
            format_size(m.bytes)
        ));
    }
    fonts.regular.draw_in(
        c,
        &parts.join(" · "),
        x,
        y + 2.0 * s,
        18.0 * s,
        12.5 * s,
        p.text_dim,
    );
    y += 30.0 * s;
    c.fill_rounded(x, y, inner, s, 0.0, p.ink(0.06));
    y += 10.0 * s;
    fonts
        .regular
        .draw_in(c, "Emplacement", x, y, 20.0 * s, 12.0 * s, p.text_dim);
    let label_w = 92.0 * s;
    for line in wrap_chars(fonts.regular, &m.place, 12.5 * s, inner - label_w, 3) {
        fonts
            .regular
            .draw_in(c, &line, x + label_w, y, 20.0 * s, 12.5 * s, p.text);
        y += 18.0 * s;
    }
    y += 12.0 * s;

    let mut actions = Vec::new();
    if files > m.archives {
        actions.push((
            icon::EXTERNAL_LINK,
            "Ouvrir les fichiers",
            Hit::Open,
            p.text,
        ));
    }
    if m.archives > 0 {
        actions.push((
            icon::PACKAGE_OPEN,
            "Décompresser les archives",
            Hit::Extract,
            p.text,
        ));
    }
    actions.push((icon::COPY, "Copier les chemins", Hit::CopyPath, p.text));
    actions.push((icon::BOT, "Demander à l'Assistant", Hit::AskAgent, p.text));
    actions.push(if in_trash {
        (icon::UNDO, "Tout remettre à sa place", Hit::Trash, p.text)
    } else {
        (
            icon::TRASH,
            "Tout mettre à la corbeille",
            Hit::Trash,
            p.danger,
        )
    });
    let bh = 32.0 * s;
    for (glyph, label, what, color) in actions {
        if y + bh > h - 48.0 * s {
            break;
        }
        let done = fonts.done == Some(&what);
        let (bg, glyph, label, color) = if done {
            let label = match what {
                Hit::CopyPath => "Chemins copiés",
                Hit::Extract => "Décompression…",
                Hit::AskAgent => "Assistant ouvert",
                _ => "Ouverture…",
            };
            (p.success.with_alpha(0.22), icon::CHECK, label, p.success)
        } else if fonts.hover == Some(&what) {
            (p.tile_hover(), glyph, label, color)
        } else {
            (p.tile(), glyph, label, color)
        };
        c.fill_rounded(x, y, inner, bh, 8.0 * s, bg);
        fonts
            .icons
            .icon(c, glyph, x + 18.0 * s, y + bh / 2.0, 14.0 * s, color);
        fonts
            .regular
            .draw_in(c, label, x + 36.0 * s, y, bh, 13.0 * s, color);
        hit(x, y, inner, bh, what);
        y += bh + 6.0 * s;
    }

    let tip = "Ctrl+clic ajoute ou retire un élément, Maj+clic prend une plage, Ctrl+A tout. Glissez la sélection sur un dossier pour l'y ranger.";
    let lines = fonts.regular.wrap(tip, 11.5 * s, inner, 4);
    let mut ty = h - 16.0 * s - lines.len() as f32 * 16.0 * s;
    if ty > y {
        for line in lines {
            fonts
                .regular
                .draw_in(c, &line, x, ty, 16.0 * s, 11.5 * s, p.text_dim);
            ty += 16.0 * s;
        }
    }
}

/// Met du texte (arguments) ou un fichier (entrée) dans le presse-papiers.
fn wl_copy(args: &[&str], input: Option<&Path>) -> Result<(), String> {
    let stdin = match input {
        Some(path) => std::fs::File::open(path)
            .map(std::process::Stdio::from)
            .map_err(|e| format!("Lecture impossible : {e}"))?,
        None => std::process::Stdio::null(),
    };
    let mut child = std::process::Command::new("wl-copy")
        .args(args)
        .stdin(stdin)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Presse-papiers indisponible (wl-copy) : {e}"))?;
    // wl-copy rend la main une fois les données prises : attente dans un fil
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Coupe un texte sans espaces (chemin, nom de fichier) au caractère près ;
/// la dernière ligne se termine par « … » si tout ne tient pas.
fn wrap_chars(font: &Text, text: &str, px: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if lines.len() + 1 == max_lines {
            let rest: String = chars[start..].iter().collect();
            lines.push(font.truncate(&rest, px, max_w));
            break;
        }
        let mut end = start + 1;
        while end < chars.len()
            && font.measure(&chars[start..=end].iter().collect::<String>(), px) <= max_w
        {
            end += 1;
        }
        lines.push(chars[start..end].iter().collect());
        start = end;
    }
    lines
}

fn plural(n: usize) -> &'static str {
    if n > 1 { "s" } else { "" }
}

/// Dossier : onglet + corps arrondis.
pub fn draw_folder_icon(c: &mut Canvas, x: f32, y: f32, size: f32, color: Rgba) {
    let mut tab = color;
    tab.a *= 0.75;
    c.fill_rounded(x, y, size * 0.5, size * 0.3, size * 0.1, tab);
    c.fill_rounded(x, y + size * 0.15, size, size * 0.7, size * 0.14, color);
}

/// Fichier : feuille arrondie avec deux lignes de « texte ».
pub fn draw_file_icon(c: &mut Canvas, x: f32, y: f32, size: f32, color: Rgba) {
    let h = size * 1.3;
    c.fill_rounded(x, y, size, h, size * 0.18, color);
    let line = theme::palette().content.with_alpha(0.6);
    for k in 0..2 {
        c.fill_rounded(
            x + size * 0.22,
            y + h * (0.4 + 0.2 * k as f32),
            size * 0.56,
            size * 0.08,
            size * 0.04,
            line,
        );
    }
}

impl CompositorHandler for Files {
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

impl WindowHandler for Files {
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
        self.chrome.configure(
            configure.is_activated(),
            configure.is_maximized(),
            configure.is_fullscreen(),
        );
        self.configured = true;
        self.follow_selection();
        self.draw();
    }
}

impl SeatHandler for Files {
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
        if self.data_device.is_none()
            && let Some(manager) = self.data_manager.as_ref()
        {
            self.data_device = Some(manager.get_data_device(qh, &seat));
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|files, _, event| files.key(event)),
            );
            if let Ok(k) = keyboard {
                self.keyboard = Some(k);
            }
        }
        if capability == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(p) = self.seat_state.get_pointer(qh, &seat)
        {
            self.pointer = Some(p);
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

impl KeyboardHandler for Files {
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
        serial: u32,
        event: KeyEvent,
    ) {
        self.serial = serial;
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

impl PointerHandler for Files {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } => {
                    self.serial = serial;
                    // Menu ouvert : choisir une ligne, ou le fermer
                    if let Some(menu) = self.menu.as_ref() {
                        let item = menu.item_at(x, y).map(|i| menu.items[i].1);
                        let inside = menu.contains(x, y);
                        self.menu = None;
                        match item {
                            Some(item) => self.menu_action(item),
                            None => self.draw(),
                        }
                        if inside {
                            continue;
                        }
                    }
                    // Renommage en cours : un clic ailleurs le valide ; un chemin
                    // en cours de saisie est abandonné
                    if self.rename.is_some() {
                        self.commit_rename();
                    }
                    let hit_now = self.hit_at(x, y);
                    if let Some(s) = self.search.as_mut() {
                        s.focused = hit_now == Some(Hit::SearchField);
                    }
                    if self.goto.is_some() && hit_now != Some(Hit::Goto) {
                        self.goto = None;
                        self.draw();
                    }
                    if let Some(b) = self.chrome.press(x as f32, y as f32, CHROME_Y) {
                        self.chrome_action(b);
                        continue;
                    }
                    let hit = self.hit_at(x, y);
                    // Hors du nom (vide, colonnes taille et date) : zone de sélection
                    let on_name = |i: usize| {
                        self.name_ends
                            .iter()
                            .find(|(r, _)| *r == i)
                            .is_some_and(|(_, end)| x <= end + 8.0)
                    };
                    let in_list = y >= self.rows_top
                        && y < (self.height as f32 - STATUS_H) as f64
                        && x > SIDEBAR_W as f64;
                    match hit {
                        Some(Hit::Row(i)) if !on_name(i) => {
                            if !self.modifiers.ctrl {
                                self.browser.select(i);
                            }
                            self.start_band(x, y);
                            self.draw();
                        }
                        None if in_list => self.start_band(x, y),
                        _ => {
                            // Un appui sur le nom d'une ligne peut devenir un glisser
                            self.press = match hit {
                                Some(Hit::Row(i)) => Some((x, y, i, serial)),
                                _ => None,
                            };
                            self.click(x, y, serial);
                        }
                    }
                }
                PointerEventKind::Press {
                    button: BTN_RIGHT,
                    serial,
                    ..
                } => {
                    self.serial = serial;
                    if self.rename.is_some() {
                        self.commit_rename();
                    }
                    match self.hit_at(x, y) {
                        Some(Hit::Row(i)) => {
                            // L'élément cliqué rejoint la sélection (ou la remplace)
                            if !self.browser.is_picked(i) {
                                self.browser.select(i);
                            }
                            self.open_menu(x, y, true);
                        }
                        None if y >= self.rows_top && x > SIDEBAR_W as f64 => {
                            self.open_menu(x, y, false)
                        }
                        _ => {}
                    }
                }
                PointerEventKind::Motion { .. } => {
                    if self.chrome.motion(x as f32, y as f32, CHROME_Y) {
                        self.draw();
                    }
                    if self.band.is_some() {
                        self.move_band(x, y);
                        continue;
                    }
                    if let Some(menu) = self.menu.as_mut() {
                        let hover = menu.item_at(x, y);
                        if hover != menu.hover {
                            menu.hover = hover;
                            self.draw();
                        }
                        continue;
                    }
                    self.pointer_moved(x, y);
                    self.update_hover(x, y);
                }
                PointerEventKind::Leave { .. } => {
                    let chrome = self.chrome.leave();
                    if self.hover.take().is_some() || chrome {
                        self.draw();
                    }
                }
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => {
                    if self.end_band() {
                        self.draw();
                    }
                    if self.press.take().is_some()
                        && let Some(i) = self.collapse.take()
                    {
                        self.browser.select(i);
                        self.draw();
                    }
                    self.collapse = None;
                }
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    // Par rangées : trois lignes de la liste, une rangée de tuiles
                    let cols = self.grid_cols();
                    let rows = self.browser.entries.len().div_ceil(cols);
                    let max = rows.saturating_sub(self.visible_rows()) * cols;
                    let unit = if cols > 1 { cols } else { 3 } as isize;
                    let step = if vertical.absolute > 0.0 { unit } else { -unit };
                    self.browser.scroll =
                        (self.browser.scroll as isize + step).clamp(0, max as isize) as usize;
                    self.draw();
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Files {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Files {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Files {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Files);
delegate_output!(Files);
delegate_seat!(Files);
delegate_keyboard!(Files);
delegate_pointer!(Files);
delegate_shm!(Files);
delegate_xdg_shell!(Files);
delegate_xdg_window!(Files);
delegate_registry!(Files);
