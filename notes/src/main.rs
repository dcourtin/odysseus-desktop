//! Notes : dossiers à gauche, notes du dossier au milieu, la note à droite.
//! Le texte est du Markdown léger mis en forme à l'écran (titres, gras,
//! italique, barré, code, listes, cases à cocher), rangé dans une base SQLite.
//!
//! Clavier : Ctrl+N (nouvelle note), Ctrl+Maj+N (nouveau dossier), Ctrl+F
//! (rechercher), Alt+↑/↓ (note précédente, suivante), Ctrl+Q (quitter).
//! Dans la note : Ctrl+B gras, Ctrl+I italique, Ctrl+E code, Ctrl+Maj+X barré,
//! Ctrl+Maj+H intertitre, Ctrl+Maj+L liste, Ctrl+Maj+C cases à cocher,
//! Tab / Maj+Tab (indenter une liste), Ctrl+Z / Ctrl+Maj+Z (annuler, rétablir),
//! Ctrl+C / X / V (presse-papiers), Ctrl+A (tout sélectionner).
//! Souris : clic, glisser pour sélectionner, double-clic (mot), triple-clic
//! (paragraphe), clic sur une case pour la cocher, double-clic sur un
//! dossier pour le renommer.

use std::{
    io::Write,
    os::{fd::AsFd, unix::process::CommandExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use chrono::{DateTime, Datelike, Local, Timelike};
use common::{
    draw::{Canvas, Text},
    theme::{self, icon},
};
use editor::{Block, Editor, Format, Layout, Metrics, Sizes, Style};
use notes::{editor, store};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
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
use store::{Folder, Store, Summary, View};

// Mise en page, en pixels logiques
const SIDEBAR_W: f32 = 216.0;
const LIST_W: f32 = 300.0;
const TOOLBAR_H: f32 = 52.0;
/// Centre vertical des boutons de la fenêtre, à hauteur de la barre d'outils
const CHROME_Y: f32 = TOOLBAR_H / 2.0;
const ROW_H: f32 = 32.0;
const NOTE_H: f32 = 64.0;
/// Sous le bandeau de la liste : le nom de la vue
const SUB_H: f32 = 28.0;
/// Colonne de texte : largeur au plus, marges au moins
const TEXT_MAX: f32 = 720.0;
const TEXT_PAD: f32 = 40.0;
/// Place de la date, au-dessus du texte
const DATE_H: f32 = 44.0;
const BTN_LEFT: u32 = 0x110;
/// Enregistrement automatique, après un temps sans frappe
const SAVE_AFTER: Duration = Duration::from_millis(800);
const BLINK: Duration = Duration::from_millis(530);
/// Confirmation des suppressions : second clic dans ce délai
const ARMED: Duration = Duration::from_secs(4);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    View(View),
    NewFolder,
    DeleteFolder(i64),
    Note(i64),
    NewNote,
    Search,
    ClearSearch,
    Format(Format),
    MoveMenu,
    MoveTo(i64),
    TrashNote,
    Restore,
    Purge,
    EmptyTrash,
    /// Case à cocher du paragraphe qui commence à cet octet
    Check(usize),
    Text,
}

type Hits = Vec<((f64, f64, f64, f64), Hit)>;

enum Focus {
    Editor,
    Search,
    /// Nom d'un dossier en cours de saisie (nouveau ou renommé)
    Rename(i64, String),
}

/// Polices du texte des notes
struct Fonts {
    regular: Text,
    bold: Text,
    italic: Text,
    bold_italic: Text,
    mono: Text,
}

impl Fonts {
    fn pick(&self, st: &Style, block: Block) -> &Text {
        let bold = st.bold || matches!(block, Block::Title | Block::Heading(_));
        let code = st.code || matches!(block, Block::Code | Block::Fence);
        match (code, bold, st.italic) {
            (true, ..) => &self.mono,
            (_, true, true) => &self.bold_italic,
            (_, true, false) => &self.bold,
            (_, false, true) => &self.italic,
            _ => &self.regular,
        }
    }
}

impl Metrics for Fonts {
    fn advance(&self, st: &Style, block: Block, ch: char, px: f32) -> f32 {
        self.pick(st, block).advance(ch, px)
    }
}

struct Notes {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    /// Sorti le temps d'un dessin (les fonctions de dessin empruntent l'appli)
    pool: Option<SlotPool>,
    window: Window,
    loop_handle: LoopHandle<'static, Notes>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    modifiers: Modifiers,
    ui: Text,
    ui_bold: Text,
    icons: Text,
    fonts: Fonts,

    store: Store,
    /// Version de la base vue au dernier passage : un écart signale une
    /// écriture venue d'ailleurs (un agent par odysseus-mcp)
    db_version: i64,
    folders: Vec<Folder>,
    /// Nombre de notes : toutes, dans la corbeille
    counts: (usize, usize),
    view: View,
    search: String,
    list: Vec<Summary>,
    list_scroll: f32,
    current: Option<i64>,
    editor: Editor,
    scroll: f32,
    /// Faire défiler jusqu'au curseur au prochain dessin
    follow: bool,
    focus: Focus,
    edited_at: Instant,
    caret_on: bool,
    /// Menu « déplacer vers » ouvert
    menu: bool,
    armed: Option<(Hit, Instant)>,
    message: Option<String>,

    /// Dernière mise en page de la note, et origine du texte (pixels du tampon)
    layout: Option<Layout>,
    origin: (f32, f32),
    hits: Hits,
    hover: Option<Hit>,
    /// Sélection à la souris en cours ; clics rapprochés (double, triple)
    dragging: bool,
    clicks: (Instant, u32, (f64, f64)),
    last_folder_click: Option<(i64, Instant)>,
    /// Boutons de la fenêtre, et dernier clic dans le bandeau (double-clic : agrandir)
    chrome: common::chrome::Chrome,
    last_toolbar_click: Option<Instant>,

    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

fn main() {
    // Posée sur le flou du compositeur : palette translucide si le flou est actif
    common::theme::use_vibrancy();
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("odysseus-notes : base de notes illisible ({e})");
            std::process::exit(1);
        }
    };
    let new_note = std::env::args().any(|a| a == "--nouvelle");

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_app_id("odysseus-notes");
    window.set_title("Notes");
    window.set_min_size(Some((760, 420)));
    window.commit();

    let mut event_loop: EventLoop<'static, Notes> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    if let Some(fd) = theme::watch() {
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, notes| {
                theme::drain(fd.as_fd());
                if theme::reload() {
                    notes.draw();
                }
                Ok(PostAction::Continue)
            },
        );
    }
    // Clignotement du curseur et enregistrement automatique
    event_loop
        .handle()
        .insert_source(Timer::from_duration(BLINK), |_, _, notes| {
            notes.tick();
            TimeoutAction::ToDuration(BLINK)
        })
        .expect("minuterie");

    let load = |p: &str| Text::load(p).expect("police");
    let mut notes = Notes {
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
        ui: load(theme::FONT_REGULAR),
        ui_bold: load(theme::FONT_BOLD),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        fonts: Fonts {
            regular: load("Inter-Regular.ttf"),
            bold: load("Inter-SemiBold.ttf"),
            italic: load("Inter-Italic.ttf"),
            bold_italic: load("Inter-SemiBoldItalic.ttf"),
            mono: load("JetBrainsMono-Regular.ttf"),
        },
        db_version: store.data_version(),
        store,
        folders: Vec::new(),
        counts: (0, 0),
        view: View::All,
        search: String::new(),
        list: Vec::new(),
        list_scroll: 0.0,
        current: None,
        editor: Editor::new(String::new()),
        scroll: 0.0,
        follow: false,
        focus: Focus::Editor,
        edited_at: Instant::now(),
        caret_on: true,
        menu: false,
        armed: None,
        message: None,
        layout: None,
        origin: (0.0, 0.0),
        hits: Vec::new(),
        hover: None,
        dragging: false,
        clicks: (Instant::now(), 0, (0.0, 0.0)),
        last_folder_click: None,
        chrome: common::chrome::Chrome::default(),
        last_toolbar_click: None,
        width: 1180,
        height: 740,
        scale: 1,
        configured: false,
        exit: false,
    };
    notes.reload();
    if new_note {
        notes.new_note();
    } else if let Some(first) = notes.list.first().map(|n| n.id) {
        notes.open(first);
    }

    while !notes.exit {
        if let Err(e) = event_loop.dispatch(None, &mut notes) {
            eprintln!("odysseus-notes : connexion au compositeur perdue ({e})");
            break;
        }
    }
    notes.leave_note();
}

// --- Dates -------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

fn local(t: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&Local))
}

/// Date courte de la liste : l'heure aujourd'hui, « hier », le jour sinon
fn short_date(t: i64) -> String {
    let Some(d) = local(t) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    match (today - d.date_naive()).num_days() {
        0 => format!("{}:{:02}", d.hour(), d.minute()),
        1 => "hier".into(),
        _ if d.year() == today.year() => format!("{} {}", d.day(), MONTHS[d.month0() as usize]),
        _ => format!("{:02}/{:02}/{}", d.day(), d.month(), d.year()),
    }
}

/// Date complète au-dessus de la note
fn long_date(t: i64) -> String {
    local(t).map_or_else(String::new, |d| {
        format!(
            "{} {} {} à {}:{:02}",
            d.day(),
            MONTHS[d.month0() as usize],
            d.year(),
            d.hour(),
            d.minute()
        )
    })
}

// --- Presse-papiers ----------------------------------------------------------------

fn copy(text: String) {
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .process_group(0)
        .spawn()
        && let Some(mut stdin) = child.stdin.take()
    {
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
            let _ = child.wait();
        });
    }
}

fn paste() -> Option<String> {
    let out = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).replace('\r', ""))
}

// --- Logique -----------------------------------------------------------------------

impl Notes {
    fn s(&self) -> f32 {
        self.scale.max(1) as f32
    }

    fn reading_trash(&self) -> bool {
        self.view == View::Trash
    }

    /// Relit dossiers, compteurs et liste de la vue
    fn reload(&mut self) {
        match self.store.folders() {
            Ok(f) => self.folders = f,
            Err(e) => self.message = Some(e),
        }
        self.counts = self.store.counts();
        if let View::Folder(id) = self.view
            && !self.folders.iter().any(|f| f.id == id)
        {
            self.view = View::All;
        }
        match self.store.notes(self.view, &self.search) {
            Ok(l) => self.list = l,
            Err(e) => self.message = Some(e),
        }
    }

    /// Enregistre la note affichée si elle a changé
    fn save(&mut self) {
        let Some(id) = self.current else {
            return;
        };
        if !self.editor.changed {
            return;
        }
        match self.store.save(id, &self.editor.text) {
            Ok(()) => self.editor.changed = false,
            Err(e) => self.message = Some(format!("Enregistrement impossible : {e}")),
        }
        self.reload();
    }

    /// On quitte la note : enregistrée, ou effacée si elle est restée vide
    fn leave_note(&mut self) {
        if let Some(id) = self.current
            && self.editor.text.trim().is_empty()
            && !self.reading_trash()
        {
            let _ = self.store.purge(id);
            self.editor = Editor::new(String::new());
            self.current = None;
            self.reload();
            return;
        }
        self.save();
    }

    fn open(&mut self, id: i64) {
        if self.current == Some(id) {
            return;
        }
        self.leave_note();
        match self.store.body(id) {
            Ok(body) => {
                self.editor = Editor::new(body);
                self.editor.move_to(0, false);
                self.current = Some(id);
            }
            Err(e) => {
                self.message = Some(e);
                self.current = None;
                self.editor = Editor::new(String::new());
            }
        }
        self.scroll = 0.0;
        self.menu = false;
        self.layout = None;
    }

    /// Dossier où ranger une nouvelle note
    fn target_folder(&self) -> Option<i64> {
        match self.view {
            View::Folder(id) => Some(id),
            _ => self.folders.first().map(|f| f.id),
        }
    }

    fn new_note(&mut self) {
        self.leave_note();
        if self.reading_trash() {
            self.view = View::All;
        }
        self.search.clear();
        match self.store.create_note(self.target_folder()) {
            Ok(id) => {
                self.reload();
                self.open(id);
            }
            Err(e) => self.message = Some(e),
        }
        self.focus = Focus::Editor;
        self.list_scroll = 0.0;
        self.draw();
    }

    fn set_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        self.leave_note();
        self.view = view;
        self.list_scroll = 0.0;
        self.reload();
        self.current = None;
        self.editor = Editor::new(String::new());
        if let Some(id) = self.list.first().map(|n| n.id) {
            self.open(id);
        }
    }

    /// Note précédente ou suivante de la liste
    fn step_note(&mut self, delta: isize) {
        if self.list.is_empty() {
            return;
        }
        let last = self.list.len() as isize - 1;
        let i = self
            .current
            .and_then(|id| self.list.iter().position(|n| n.id == id))
            .map_or(0, |i| (i as isize + delta).clamp(0, last) as usize);
        let id = self.list[i].id;
        self.open(id);
        // L'ouverture a pu effacer une note vide : la liste a changé
        if let Some(i) = self.list.iter().position(|n| n.id == id) {
            self.reveal_in_list(i);
        }
        self.draw();
    }

    fn reveal_in_list(&mut self, i: usize) {
        let top = i as f32 * NOTE_H;
        let visible = self.height as f32 - TOOLBAR_H - SUB_H;
        if top < self.list_scroll {
            self.list_scroll = top;
        } else if top + NOTE_H > self.list_scroll + visible {
            self.list_scroll = top + NOTE_H - visible;
        }
    }

    fn new_folder(&mut self) {
        self.commit_rename();
        match self.store.create_folder("Nouveau dossier") {
            Ok(id) => {
                self.reload();
                self.set_view(View::Folder(id));
                self.focus = Focus::Rename(id, String::new());
            }
            Err(e) => self.message = Some(e),
        }
        self.draw();
    }

    fn commit_rename(&mut self) {
        if let Focus::Rename(id, name) = std::mem::replace(&mut self.focus, Focus::Editor) {
            let name = name.trim();
            if !name.is_empty()
                && let Err(e) = self.store.rename_folder(id, name)
            {
                self.message = Some(e);
            }
            self.reload();
        }
    }

    /// Second clic dans le délai : l'action est confirmée
    fn confirmed(&mut self, hit: Hit) -> bool {
        let ok = self.is_armed(&hit);
        self.armed = if ok {
            None
        } else {
            Some((hit, Instant::now()))
        };
        ok
    }

    fn is_armed(&self, hit: &Hit) -> bool {
        self.armed
            .as_ref()
            .is_some_and(|(h, at)| h == hit && at.elapsed() < ARMED)
    }

    /// Note retirée de la liste (corbeille, suppression) : on passe à la voisine
    fn after_removal(&mut self, id: i64) {
        let i = self.list.iter().position(|n| n.id == id).unwrap_or(0);
        self.current = None;
        self.editor = Editor::new(String::new());
        self.reload();
        let next = self
            .list
            .get(i.min(self.list.len().saturating_sub(1)))
            .map(|n| n.id);
        if let Some(next) = next {
            self.open(next);
        }
    }

    fn action(&mut self, hit: Hit) {
        self.message = None;
        match hit {
            Hit::View(v) => {
                let mut rename = None;
                if let View::Folder(id) = v {
                    let double = self
                        .last_folder_click
                        .is_some_and(|(f, at)| f == id && at.elapsed() < DOUBLE_CLICK);
                    self.last_folder_click = Some((id, Instant::now()));
                    if double {
                        rename = self
                            .folders
                            .iter()
                            .find(|f| f.id == id)
                            .map(|f| (id, f.name.clone()));
                    }
                }
                self.set_view(v);
                self.focus = match rename {
                    Some((id, name)) => Focus::Rename(id, name),
                    None => Focus::Editor,
                };
            }
            Hit::NewFolder => self.new_folder(),
            Hit::DeleteFolder(id) => {
                if self.confirmed(Hit::DeleteFolder(id)) {
                    self.leave_note();
                    if let Err(e) = self.store.delete_folder(id) {
                        self.message = Some(e);
                    }
                    self.current = None;
                    self.editor = Editor::new(String::new());
                    // Vue courante peut-être disparue : retour à toutes les notes
                    self.view = View::Folder(id);
                    self.set_view(View::All);
                }
            }
            Hit::Note(id) => {
                self.open(id);
                self.focus = Focus::Editor;
            }
            Hit::NewNote => self.new_note(),
            Hit::Search => self.focus = Focus::Search,
            Hit::ClearSearch => {
                self.search.clear();
                self.reload();
                self.focus = Focus::Search;
            }
            Hit::Format(f) => {
                if !self.reading_trash() && self.current.is_some() {
                    self.editor.format(f);
                    self.focus = Focus::Editor;
                    self.edited();
                }
            }
            Hit::MoveMenu => self.menu = !self.menu,
            Hit::MoveTo(folder) => {
                self.menu = false;
                if let Some(id) = self.current {
                    self.save();
                    if let Err(e) = self.store.move_note(id, folder) {
                        self.message = Some(e);
                    }
                    if matches!(self.view, View::Folder(f) if f != folder) {
                        self.after_removal(id);
                    } else {
                        self.reload();
                    }
                }
            }
            Hit::TrashNote => {
                if let Some(id) = self.current
                    && self.confirmed(Hit::TrashNote)
                {
                    self.save();
                    if let Err(e) = self.store.trash(id) {
                        self.message = Some(e);
                    }
                    self.after_removal(id);
                }
            }
            Hit::Restore => {
                if let Some(id) = self.current {
                    if let Err(e) = self.store.restore(id) {
                        self.message = Some(e);
                    }
                    self.after_removal(id);
                }
            }
            Hit::Purge => {
                if let Some(id) = self.current
                    && self.confirmed(Hit::Purge)
                {
                    if let Err(e) = self.store.purge(id) {
                        self.message = Some(e);
                    }
                    self.after_removal(id);
                }
            }
            Hit::EmptyTrash => {
                if self.confirmed(Hit::EmptyTrash) {
                    if let Err(e) = self.store.empty_trash() {
                        self.message = Some(e);
                    }
                    self.current = None;
                    self.editor = Editor::new(String::new());
                    self.reload();
                }
            }
            Hit::Check(at) => {
                if !self.reading_trash() {
                    self.editor.toggle_check(at);
                    self.edited();
                }
            }
            Hit::Text => {}
        }
        self.draw();
    }

    /// Le texte vient de changer
    /// Une autre connexion a écrit : listes relues, et la note affichée
    /// reprise si l'on n'y a rien tapé depuis le dernier enregistrement
    fn external_change(&mut self) {
        self.reload();
        if let Some(id) = self.current
            && !self.editor.changed
        {
            match self.store.note(id) {
                Ok(Some(note)) if note.body != self.editor.text => {
                    let mut pos = self.editor.cursor.min(note.body.len());
                    while !note.body.is_char_boundary(pos) {
                        pos -= 1;
                    }
                    self.editor = Editor::new(note.body);
                    self.editor.move_to(pos, false);
                    self.layout = None;
                }
                Ok(None) => {
                    self.current = None;
                    self.editor = Editor::new(String::new());
                    self.layout = None;
                }
                _ => {}
            }
        }
        self.draw();
    }

    fn edited(&mut self) {
        self.edited_at = Instant::now();
        self.follow = true;
        self.caret_on = true;
    }

    fn tick(&mut self) {
        let version = self.store.data_version();
        if version != self.db_version {
            self.db_version = version;
            self.external_change();
        }
        if self.editor.changed && self.edited_at.elapsed() >= SAVE_AFTER {
            self.save();
            self.draw();
            return;
        }
        let blinking = match self.focus {
            Focus::Editor => {
                self.current.is_some() && self.editor.selection().is_none() && !self.reading_trash()
            }
            _ => true,
        };
        if blinking {
            self.caret_on = !self.caret_on;
            self.draw();
        }
    }

    // --- Clavier ---

    fn key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        let shift = self.modifiers.shift;
        self.caret_on = true;
        if self.menu && event.keysym == Keysym::Escape {
            self.menu = false;
            return self.draw();
        }
        // Raccourcis valables partout
        match event.keysym {
            Keysym::n | Keysym::N if ctrl && shift => return self.new_folder(),
            Keysym::n if ctrl => return self.new_note(),
            Keysym::f if ctrl => {
                self.commit_rename();
                self.focus = Focus::Search;
                return self.draw();
            }
            Keysym::q if ctrl => {
                self.exit = true;
                return;
            }
            Keysym::Up if self.modifiers.alt => return self.step_note(-1),
            Keysym::Down if self.modifiers.alt => return self.step_note(1),
            _ => {}
        }
        match &mut self.focus {
            Focus::Rename(_, name) => {
                match event.keysym {
                    Keysym::Return | Keysym::KP_Enter => self.commit_rename(),
                    Keysym::Escape => self.focus = Focus::Editor,
                    Keysym::BackSpace => {
                        name.pop();
                    }
                    _ => push_typed(name, &event, &self.modifiers),
                }
                self.draw()
            }
            Focus::Search => {
                match event.keysym {
                    Keysym::Escape => {
                        self.search.clear();
                        self.focus = Focus::Editor;
                    }
                    Keysym::Return | Keysym::KP_Enter | Keysym::Down => {
                        self.focus = Focus::Editor;
                    }
                    Keysym::BackSpace if ctrl => self.search.clear(),
                    Keysym::BackSpace => {
                        self.search.pop();
                    }
                    _ => push_typed(&mut self.search, &event, &self.modifiers),
                }
                self.list_scroll = 0.0;
                self.reload();
                // Le premier résultat s'affiche à mesure qu'on tape
                if let Some(id) = self.list.first().map(|n| n.id)
                    && !self.list.iter().any(|n| Some(n.id) == self.current)
                {
                    self.open(id);
                }
                self.draw()
            }
            Focus::Editor => self.editor_key(event),
        }
    }

    fn editor_key(&mut self, event: KeyEvent) {
        if self.current.is_none() {
            return;
        }
        let (ctrl, shift) = (self.modifiers.ctrl, self.modifiers.shift);
        // Déplacements (la mise en page sert à ↑, ↓, Début, Fin)
        let moved = match event.keysym {
            Keysym::Left => {
                self.editor.left(ctrl, shift);
                true
            }
            Keysym::Right => {
                self.editor.right(ctrl, shift);
                true
            }
            Keysym::Up => {
                self.vertical(-1, shift);
                true
            }
            Keysym::Down => {
                self.vertical(1, shift);
                true
            }
            Keysym::Prior => {
                self.vertical(-self.page_lines(), shift);
                true
            }
            Keysym::Next => {
                self.vertical(self.page_lines(), shift);
                true
            }
            Keysym::Home | Keysym::End if ctrl => {
                let to = if event.keysym == Keysym::End {
                    self.editor.text.len()
                } else {
                    0
                };
                self.editor.move_to(to, shift);
                true
            }
            Keysym::Home | Keysym::End => {
                self.line_edge(event.keysym == Keysym::End, shift);
                true
            }
            Keysym::Escape => {
                let c = self.editor.cursor;
                self.editor.move_to(c, false);
                true
            }
            _ => false,
        };
        if moved {
            self.follow = true;
            return self.draw();
        }
        let e = &mut self.editor;
        match event.keysym {
            Keysym::a if ctrl => {
                e.select_all();
                return self.draw();
            }
            Keysym::c if ctrl && !shift => {
                if let Some(t) = e.selected_text() {
                    copy(t.to_string());
                }
                return;
            }
            _ => {}
        }
        // La corbeille se lit sans se modifier
        if self.view == View::Trash {
            return;
        }
        match event.keysym {
            Keysym::x if ctrl && !shift => match e.selected_text() {
                Some(t) => {
                    copy(t.to_string());
                    e.backspace(false);
                }
                None => return,
            },
            Keysym::v if ctrl => match paste() {
                Some(t) if !t.is_empty() => e.insert(&t),
                _ => return,
            },
            Keysym::z | Keysym::Z if ctrl && shift => e.redo(),
            Keysym::y if ctrl => e.redo(),
            Keysym::z if ctrl => e.undo(),
            Keysym::b if ctrl => e.format(Format::Bold),
            Keysym::i if ctrl => e.format(Format::Italic),
            Keysym::e if ctrl => e.format(Format::Code),
            Keysym::x | Keysym::X if ctrl && shift => e.format(Format::Strike),
            Keysym::h | Keysym::H if ctrl && shift => e.format(Format::Heading),
            Keysym::l | Keysym::L if ctrl && shift => e.format(Format::Bullet),
            Keysym::c | Keysym::C if ctrl && shift => e.format(Format::Check),
            Keysym::Return | Keysym::KP_Enter => e.newline(),
            Keysym::BackSpace => e.backspace(ctrl),
            Keysym::Delete | Keysym::KP_Delete => e.delete(ctrl),
            Keysym::Tab => e.indent(false),
            Keysym::ISO_Left_Tab => e.indent(true),
            _ => {
                if ctrl || self.modifiers.alt || self.modifiers.logo {
                    return;
                }
                match event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                {
                    Some(t) => e.insert(&t),
                    None => return,
                }
            }
        }
        self.edited();
        self.draw();
    }

    /// Lignes par page (Pg.préc, Pg.suiv)
    fn page_lines(&self) -> isize {
        let h = self.height as f32 - TOOLBAR_H - DATE_H;
        (h / (15.0 * 1.5)).floor().max(2.0) as isize - 1
    }

    /// Mise en page à jour (le texte a pu changer depuis le dernier dessin)
    fn fresh_layout(&mut self) -> &Layout {
        let (_, width) = self.text_column();
        let reveal = self.reveal();
        self.layout.insert(editor::layout(
            &self.editor.text,
            &self.fonts,
            &self.sizes(),
            width,
            reveal,
        ))
    }

    /// Paragraphes dont les marqueurs Markdown s'affichent : ceux du curseur
    /// et de la sélection ; aucun dans la corbeille (lecture seule)
    fn reveal(&self) -> Option<(usize, usize)> {
        if self.reading_trash() || self.current.is_none() {
            return None;
        }
        let e = &self.editor;
        let (a, b) = e.selection().unwrap_or((e.cursor, e.cursor));
        Some((e.paragraph(a).0, e.paragraph(b).1))
    }

    /// ↑, ↓, Pg.préc, Pg.suiv : d'une ligne à l'écran à l'autre, à la même abscisse
    fn vertical(&mut self, lines: isize, select: bool) {
        let cursor = self.editor.cursor;
        let goal = self.editor.goal_x;
        let len = self.editor.text.len();
        let lay = self.fresh_layout();
        let i = lay.line_of(cursor);
        let x = goal.unwrap_or_else(|| lay.lines[i].x_of(cursor));
        let target = i as isize + lines;
        let pos = if target < 0 {
            0
        } else if target as usize >= lay.lines.len() {
            len
        } else {
            lay.lines[target as usize].byte_at(x)
        };
        self.editor.move_to(pos, select);
        self.editor.goal_x = Some(x);
    }

    /// Début ou fin de la ligne à l'écran
    fn line_edge(&mut self, end: bool, select: bool) {
        let cursor = self.editor.cursor;
        let lay = self.fresh_layout();
        let line = &lay.lines[lay.line_of(cursor)];
        let pos = if end {
            line.byte_at(f32::MAX)
        } else {
            line.glyphs
                .iter()
                .find(|g| !g.style.hidden)
                .map_or(line.end, |g| g.byte)
        };
        self.editor.goal_x = None;
        self.editor.move_to(pos, select);
    }

    // --- Souris ---

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits
            .iter()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .map(|(_, hit)| hit.clone())
    }

    /// Position dans le texte sous le pointeur
    fn byte_under(&self, x: f64, y: f64) -> Option<usize> {
        let s = self.s() as f64;
        let lay = self.layout.as_ref()?;
        Some(lay.byte_at(
            (x * s) as f32 - self.origin.0,
            (y * s) as f32 - self.origin.1,
        ))
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

    fn press(&mut self, x: f64, y: f64, serial: u32) {
        if let Some(b) = self.chrome.press(x as f32, y as f32, CHROME_Y) {
            return self.chrome_action(b);
        }
        let hit = self.hit_at(x, y);
        // Un clic hors du menu le ferme, sans autre effet
        if self.menu && !matches!(hit, Some(Hit::MoveTo(_)) | Some(Hit::MoveMenu)) {
            self.menu = false;
            return self.draw();
        }
        if let Focus::Rename(id, _) = self.focus
            && hit != Some(Hit::View(View::Folder(id)))
        {
            self.commit_rename();
        }
        match hit {
            Some(Hit::Text) => {
                self.focus = Focus::Editor;
                let Some(pos) = self.byte_under(x, y).filter(|_| self.current.is_some()) else {
                    return self.draw();
                };
                let (at, n, (px, py)) = self.clicks;
                let near = (px - x).abs() < 4.0 && (py - y).abs() < 4.0;
                let n = if near && at.elapsed() < DOUBLE_CLICK {
                    n % 3 + 1
                } else {
                    1
                };
                self.clicks = (Instant::now(), n, (x, y));
                match n {
                    2 => self.editor.select_word(pos),
                    3 => self.editor.select_paragraph(pos),
                    _ => {
                        self.editor.move_to(pos, self.modifiers.shift);
                        self.editor.goal_x = None;
                        self.dragging = true;
                    }
                }
                self.caret_on = true;
                self.draw();
            }
            Some(h) => self.action(h),
            None => {
                // Bandeau du haut vide : double-clic agrandit, glisser déplace la fenêtre
                let double = self
                    .last_toolbar_click
                    .is_some_and(|t| t.elapsed() < DOUBLE_CLICK);
                if y < TOOLBAR_H as f64 && double {
                    self.last_toolbar_click = None;
                    self.chrome_action(common::chrome::Button::Maximize);
                } else if y < TOOLBAR_H as f64
                    && let Some(seat) = self.seat.as_ref()
                {
                    self.last_toolbar_click = Some(Instant::now());
                    self.window.move_(seat, serial);
                }
            }
        }
    }

    fn motion(&mut self, x: f64, y: f64) {
        if self.chrome.motion(x as f32, y as f32, CHROME_Y) {
            self.draw();
        }
        if self.dragging {
            if let Some(pos) = self.byte_under(x, y) {
                self.editor.cursor = pos;
                self.follow = true;
                self.draw();
            }
            return;
        }
        let hover = self.hit_at(x, y).filter(|h| *h != Hit::Text);
        if hover != self.hover {
            self.hover = hover;
            self.draw();
        }
    }

    fn wheel(&mut self, x: f64, dy: f64) {
        let step = (dy * 1.5) as f32;
        if x < SIDEBAR_W as f64 {
            return;
        }
        if x < (SIDEBAR_W + LIST_W) as f64 {
            let visible = self.height as f32 - TOOLBAR_H - SUB_H;
            let max = (self.list.len() as f32 * NOTE_H - visible).max(0.0);
            self.list_scroll = (self.list_scroll + step).clamp(0.0, max);
        } else {
            self.scroll = (self.scroll + step).max(0.0);
        }
        self.draw();
    }

    // --- Dessin ---

    fn sizes(&self) -> Sizes {
        let s = self.s();
        Sizes {
            body: 15.0 * s,
            code: 13.5 * s,
            title: 24.0 * s,
            h1: 21.0 * s,
            h2: 18.0 * s,
            h3: 16.0 * s,
            indent: 24.0 * s,
            widget: 26.0 * s,
        }
    }

    /// Colonne de texte : abscisse et largeur (pixels du tampon)
    fn text_column(&self) -> (f32, f32) {
        let s = self.s();
        let x0 = (SIDEBAR_W + LIST_W) * s;
        let avail = self.width as f32 * s - x0;
        let w = (avail - 2.0 * TEXT_PAD * s)
            .min(TEXT_MAX * s)
            .max(100.0 * s);
        (x0 + (avail - w) / 2.0, w)
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let s = self.s();
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
        // Sous la barre latérale, rien d'opaque : elle est translucide sur le flou
        // du compositeur (theme::use_vibrancy) ; le contenu, lui, reste opaque
        c.clear();
        c.fill_rect(
            SIDEBAR_W * s,
            0.0,
            w as f32 - SIDEBAR_W * s,
            h as f32,
            theme::palette().content,
        );
        let mut hits = Hits::new();

        // La note d'abord : les bandeaux se dessinent par-dessus quand elle défile
        self.draw_note(&mut c, &mut hits);
        self.draw_list(&mut c, &mut hits);
        self.draw_sidebar(&mut c, &mut hits);
        self.draw_toolbar(&mut c, &mut hits);
        if self.menu {
            self.draw_menu(&mut c, &mut hits);
        }

        // Ce qui est devant passe en premier : menu, petits boutons, puis le texte
        hits.sort_by_key(|(_, h)| match h {
            Hit::MoveTo(_) => 0,
            Hit::DeleteFolder(_) | Hit::Check(_) | Hit::ClearSearch => 1,
            Hit::Text => 3,
            _ => 2,
        });
        self.hits = hits;
        let surface = self.window.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.window.commit();
        }
        self.pool = Some(pool);
    }

    fn draw_note(&mut self, c: &mut Canvas, hits: &mut Hits) {
        let s = self.s();
        let p = theme::palette();
        let (tx, tw) = self.text_column();
        let top = TOOLBAR_H * s;
        let h = c.height as f32;
        let ex0 = (SIDEBAR_W + LIST_W) * s;
        push_hit(hits, s, ex0, top, c.width as f32 - ex0, h - top, Hit::Text);
        if self.current.is_none() {
            let (msg, hint) = if self.list.is_empty() && self.search.is_empty() {
                ("Aucune note", "Ctrl+N pour en écrire une")
            } else {
                ("Aucune note ouverte", "Choisissez-en une dans la liste")
            };
            let cx = ex0 + (c.width as f32 - ex0) / 2.0;
            self.ui_bold.draw_centered(
                c,
                msg,
                cx,
                h / 2.0 - 30.0 * s,
                24.0 * s,
                16.0 * s,
                p.text_dim,
            );
            self.ui
                .draw_centered(c, hint, cx, h / 2.0, 20.0 * s, 13.0 * s, p.text_dim);
            self.layout = None;
            return;
        }
        let lay = editor::layout(
            &self.editor.text,
            &self.fonts,
            &self.sizes(),
            tw,
            self.reveal(),
        );
        // Défilement : le curseur reste visible ; pas au-delà du texte
        let view_h = h - top - DATE_H * s;
        if self.follow {
            let (_, cy, ch) = lay.point(self.editor.cursor);
            let sc = self.scroll * s;
            if cy < sc {
                self.scroll = cy / s;
            } else if cy + ch > sc + view_h - 16.0 * s {
                self.scroll = (cy + ch - view_h + 16.0 * s) / s;
            }
            self.follow = false;
        }
        let max = ((lay.height - view_h + 48.0 * s) / s).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let oy = top + DATE_H * s - self.scroll * s;
        self.origin = (tx, oy);

        // Date de la dernière modification
        if let Some(t) = self.current.and_then(|id| self.store.modified(id)) {
            let label = if self.reading_trash() {
                format!("Dans la corbeille · modifiée le {}", long_date(t))
            } else {
                long_date(t)
            };
            self.ui.draw_centered(
                c,
                &label,
                tx + tw / 2.0,
                oy - DATE_H * s + 12.0 * s,
                20.0 * s,
                12.0 * s,
                p.text_dim,
            );
        }

        let sel = self.editor.selection();
        let focused = matches!(self.focus, Focus::Editor);
        let sel_color = p.accent.with_alpha(if focused { 0.30 } else { 0.16 });
        for line in &lay.lines {
            let ly = oy + line.y;
            if ly > h {
                break;
            }
            if ly + line.h < top {
                continue;
            }
            // Fond des blocs de code, d'un seul tenant
            if matches!(line.block, Block::Code | Block::Fence) {
                c.fill_rect(tx - 10.0 * s, ly, tw + 20.0 * s, line.h, p.ink(0.05));
            }
            let baseline = self.fonts.regular.baseline(ly, line.h, line.px);
            // Sélection
            if let Some((a, b)) = sel
                && a <= line.end
                && b >= line.start
                && !(b == line.start && !line.first && a < b)
            {
                let x0 = line.x_of(a.max(line.start));
                let mut x1 = line.x_of(b.min(line.end));
                if b > line.end && line.last {
                    x1 += 6.0 * s;
                }
                c.fill_rect(tx + x0, ly, x1 - x0, line.h, sel_color);
            }
            // Puce, case, filet de citation
            let mid = ly + line.h / 2.0;
            let wx = tx + line.widget_x;
            match line.block {
                Block::Bullet if line.first => c.fill_circle(wx + 9.0 * s, mid, 2.6 * s, p.text),
                Block::Check(done) if line.first => {
                    let size = 16.0 * s;
                    let (bx, by) = (wx + 1.0 * s, mid - size / 2.0);
                    if done {
                        c.fill_rounded(bx, by, size, size, 4.0 * s, p.accent);
                        self.icons.icon(
                            c,
                            icon::CHECK,
                            bx + size / 2.0,
                            mid,
                            12.0 * s,
                            p.on_accent,
                        );
                    } else {
                        c.stroke_rounded(bx, by, size, size, 4.0 * s, 1.5 * s, p.text_dim);
                    }
                    push_hit(
                        hits,
                        s,
                        bx - 4.0 * s,
                        by - 4.0 * s,
                        size + 8.0 * s,
                        size + 8.0 * s,
                        Hit::Check(line.start),
                    );
                }
                Block::Quote => {
                    c.fill_rounded(tx + 2.0 * s, ly, 3.0 * s, line.h, 1.5 * s, p.accent)
                }
                _ => {}
            }
            // Texte, par suites de caractères de même style
            let g = &line.glyphs;
            let done = matches!(line.block, Block::Check(true));
            let mut i = 0;
            while i < g.len() {
                let st = g[i].style;
                let mut j = i;
                let mut run = String::new();
                while j < g.len() && g[j].style == st {
                    run.push(g[j].ch);
                    j += 1;
                }
                if st.hidden {
                    i = j;
                    continue;
                }
                let x = tx + g[i].x;
                let x_end = tx + g[j - 1].x + g[j - 1].w;
                let color = if st.marker {
                    p.text_dim.with_alpha(0.55)
                } else if done || line.block == Block::Quote {
                    p.text_soft()
                } else {
                    p.text
                };
                if st.code && !st.marker && !matches!(line.block, Block::Code | Block::Fence) {
                    c.fill_rounded(
                        x - 2.0 * s,
                        ly + line.h * 0.16,
                        x_end - x + 4.0 * s,
                        line.h * 0.68,
                        4.0 * s,
                        p.ink(0.08),
                    );
                }
                let font = self.fonts.pick(&st, line.block);
                font.draw_baseline(c, &run, x, baseline, line.px, color);
                if st.strike && !st.marker {
                    let sy = (baseline - line.px * 0.3).round();
                    c.fill_rect(x, sy, x_end - x, s.max(1.0), color);
                }
                i = j;
            }
        }
        // Texte vide : invitation
        if self.editor.text.is_empty() {
            let px = self.sizes().title;
            self.fonts.bold.draw_in(
                c,
                "Titre de la note",
                tx,
                oy,
                (px * 1.5).round(),
                px,
                p.text_dim.with_alpha(0.5),
            );
        }
        // Curseur
        if focused && self.caret_on && !self.reading_trash() {
            let (cx, cy, ch) = lay.point(self.editor.cursor);
            let line = &lay.lines[lay.line_of(self.editor.cursor)];
            let caret_h = (line.px * 1.3).min(ch);
            let y = oy + cy + (ch - caret_h) / 2.0;
            if y + caret_h > top {
                c.fill_rect(tx + cx - s, y, 2.0 * s, caret_h, p.accent);
            }
        }
        self.layout = Some(lay);
    }

    fn draw_toolbar(&self, c: &mut Canvas, hits: &mut Hits) {
        let s = self.s();
        let p = theme::palette();
        let x0 = (SIDEBAR_W + LIST_W) * s;
        let w = c.width as f32;
        let th = TOOLBAR_H * s;
        c.fill_rect(x0, 0.0, w - x0, th, p.content);
        c.fill_rect(x0, th - s, w - x0, s, p.ink(0.06));
        if self.current.is_none() {
            return;
        }
        let size = 30.0 * s;
        let y = (th - size) / 2.0;
        let mut x = x0 + 16.0 * s;
        let button = |c: &mut Canvas, hits: &mut Hits, x: f32, glyph: char, hit: Hit| {
            if self.hover.as_ref() == Some(&hit) {
                c.fill_rounded(x, y, size, size, 8.0 * s, p.tile_hover());
            }
            self.icons.icon(
                c,
                glyph,
                x + size / 2.0,
                y + size / 2.0,
                16.0 * s,
                p.text_soft(),
            );
            push_hit(hits, s, x, y, size, size, hit);
        };
        // Bouton à libellé ; rouge une fois armé (second clic pour confirmer)
        let labeled = |c: &mut Canvas, hits: &mut Hits, x: f32, label: &str, hit: Hit, danger| {
            let bw = self.ui.measure(label, 13.0 * s) + 28.0 * s;
            let (bg, fg) = if self.is_armed(&hit) {
                (p.danger, p.on_accent)
            } else {
                let bg = if self.hover.as_ref() == Some(&hit) {
                    p.tile_hover()
                } else {
                    p.tile()
                };
                (bg, if danger { p.danger } else { p.text })
            };
            c.fill_rounded(x, y, bw, size, 8.0 * s, bg);
            self.ui
                .draw_centered(c, label, x + bw / 2.0, y, size, 13.0 * s, fg);
            push_hit(hits, s, x, y, bw, size, hit);
            bw
        };
        if self.reading_trash() {
            // Corbeille : remettre la note, ou la supprimer pour de bon
            x += labeled(c, hits, x, "Remettre", Hit::Restore, false) + 8.0 * s;
            let purge = if self.is_armed(&Hit::Purge) {
                "Confirmer la suppression"
            } else {
                "Supprimer définitivement"
            };
            labeled(c, hits, x, purge, Hit::Purge, true);
            return;
        }
        let groups: [&[(char, Format)]; 2] = [
            &[
                (icon::HEADING, Format::Heading),
                (icon::BOLD, Format::Bold),
                (icon::ITALIC, Format::Italic),
                (icon::STRIKETHROUGH, Format::Strike),
                (icon::CODE, Format::Code),
            ],
            &[
                (icon::LIST, Format::Bullet),
                (icon::LIST_CHECKS, Format::Check),
            ],
        ];
        for (k, group) in groups.iter().enumerate() {
            if k > 0 {
                c.fill_rect(x + 4.0 * s, y + 7.0 * s, s, size - 14.0 * s, p.ink(0.1));
                x += 12.0 * s;
            }
            for &(glyph, f) in *group {
                button(c, hits, x, glyph, Hit::Format(f));
                x += size + 4.0 * s;
            }
        }
        // À droite : la corbeille, et le dossier de la note (menu pour la déplacer)
        let right = w - 16.0 * s;
        let trash_x = if self.is_armed(&Hit::TrashNote) {
            let label = "Mettre à la corbeille ?";
            let bw = self.ui.measure(label, 13.0 * s) + 28.0 * s;
            labeled(c, hits, right - bw, label, Hit::TrashNote, true);
            right - bw
        } else {
            button(c, hits, right - size, icon::TRASH, Hit::TrashNote);
            right - size
        };
        let folder = self
            .current
            .and_then(|id| self.list.iter().find(|n| n.id == id))
            .and_then(|n| n.folder)
            .and_then(|f| self.folders.iter().find(|x| x.id == f))
            .map_or("Sans dossier".to_string(), |f| f.name.clone());
        let label = self.ui.truncate(&folder, 13.0 * s, 160.0 * s);
        let pw = self.ui.measure(&label, 13.0 * s) + 56.0 * s;
        let px = trash_x - 8.0 * s - pw;
        let bg = if self.menu || self.hover.as_ref() == Some(&Hit::MoveMenu) {
            p.tile_hover()
        } else {
            p.tile()
        };
        c.fill_rounded(px, y, pw, size, 8.0 * s, bg);
        self.icons.icon(
            c,
            icon::FOLDER,
            px + 17.0 * s,
            y + size / 2.0,
            14.0 * s,
            p.accent,
        );
        self.ui
            .draw_in(c, &label, px + 32.0 * s, y, size, 13.0 * s, p.text);
        self.icons.icon(
            c,
            icon::CHEVRON_DOWN,
            px + pw - 15.0 * s,
            y + size / 2.0,
            12.0 * s,
            p.text_dim,
        );
        push_hit(hits, s, px, y, pw, size, Hit::MoveMenu);
    }

    /// Menu « déplacer vers », sous le bouton du dossier
    fn draw_menu(&self, c: &mut Canvas, hits: &mut Hits) {
        let s = self.s();
        let p = theme::palette();
        let Some(((bx, _, bw, _), _)) = hits.iter().find(|(_, h)| *h == Hit::MoveMenu).cloned()
        else {
            return;
        };
        let current = self
            .current
            .and_then(|id| self.list.iter().find(|n| n.id == id))
            .and_then(|n| n.folder);
        let row = 32.0 * s;
        let mw = (bw as f32 * s).max(220.0 * s);
        let mh = row * self.folders.len() as f32 + 44.0 * s;
        let mx = (bx as f32 * s + bw as f32 * s - mw).max((SIDEBAR_W + LIST_W) * s);
        let my = TOOLBAR_H * s - 4.0 * s;
        c.shadow(mx, my, mw, mh, 10.0 * s, 18.0 * s, 0.25);
        c.fill_rounded(mx, my, mw, mh, 10.0 * s, p.panel);
        c.stroke_rounded(mx, my, mw, mh, 10.0 * s, s, p.panel_border());
        self.ui.draw_in(
            c,
            "Déplacer vers",
            mx + 14.0 * s,
            my + 8.0 * s,
            24.0 * s,
            11.5 * s,
            p.text_dim,
        );
        let mut y = my + 36.0 * s;
        for f in &self.folders {
            let hit = Hit::MoveTo(f.id);
            if self.hover.as_ref() == Some(&hit) {
                c.fill_rounded(mx + 6.0 * s, y, mw - 12.0 * s, row, 6.0 * s, p.tile_hover());
            }
            let here = current == Some(f.id);
            self.icons.icon(
                c,
                if here { icon::CHECK } else { icon::FOLDER },
                mx + 24.0 * s,
                y + row / 2.0,
                14.0 * s,
                p.accent,
            );
            let name = self.ui.truncate(&f.name, 13.5 * s, mw - 60.0 * s);
            self.ui
                .draw_in(c, &name, mx + 42.0 * s, y, row, 13.5 * s, p.text);
            push_hit(hits, s, mx + 6.0 * s, y, mw - 12.0 * s, row, hit);
            y += row;
        }
    }

    fn draw_list(&self, c: &mut Canvas, hits: &mut Hits) {
        let s = self.s();
        let p = theme::palette();
        let x0 = SIDEBAR_W * s;
        let lw = LIST_W * s;
        let h = c.height as f32;
        let th = TOOLBAR_H * s;
        let sub_h = SUB_H * s;
        c.fill_rect(x0, 0.0, lw, h, p.content);
        c.fill_rect(x0 + lw - s, 0.0, s, h, p.ink(0.06));

        // Notes
        let top = th + sub_h;
        let focused_list = !matches!(self.focus, Focus::Editor);
        let nh = NOTE_H * s;
        let (rx, rw) = (x0 + 8.0 * s, lw - 16.0 * s);
        for (i, n) in self.list.iter().enumerate() {
            let y = top + i as f32 * nh - self.list_scroll * s;
            if y + nh < top {
                continue;
            }
            if y > h {
                break;
            }
            let selected = self.current == Some(n.id);
            let prev_selected = i > 0 && self.current == Some(self.list[i - 1].id);
            if selected {
                let a = if focused_list { 0.14 } else { 0.22 };
                c.fill_rounded(
                    rx,
                    y + 2.0 * s,
                    rw,
                    nh - 4.0 * s,
                    8.0 * s,
                    p.accent.with_alpha(a),
                );
            } else if self.hover.as_ref() == Some(&Hit::Note(n.id)) {
                c.fill_rounded(rx, y + 2.0 * s, rw, nh - 4.0 * s, 8.0 * s, p.ink(0.04));
            } else if i > 0 && !prev_selected {
                c.fill_rect(x0 + 20.0 * s, y, lw - 40.0 * s, s, p.ink(0.06));
            }
            let inner = rw - 24.0 * s;
            let (title, tcolor) = if n.title.is_empty() {
                ("Nouvelle note".to_string(), p.text_dim)
            } else {
                (self.ui_bold.truncate(&n.title, 14.0 * s, inner), p.text)
            };
            self.ui_bold.draw_in(
                c,
                &title,
                rx + 12.0 * s,
                y + 10.0 * s,
                22.0 * s,
                14.0 * s,
                tcolor,
            );
            let date = short_date(n.modified);
            self.ui.draw_in(
                c,
                &date,
                rx + 12.0 * s,
                y + 32.0 * s,
                20.0 * s,
                12.0 * s,
                p.text,
            );
            let dw = self.ui.measure(&date, 12.0 * s) + 10.0 * s;
            let preview = if n.preview.is_empty() {
                "Pas d'autre texte"
            } else {
                &n.preview
            };
            let preview = self.ui.truncate(preview, 12.0 * s, inner - dw);
            self.ui.draw_in(
                c,
                &preview,
                rx + 12.0 * s + dw,
                y + 32.0 * s,
                20.0 * s,
                12.0 * s,
                p.text_dim,
            );
            push_hit(hits, s, rx, y.max(top), rw, nh, Hit::Note(n.id));
        }
        if self.list.is_empty() {
            let msg = if !self.search.is_empty() {
                "Aucun résultat"
            } else if self.reading_trash() {
                "La corbeille est vide"
            } else {
                "Aucune note"
            };
            self.ui.draw_centered(
                c,
                msg,
                x0 + lw / 2.0,
                top + 40.0 * s,
                24.0 * s,
                13.5 * s,
                p.text_dim,
            );
        }

        // Bandeau : recherche, nouvelle note ; puis le nom de la vue
        c.fill_rect(x0, 0.0, lw - s, top, p.content);
        let size = 32.0 * s;
        let y = (th - size) / 2.0;
        let fx = x0 + 12.0 * s;
        let fw = lw - 24.0 * s - size - 8.0 * s;
        let searching = matches!(self.focus, Focus::Search);
        c.fill_rounded(fx, y, fw, size, 8.0 * s, p.ink(0.06));
        if searching {
            c.stroke_rounded(fx, y, fw, size, 8.0 * s, 1.5 * s, p.accent);
        }
        self.icons.icon(
            c,
            icon::SEARCH,
            fx + 16.0 * s,
            y + size / 2.0,
            14.0 * s,
            p.text_dim,
        );
        let tx = fx + 32.0 * s;
        let shown = tail_fit(&self.ui, &self.search, 13.0 * s, fw - 64.0 * s);
        if self.search.is_empty() {
            if !searching {
                self.ui
                    .draw_in(c, "Rechercher", tx, y, size, 13.0 * s, p.text_dim);
            }
        } else {
            self.ui.draw_in(c, &shown, tx, y, size, 13.0 * s, p.text);
            self.icons.icon(
                c,
                icon::CLOSE,
                fx + fw - 16.0 * s,
                y + size / 2.0,
                13.0 * s,
                p.text_dim,
            );
            push_hit(
                hits,
                s,
                fx + fw - 30.0 * s,
                y,
                30.0 * s,
                size,
                Hit::ClearSearch,
            );
        }
        if searching && self.caret_on {
            let cx = tx + self.ui.measure(&shown, 13.0 * s);
            c.fill_rect(cx, y + 8.0 * s, 1.5 * s, size - 16.0 * s, p.accent);
        }
        push_hit(hits, s, fx, y, fw, size, Hit::Search);
        let nx = fx + fw + 8.0 * s;
        let bg = if self.hover.as_ref() == Some(&Hit::NewNote) {
            p.tile_hover()
        } else {
            p.tile()
        };
        c.fill_rounded(nx, y, size, size, 8.0 * s, bg);
        self.icons.icon(
            c,
            icon::NOTEBOOK_PEN,
            nx + size / 2.0,
            y + size / 2.0,
            16.0 * s,
            p.accent,
        );
        push_hit(hits, s, nx, y, size, size, Hit::NewNote);

        let name = match self.view {
            View::All => "Toutes les notes".to_string(),
            View::Trash => "Corbeille".to_string(),
            View::Folder(id) => self
                .folders
                .iter()
                .find(|f| f.id == id)
                .map_or_else(String::new, |f| f.name.clone()),
        };
        let count = self.list.len();
        let label = format!(
            "{} · {count} note{}",
            self.ui.truncate(&name, 12.0 * s, lw - 170.0 * s),
            if count > 1 { "s" } else { "" }
        );
        self.ui.draw_in(
            c,
            &label,
            x0 + 20.0 * s,
            th - 4.0 * s,
            sub_h,
            12.0 * s,
            p.text_dim,
        );
        if self.reading_trash() && count > 0 {
            let armed = self.is_armed(&Hit::EmptyTrash);
            let label = if armed { "Confirmer ?" } else { "Vider" };
            let bw = self.ui.measure(label, 12.0 * s) + 18.0 * s;
            let bx = x0 + lw - 14.0 * s - bw;
            let by = th;
            let (bg, fg) = if armed {
                (p.danger, p.on_accent)
            } else {
                (p.tile(), p.danger)
            };
            c.fill_rounded(bx, by, bw, 22.0 * s, 6.0 * s, bg);
            self.ui
                .draw_centered(c, label, bx + bw / 2.0, by, 22.0 * s, 12.0 * s, fg);
            push_hit(hits, s, bx, by, bw, 22.0 * s, Hit::EmptyTrash);
        }
        if let Some(msg) = &self.message {
            let m = self.ui.truncate(msg, 12.0 * s, lw - 24.0 * s);
            c.fill_rect(x0, h - 28.0 * s, lw - s, 28.0 * s, p.content);
            self.ui.draw_in(
                c,
                &m,
                x0 + 12.0 * s,
                h - 28.0 * s,
                28.0 * s,
                12.0 * s,
                p.urgent,
            );
        }
    }

    fn draw_sidebar(&self, c: &mut Canvas, hits: &mut Hits) {
        let s = self.s();
        let p = theme::palette();
        let sw = SIDEBAR_W * s;
        let h = c.height as f32;
        c.fill_rect(0.0, 0.0, sw, h, p.sidebar);
        c.fill_rect(sw - s, 0.0, s, h, p.ink(0.06));
        let row_h = ROW_H * s;
        let (rx, rw) = (10.0 * s, sw - 20.0 * s);

        let row = |c: &mut Canvas,
                   hits: &mut Hits,
                   y: f32,
                   glyph: char,
                   label: &str,
                   count: Option<usize>,
                   hit: Hit| {
            let active = matches!(hit, Hit::View(v) if v == self.view);
            if active {
                c.fill_rounded(rx, y, rw, row_h, 8.0 * s, p.accent.with_alpha(0.18));
            } else if self.hover.as_ref() == Some(&hit) {
                c.fill_rounded(rx, y, rw, row_h, 8.0 * s, p.ink(0.04));
            }
            self.icons
                .icon(c, glyph, 30.0 * s, y + row_h / 2.0, 15.0 * s, p.accent);
            let text = self.ui.truncate(label, 14.0 * s, sw - 100.0 * s);
            let color = if active { p.text } else { p.text_soft() };
            self.ui
                .draw_in(c, &text, 48.0 * s, y, row_h, 14.0 * s, color);
            if let Some(n) = count.filter(|n| *n > 0) {
                let t = n.to_string();
                let tw = self.ui.measure(&t, 12.0 * s);
                self.ui
                    .draw_in(c, &t, sw - 22.0 * s - tw, y, row_h, 12.0 * s, p.text_dim);
            }
            push_hit(hits, s, rx, y, rw, row_h, hit);
        };

        self.chrome.draw(c, CHROME_Y, s);
        let mut y = TOOLBAR_H * s + 4.0 * s;
        row(
            c,
            hits,
            y,
            icon::NOTEBOOK,
            "Toutes les notes",
            Some(self.counts.0),
            Hit::View(View::All),
        );
        y += row_h + 18.0 * s;

        // Dossiers, avec le bouton « nouveau dossier »
        self.ui
            .draw_in(c, "DOSSIERS", 20.0 * s, y, 24.0 * s, 11.0 * s, p.text_dim);
        let nb = 24.0 * s;
        let nx = sw - 16.0 * s - nb;
        if self.hover.as_ref() == Some(&Hit::NewFolder) {
            c.fill_rounded(nx, y, nb, nb, 6.0 * s, p.tile_hover());
        }
        self.icons.icon(
            c,
            icon::FOLDER_PLUS,
            nx + nb / 2.0,
            y + nb / 2.0,
            14.0 * s,
            p.text_dim,
        );
        push_hit(hits, s, nx, y, nb, nb, Hit::NewFolder);
        y += 28.0 * s;

        let trash_y = h - 16.0 * s - row_h;
        for f in &self.folders {
            if y + row_h > trash_y - 8.0 * s {
                break;
            }
            let view_hit = Hit::View(View::Folder(f.id));
            if let Focus::Rename(id, name) = &self.focus
                && *id == f.id
            {
                // Champ de saisie du nom
                c.fill_rounded(rx, y, rw, row_h, 8.0 * s, p.content);
                c.stroke_rounded(rx, y, rw, row_h, 8.0 * s, 1.5 * s, p.accent);
                self.icons.icon(
                    c,
                    icon::FOLDER,
                    30.0 * s,
                    y + row_h / 2.0,
                    15.0 * s,
                    p.accent,
                );
                let shown = tail_fit(&self.ui, name, 14.0 * s, sw - 90.0 * s);
                if name.is_empty() {
                    self.ui.draw_in(
                        c,
                        "Nom du dossier",
                        48.0 * s,
                        y,
                        row_h,
                        14.0 * s,
                        p.text_dim,
                    );
                }
                self.ui
                    .draw_in(c, &shown, 48.0 * s, y, row_h, 14.0 * s, p.text);
                if self.caret_on {
                    let cx = 48.0 * s + self.ui.measure(&shown, 14.0 * s);
                    c.fill_rect(cx, y + 8.0 * s, 1.5 * s, row_h - 16.0 * s, p.accent);
                }
                push_hit(hits, s, rx, y, rw, row_h, view_hit);
                y += row_h + 2.0 * s;
                continue;
            }
            let del = Hit::DeleteFolder(f.id);
            let hovered =
                self.hover.as_ref() == Some(&view_hit) || self.hover.as_ref() == Some(&del);
            let armed = self.is_armed(&del);
            // Survolé : une corbeille à la place du compteur (s'il reste un autre dossier)
            let deletable = self.folders.len() > 1;
            let count = if (hovered && deletable) || armed {
                None
            } else {
                Some(f.count)
            };
            row(c, hits, y, icon::FOLDER, &f.name, count, view_hit);
            if armed {
                let label = "Supprimer ?";
                let bw = self.ui.measure(label, 11.5 * s) + 14.0 * s;
                let bx = sw - 16.0 * s - bw;
                c.fill_rounded(bx, y + 5.0 * s, bw, row_h - 10.0 * s, 6.0 * s, p.danger);
                self.ui
                    .draw_centered(c, label, bx + bw / 2.0, y, row_h, 11.5 * s, p.on_accent);
                push_hit(hits, s, bx, y, bw, row_h, del);
            } else if hovered && deletable {
                let bx = sw - 40.0 * s;
                self.icons.icon(
                    c,
                    icon::TRASH,
                    bx + 12.0 * s,
                    y + row_h / 2.0,
                    13.0 * s,
                    p.text_dim,
                );
                push_hit(hits, s, bx, y, 24.0 * s, row_h, del);
            }
            y += row_h + 2.0 * s;
        }

        // Corbeille, en bas
        c.fill_rect(20.0 * s, trash_y - 8.0 * s, sw - 40.0 * s, s, p.ink(0.06));
        row(
            c,
            hits,
            trash_y,
            icon::TRASH,
            "Corbeille",
            Some(self.counts.1),
            Hit::View(View::Trash),
        );
    }
}

/// Zone cliquable : pixels du tampon → coordonnées logiques
fn push_hit(hits: &mut Hits, s: f32, x: f32, y: f32, w: f32, h: f32, hit: Hit) {
    let s = s as f64;
    hits.push((
        (x as f64 / s, y as f64 / s, w as f64 / s, h as f64 / s),
        hit,
    ));
}

/// La fin d'un texte trop long pour son champ (on voit ce qu'on tape)
fn tail_fit(font: &Text, s: &str, px: f32, max_w: f32) -> String {
    if font.measure(s, px) <= max_w {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = font.measure("…", px);
    for ch in s.chars().rev() {
        w += font.advance(ch, px);
        if w > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out.into_iter().rev().collect()
}

/// Caractères tapés dans un champ d'une ligne
fn push_typed(field: &mut String, event: &KeyEvent, m: &Modifiers) {
    if m.ctrl || m.alt || m.logo {
        return;
    }
    if let Some(t) = event
        .utf8
        .as_ref()
        .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
    {
        field.push_str(t);
    }
}

// --- Wayland -----------------------------------------------------------------------

impl CompositorHandler for Notes {
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

impl WindowHandler for Notes {
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
        self.draw();
    }
}

impl SeatHandler for Notes {
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
                Box::new(|notes, _, event| notes.key(event)),
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

impl KeyboardHandler for Notes {
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
        // Fenêtre quittée : rien ne reste en attente d'écriture
        self.save();
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

impl PointerHandler for Notes {
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
                } => self.press(x, y, serial),
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } => self.dragging = false,
                PointerEventKind::Motion { .. } => self.motion(x, y),
                PointerEventKind::Leave { .. } => {
                    let chrome = self.chrome.leave();
                    if self.hover.take().is_some() || chrome {
                        self.draw();
                    }
                }
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    self.wheel(x, vertical.absolute)
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Notes {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Notes {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Notes {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Notes);
delegate_registry!(Notes);
delegate_output!(Notes);
delegate_seat!(Notes);
delegate_keyboard!(Notes);
delegate_pointer!(Notes);
delegate_shm!(Notes);
delegate_xdg_shell!(Notes);
delegate_xdg_window!(Notes);
