//! Assistant d'odysseus-desktop : panneau ouvert par Super+A (ou « ? » dans le
//! lanceur), qui pose les questions à l'agent choisi dans Réglages > Agent
//! (Claude Code, Ollama, commande). La réponse s'écrit au fil de l'eau ; les
//! outils du bureau qu'il utilise (odysseus-mcp) s'affichent au passage.
//! On y glisse des fichiers depuis Fichiers (pièces jointes).
//!
//! L'agent dans le contexte : à l'ouverture, le panneau rassemble ce que
//! l'utilisateur avait sous les yeux (context) en pastilles retirables, la
//! fenêtre active, le texte sélectionné, le presse-papiers, le dossier ouvert
//! dans Fichiers ; avec `--contexte` (Super+A) il se place près de la fenêtre.
//! Chaque réponse se copie, ou s'insère dans la fenêtre d'où l'on vient.
//!
//! Les conversations sont gardées (store, ~/.local/share/odysseus/assistant.db)
//! et se reprennent : les plus récentes sous les suggestions, toutes par
//! l'icône d'horloge ou Ctrl+H, avec une recherche.
//!
//! `odysseus-assistant [--contexte] [--joindre FICHIER]… [QUESTION]`
//! Clavier : Entrée envoie, Échap arrête la réponse puis ferme, ↑ reprend la
//! dernière question, Ctrl+V colle, Ctrl+L nouvelle conversation, Ctrl+H les
//! conversations précédentes.

mod backend;
mod context;
mod mcp;

use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use assistant::store::{Conversation, Kind, Message, Store, Summary};
use backend::{Ask, Context, Event, Job};
use chrono::{DateTime, Datelike, Local, Timelike};
use common::{
    agent::AgentConfig,
    draw::{Canvas, Text},
    theme::{self, Rgba, icon},
};
use context::{Chip, Window};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            channel::{self, Event as ChannelEvent},
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm,
                wl_surface,
            },
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

const MARGIN: f32 = 16.0;
const PW: f32 = 580.0;
const PH: f32 = 700.0;
const HEADER_H: f32 = 56.0;
const INPUT_H: f32 = 96.0;
const PAD: f32 = 22.0;
const LINE: f32 = 21.0;
const BTN_LEFT: u32 = 0x110;
/// Hauteur d'une ligne de la liste des conversations
const ROW_H: f32 = 54.0;
/// Conversations récentes proposées sous les suggestions
const RECENT: usize = 3;
/// Hauteur d'une rangée de pastilles (fichiers joints, contexte)
const CHIP_ROW: f32 = 30.0;

const SUGGESTIONS: [&str; 4] = [
    "Qu'est-ce qui chauffe en ce moment ?",
    "Quelles fenêtres sont ouvertes ?",
    "Résume mes notifications",
    "Pourquoi la machine rame ?",
];

/// Élément de la conversation
enum Item {
    User {
        text: String,
        files: Vec<String>,
        /// Pastilles de contexte envoyées avec la question (« fenêtre », « sélection »…)
        context: Vec<String>,
    },
    Answer(String),
    Tool {
        label: String,
        state: ToolState,
    },
    Error(String),
    /// Information du panneau lui-même, pas gardée dans le fil
    Notice(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ToolState {
    Running,
    Done,
    Refused,
    /// La réponse s'est arrêtée avant la fin de l'outil
    Interrupted,
}

/// Ce qui est gardé d'un élément ; rien pour un avis du panneau
fn message_from(item: &Item) -> Option<Message> {
    let (kind, body, extra) = match item {
        Item::User {
            text,
            files,
            context,
        } => {
            let mut extra = files.clone();
            if !context.is_empty() {
                extra.push(format!("contexte: {}", context.join(", ")));
            }
            (Kind::User, text.clone(), extra.join("\n"))
        }
        Item::Answer(t) => (Kind::Answer, t.clone(), String::new()),
        Item::Tool { label, state } => {
            let state = match state {
                ToolState::Done => "ok",
                ToolState::Refused => "refuse",
                ToolState::Running | ToolState::Interrupted => "interrompu",
            };
            (Kind::Tool, label.clone(), state.into())
        }
        Item::Error(e) => (Kind::Error, e.clone(), String::new()),
        Item::Notice(_) => return None,
    };
    Some(Message { kind, body, extra })
}

fn item_from(m: &Message) -> Item {
    match m.kind {
        Kind::User => Item::User {
            text: m.body.clone(),
            files: m
                .extra
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with("contexte: "))
                .map(String::from)
                .collect(),
            context: m
                .extra
                .lines()
                .find_map(|l| l.strip_prefix("contexte: "))
                .map(|c| c.split(", ").map(String::from).collect())
                .unwrap_or_default(),
        },
        Kind::Answer => Item::Answer(m.body.clone()),
        Kind::Tool => Item::Tool {
            label: m.body.clone(),
            state: match m.extra.as_str() {
                "refuse" => ToolState::Refused,
                "interrompu" => ToolState::Interrupted,
                _ => ToolState::Done,
            },
        },
        Kind::Error => Item::Error(m.body.clone()),
    }
}

/// La liste des conversations, quand elle est ouverte
struct History {
    search: String,
    rows: Vec<Summary>,
    selected: usize,
    scroll: f32,
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    Suggestion(usize),
    Send,
    Stop,
    New,
    Close,
    RemoveFile(usize),
    RemoveChip(usize),
    /// Une réponse (indice dans la conversation) : la copier, l'insérer dans la fenêtre
    CopyAnswer(usize),
    InsertAnswer(usize),
    /// Ouvrir ou fermer la liste des conversations
    History,
    Back,
    /// Reprendre une conversation, la supprimer
    Open(i64),
    Delete(i64),
}

/// Largeur, hauteur du tampon et thème
type BackgroundKey = (u32, u32, &'static str);

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: Option<SlotPool>,
    layer: LayerSurface,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    data_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    regular: Text,
    bold: Text,
    icons: Text,
    events: channel::Sender<(u64, Event)>,
    drops: channel::Sender<Vec<PathBuf>>,
    agent: AgentConfig,
    items: Vec<Item>,
    input: String,
    files: Vec<PathBuf>,
    /// Le contexte rassemblé à l'ouverture, tant qu'il n'est pas envoyé ni retiré
    chips: Vec<Chip>,
    /// La fenêtre active à l'ouverture : où insérer une réponse, où se placer
    window: Option<Window>,
    /// `--contexte` : la bulle se place près de la fenêtre active
    anchor: bool,
    /// Réponse copiée à l'instant (le bouton le dit deux secondes)
    copied: Option<(usize, Instant)>,
    last_question: String,
    job: Option<Job>,
    /// Numéro de la réponse en cours : les événements d'une réponse
    /// abandonnée arrivent avec un numéro périmé et sont ignorés
    generation: u64,
    context: Context,
    /// Le fil ; absent si la base ne s'ouvre pas (l'Assistant marche quand même)
    store: Option<Store>,
    /// Conversation en cours dans le fil, dès la première question
    conversation: Option<i64>,
    /// Les plus récentes, proposées quand la conversation est vide
    recent: Vec<Summary>,
    history: Option<History>,
    /// Conversations supprimées pendant cette session, pour Ctrl+Z
    trash: Vec<Conversation>,
    /// Défilement : distance depuis le bas (0 : on suit la réponse)
    scroll: f32,
    content_h: f32,
    hits: Vec<((f32, f32, f32, f32), Hit)>,
    hover: Option<Hit>,
    drop_hover: bool,
    frame_pending: bool,
    dirty: bool,
    /// Fond déjà dessiné, pour cette taille de tampon et ce thème
    background: Option<(BackgroundKey, Vec<u8>)>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

fn pid_file() -> Option<PathBuf> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let display = display.rsplit('/').next().unwrap_or("").to_string();
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join(format!("odysseus-assistant.{display}.pid")))
}

/// Déjà ouvert : on le ferme (bascule) ; renvoie vrai s'il tournait
fn close_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return false;
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let alive = stat.contains("(odysseus-assis")
        && !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z');
    if !alive || pid as u32 == std::process::id() {
        return false;
    }
    unsafe { libc::kill(pid, libc::SIGTERM) };
    true
}

fn main() {
    // Arguments : pièces jointes, question, bulle en contexte
    let mut files = Vec::new();
    let mut words = Vec::new();
    let mut anchor = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--joindre" => {
                if let Some(f) = args.next() {
                    files.push(PathBuf::from(f));
                }
            }
            "--contexte" => anchor = true,
            _ => words.push(a),
        }
    }
    let question = words.join(" ");
    let with_args = !files.is_empty() || !question.is_empty();
    // Super+A : bascule. Avec une question ou un fichier : l'ancien panneau
    // cède la place au nouveau.
    if close_existing() && !with_args {
        return;
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };
    // Ce que l'utilisateur a sous les yeux, avant que le panneau ne prenne le clavier
    let window = context::focused_window();
    let chips = context::gather(window.as_ref());

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<App> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let (width, height) = ((PW + 2.0 * MARGIN) as u32, (PH + 2.0 * MARGIN) as u32);
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("odysseus-assistant"),
        None,
    );
    // En haut à droite, sous son icône de la barre, comme les autres panneaux
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_size(width, height);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.commit();

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");
    let (events, rx) = channel::channel::<(u64, Event)>();
    event_loop
        .handle()
        .insert_source(rx, |event, _, app| {
            if let ChannelEvent::Msg((generation, e)) = event {
                app.on_event(generation, e);
            }
        })
        .expect("canal de l'agent");
    let (drops, drop_rx) = channel::channel::<Vec<PathBuf>>();
    event_loop
        .handle()
        .insert_source(drop_rx, |event, _, app| {
            if let ChannelEvent::Msg(paths) = event {
                app.files.extend(paths.into_iter().filter(|p| p.exists()));
                app.draw();
            }
        })
        .expect("canal des dépôts");
    // Outil en cours : ses points s'allument tour à tour (rien à dessiner sinon)
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_millis(300)),
            |_, _, app| {
                let tool_running = app.items.iter().any(|i| {
                    matches!(
                        i,
                        Item::Tool {
                            state: ToolState::Running,
                            ..
                        }
                    )
                });
                if app.job.is_some() && tool_running {
                    app.draw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(300))
            },
        )
        .expect("minuterie");

    let store = Store::open()
        .map_err(|e| eprintln!("odysseus-assistant : fil des conversations : {e}"))
        .ok();
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: Some(SlotPool::new(4 * 1024 * 1024, &shm).expect("pool shm")),
        shm,
        layer,
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        data_manager: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        events,
        drops,
        agent: AgentConfig::load(),
        items: Vec::new(),
        input: String::new(),
        files,
        chips,
        window,
        anchor,
        copied: None,
        last_question: String::new(),
        job: None,
        generation: 0,
        context: Context::default(),
        store,
        conversation: None,
        recent: Vec::new(),
        history: None,
        trash: Vec::new(),
        scroll: 0.0,
        content_h: 0.0,
        hits: Vec::new(),
        hover: None,
        drop_hover: false,
        frame_pending: false,
        dirty: false,
        background: None,
        width,
        height,
        scale: 1,
        configured: false,
        exit: false,
    };
    app.refresh_recent();
    // Question passée en argument (lanceur) : posée aussitôt
    if !question.is_empty() {
        app.input = question;
        app.send();
    }
    while !app.exit && !TERM.load(std::sync::atomic::Ordering::Relaxed) {
        if event_loop
            .dispatch(Some(Duration::from_millis(250)), &mut app)
            .is_err()
        {
            break;
        }
    }
    // Fermé pendant une réponse (Super+A, lanceur) : le fil garde ce qui a été dit
    app.abandon();
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Met un texte dans le presse-papiers (wl-copy le sert après la fermeture)
fn wl_copy(text: &str) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("wl-copy : {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("wl-copy : {e}"))?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("wl-copy a échoué".into())
    }
}

/// Markdown simplifié : gras et code retirés, puces rendues « • »
fn plain(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            let indent = &l[..l.len() - t.len()];
            let t = t.trim_start_matches('#').trim_start_matches(' ');
            let t = if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                format!("{indent}• {rest}")
            } else if l.trim_start().starts_with('#') {
                t.to_string()
            } else {
                format!("{indent}{t}")
            };
            t.replace("**", "").replace("__", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Nom de l'agent affiché en haut
fn agent_name(a: &AgentConfig) -> String {
    match a.agent.as_str() {
        "ollama" => format!("Ollama · {}", a.ollama_model),
        "commande" => "Commande personnalisée".into(),
        _ => "Claude Code".into(),
    }
}

/// Nom court d'un agent du fil (claude, ollama, commande)
fn agent_label(agent: &str) -> &'static str {
    match agent {
        "ollama" => "Ollama",
        "commande" => "Commande",
        _ => "Claude Code",
    }
}

/// « aujourd'hui à 14:32 », « hier à 9:05 », « 16 oct. », « 3 mars 2025 »
fn when(t: i64) -> String {
    const MOIS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    let Some(d) = DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&Local)) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    match (today - d.date_naive()).num_days() {
        0 => format!("aujourd'hui à {}:{:02}", d.hour(), d.minute()),
        1 => format!("hier à {}:{:02}", d.hour(), d.minute()),
        _ if d.year() == today.year() => format!("{} {}", d.day(), MOIS[d.month0() as usize]),
        _ => format!("{} {} {}", d.day(), MOIS[d.month0() as usize], d.year()),
    }
}

impl App {
    fn send(&mut self) {
        if self.job.is_some() {
            return;
        }
        let question = self.input.trim().to_string();
        if question.is_empty() && self.files.is_empty() && self.chips.is_empty() {
            return;
        }
        // L'agent choisi peut avoir changé (Réglages, agent.json)
        let agent = AgentConfig::load();
        if agent.agent != self.agent.agent {
            self.context = Context::default();
        }
        self.agent = agent;
        let files: Vec<PathBuf> = std::mem::take(&mut self.files);
        // Les pastilles partent avec la question, une fois
        let chips: Vec<Chip> = std::mem::take(&mut self.chips);
        self.items.push(Item::User {
            text: question.clone(),
            files: files
                .iter()
                .map(|f| {
                    f.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect(),
            context: chips.iter().map(|c| c.kind.label().to_string()).collect(),
        });
        self.last_question = question.clone();
        self.input.clear();
        self.scroll = 0.0;
        // La question est gardée avant la réponse : fermé entre-temps, rien n'est perdu
        self.persist();
        self.generation += 1;
        let ask = Ask {
            question,
            attachments: files,
            context: chips.iter().map(|c| c.text.clone()).collect(),
            dirs: chips.iter().filter_map(|c| c.dir.clone()).collect(),
        };
        self.job = Some(backend::ask(
            self.agent.clone(),
            ask,
            self.context.clone(),
            self.conversation,
            self.generation,
            self.events.clone(),
        ));
        self.draw();
    }

    fn stop(&mut self) {
        if let Some(job) = self.job.as_ref() {
            job.stop();
        }
    }

    /// La réponse en cours, s'il y en a une, est abandonnée et le fil garde
    /// la conversation telle quelle ; ce que la réponse enverra encore ne
    /// sera pas écouté
    fn abandon(&mut self) {
        if let Some(job) = self.job.take() {
            job.stop();
            self.settle_tools();
            self.items.push(Item::Error("Interrompu".into()));
            self.persist();
        }
        self.generation += 1;
    }

    /// La réponse s'arrête là : les outils encore en cours le disent
    fn settle_tools(&mut self) {
        for item in &mut self.items {
            if let Item::Tool { state, .. } = item
                && *state == ToolState::Running
            {
                *state = ToolState::Interrupted;
            }
        }
    }

    // --- Une réponse, vers le presse-papiers ou la fenêtre ----------------------------

    /// Le texte d'une réponse, débarrassé de ses marques Markdown
    fn answer_text(&self, k: usize) -> Option<String> {
        match self.items.get(k) {
            Some(Item::Answer(t)) => Some(plain(t)),
            _ => None,
        }
    }

    fn copy_answer(&mut self, k: usize) {
        let Some(text) = self.answer_text(k) else {
            return;
        };
        match wl_copy(&text) {
            Ok(()) => {
                self.copied = Some((k, Instant::now()));
                // Le bouton redit « Copier » deux secondes plus tard
                let _ = self.loop_handle.insert_source(
                    Timer::from_duration(Duration::from_millis(2200)),
                    |_, _, app| {
                        app.draw();
                        TimeoutAction::Drop
                    },
                );
            }
            Err(e) => self.items.push(Item::Notice(e)),
        }
        self.draw();
    }

    /// Insérer : le texte va au presse-papiers, puis le compositeur colle
    /// (Ctrl+V) dans la fenêtre d'où l'on est venu
    fn insert_answer(&mut self, k: usize) {
        let Some(text) = self.answer_text(k) else {
            return;
        };
        let Some(id) = self.window.as_ref().map(|w| w.id) else {
            return;
        };
        let done = wl_copy(&text).and_then(|()| {
            // wl-copy vient de prendre la sélection : un instant avant de coller
            std::thread::sleep(Duration::from_millis(80));
            common::ipc::command(&format!("paste {id}"))
        });
        if let Err(e) = done {
            self.items.push(Item::Notice(format!(
                "Impossible d'insérer dans la fenêtre ({e}) : la réponse est dans le presse-papiers."
            )));
        }
        self.draw();
    }

    fn new_conversation(&mut self) {
        self.abandon();
        self.items.clear();
        self.context = Context::default();
        self.conversation = None;
        self.history = None;
        self.scroll = 0.0;
        self.refresh_recent();
        self.draw();
    }

    // --- Le fil ---------------------------------------------------------------------

    /// La conversation en cours, telle qu'elle est, dans le fil
    fn persist(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let messages: Vec<Message> = self.items.iter().filter_map(message_from).collect();
        if messages.is_empty() {
            return;
        }
        let id = match self.conversation {
            Some(id) => id,
            None => match store.create(&self.agent.agent) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("odysseus-assistant : fil : {e}");
                    return;
                }
            },
        };
        self.conversation = Some(id);
        let c = Conversation {
            agent: self.agent.agent.clone(),
            claude_session: self.context.claude_session.clone(),
            ollama_history: self.context.ollama_history.clone(),
            messages,
        };
        if let Err(e) = store.save(id, &c) {
            eprintln!("odysseus-assistant : fil : {e}");
        }
    }

    fn refresh_recent(&mut self) {
        self.recent = self
            .store
            .as_ref()
            .and_then(|s| s.list("").ok())
            .unwrap_or_default()
            .into_iter()
            .take(RECENT)
            .collect();
    }

    /// Reprendre une conversation du fil, avec ce qu'il faut au modèle pour
    /// la continuer si c'est encore le même agent
    fn open_conversation(&mut self, id: i64) {
        let Some(c) = self.store.as_ref().and_then(|s| s.load(id).ok().flatten()) else {
            return;
        };
        self.abandon();
        self.history = None;
        self.agent = AgentConfig::load();
        self.conversation = Some(id);
        self.items = c.messages.iter().map(item_from).collect();
        if c.agent == self.agent.agent {
            self.context = Context {
                claude_session: c.claude_session,
                ollama_history: c.ollama_history,
            };
        } else {
            self.context = Context::default();
            self.items.push(Item::Notice(format!(
                "Conversation menée avec {} : {} la reprend sans en connaître le détail.",
                agent_label(&c.agent),
                agent_name(&self.agent)
            )));
        }
        self.scroll = 0.0;
        self.draw();
    }

    /// Supprimée du fil, mais gardée en mémoire le temps d'un Ctrl+Z
    fn delete_conversation(&mut self, id: i64) {
        if self.conversation == Some(id) {
            // C'est celle qu'on lit : le panneau repart à vide
            self.abandon();
            self.items.clear();
            self.context = Context::default();
            self.conversation = None;
            self.scroll = 0.0;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let kept = store.load(id).ok().flatten();
        if let Err(e) = store.delete(id) {
            eprintln!("odysseus-assistant : fil : {e}");
            return;
        }
        if let Some(c) = kept {
            self.trash.push(c);
        }
        self.refresh_recent();
        if let Some(h) = self.history.as_mut() {
            h.rows.retain(|r| r.id != id);
            h.selected = h.selected.min(h.rows.len().saturating_sub(1));
        }
        self.draw();
    }

    /// Ctrl+Z dans la liste : la dernière conversation supprimée revient
    fn restore_conversation(&mut self) {
        let Some(c) = self.trash.pop() else {
            return;
        };
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(e) = store.create(&c.agent).and_then(|id| store.save(id, &c)) {
            eprintln!("odysseus-assistant : fil : {e}");
        }
        self.refresh_recent();
        self.refresh_history();
        self.draw();
    }

    fn show_history(&mut self) {
        if self.store.is_none() {
            return;
        }
        self.history = Some(History {
            search: String::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0.0,
        });
        self.refresh_history();
        self.draw();
    }

    fn hide_history(&mut self) {
        self.history = None;
        self.draw();
    }

    /// La liste suit la recherche
    fn refresh_history(&mut self) {
        let Some(h) = self.history.as_mut() else {
            return;
        };
        h.rows = self
            .store
            .as_ref()
            .and_then(|s| s.list(&h.search).ok())
            .unwrap_or_default();
        h.selected = 0;
        h.scroll = 0.0;
    }

    /// Déplacer la sélection de la liste, en la gardant visible
    fn select_history(&mut self, delta: i32) {
        let Some(h) = self.history.as_mut() else {
            return;
        };
        if h.rows.is_empty() {
            return;
        }
        let last = h.rows.len() as i32 - 1;
        h.selected = (h.selected as i32 + delta).clamp(0, last) as usize;
        let view = PH - HEADER_H - INPUT_H - 44.0;
        let top = h.selected as f32 * ROW_H;
        if top < h.scroll {
            h.scroll = top;
        } else if top + ROW_H > h.scroll + view {
            h.scroll = top + ROW_H - view;
        }
    }

    fn on_event(&mut self, generation: u64, e: Event) {
        if generation != self.generation {
            return;
        }
        match e {
            Event::Delta(t) => match self.items.last_mut() {
                Some(Item::Answer(text)) => text.push_str(&t),
                _ => self.items.push(Item::Answer(t)),
            },
            Event::Break => {
                if let Some(Item::Answer(text)) = self.items.last_mut() {
                    text.push_str("\n\n");
                }
            }
            Event::Tool(label) => self.items.push(Item::Tool {
                label,
                state: ToolState::Running,
            }),
            Event::ToolDone(ok) => {
                if let Some(Item::Tool { state, .. }) = self.items.iter_mut().rev().find(|i| {
                    matches!(
                        i,
                        Item::Tool {
                            state: ToolState::Running,
                            ..
                        }
                    )
                }) {
                    *state = if ok {
                        ToolState::Done
                    } else {
                        ToolState::Refused
                    };
                }
            }
            Event::Session(id) => self.context.claude_session = Some(id),
            Event::History(h) => self.context.ollama_history = h,
            Event::Notice(t) => self.items.push(Item::Notice(t)),
            Event::Done(result) => {
                self.job = None;
                self.settle_tools();
                if let Err(e) = result {
                    self.items.push(Item::Error(e));
                }
                // Réponse terminée : les blancs de fin ne comptent pas
                if let Some(Item::Answer(text)) = self.items.last_mut() {
                    let trimmed = text.trim_end().len();
                    text.truncate(trimmed);
                }
                self.persist();
            }
        }
        self.draw();
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .map(|(_, h)| h.clone())
    }

    fn click(&mut self, x: f32, y: f32) {
        match self.hit_at(x, y) {
            Some(Hit::Suggestion(i)) => {
                self.input = SUGGESTIONS[i].to_string();
                self.send();
            }
            Some(Hit::Send) => self.send(),
            Some(Hit::Stop) => self.stop(),
            Some(Hit::New) => self.new_conversation(),
            Some(Hit::Close) => self.exit = true,
            Some(Hit::RemoveFile(i)) => {
                if i < self.files.len() {
                    self.files.remove(i);
                }
                self.draw();
            }
            Some(Hit::RemoveChip(i)) => {
                if i < self.chips.len() {
                    self.chips.remove(i);
                }
                self.draw();
            }
            Some(Hit::CopyAnswer(k)) => self.copy_answer(k),
            Some(Hit::InsertAnswer(k)) => self.insert_answer(k),
            Some(Hit::History) => self.show_history(),
            Some(Hit::Back) => self.hide_history(),
            Some(Hit::Open(id)) => self.open_conversation(id),
            Some(Hit::Delete(id)) => self.delete_conversation(id),
            None => {}
        }
    }

    /// Clavier dans la liste des conversations : la saisie cherche, ↑ ↓
    /// choisissent, Entrée reprend, Suppr supprime, Échap revient
    fn history_key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        // Une page : ce que la liste montre d'un coup
        let page = ((PH - HEADER_H - INPUT_H - 44.0) / ROW_H) as i32;
        match event.keysym {
            Keysym::Escape => self.hide_history(),
            Keysym::h if ctrl => self.hide_history(),
            Keysym::l if ctrl => self.new_conversation(),
            Keysym::z if ctrl => self.restore_conversation(),
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(id) = self
                    .history
                    .as_ref()
                    .and_then(|h| h.rows.get(h.selected))
                    .map(|r| r.id)
                {
                    self.open_conversation(id);
                }
            }
            Keysym::Delete => {
                if let Some(id) = self
                    .history
                    .as_ref()
                    .and_then(|h| h.rows.get(h.selected))
                    .map(|r| r.id)
                {
                    self.delete_conversation(id);
                }
            }
            Keysym::Up => {
                self.select_history(-1);
                self.draw();
            }
            Keysym::Down => {
                self.select_history(1);
                self.draw();
            }
            Keysym::Prior => {
                self.select_history(-page);
                self.draw();
            }
            Keysym::Next => {
                self.select_history(page);
                self.draw();
            }
            Keysym::BackSpace => {
                if let Some(h) = self.history.as_mut() {
                    if ctrl {
                        let trimmed = h.search.trim_end().len();
                        h.search.truncate(trimmed);
                        let cut = h.search.rfind(' ').map(|i| i + 1).unwrap_or(0);
                        h.search.truncate(cut);
                    } else {
                        h.search.pop();
                    }
                }
                self.refresh_history();
                self.draw();
            }
            _ => {
                if ctrl || self.modifiers.alt || self.modifiers.logo {
                    return;
                }
                let Some(t) = event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                else {
                    return;
                };
                if let Some(h) = self.history.as_mut() {
                    h.search.push_str(&t);
                }
                self.refresh_history();
                self.draw();
            }
        }
    }

    fn key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        if self.history.is_some() {
            self.history_key(event);
            return;
        }
        match event.keysym {
            Keysym::Escape if self.job.is_some() => self.stop(),
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.send(),
            Keysym::l if ctrl => self.new_conversation(),
            Keysym::h if ctrl => self.show_history(),
            Keysym::v if ctrl => {
                // Texte du presse-papiers, sur une ligne
                if let Ok(out) = Command::new("wl-paste")
                    .args(["--no-newline", "--type", "text/plain"])
                    .output()
                {
                    let t = String::from_utf8_lossy(&out.stdout).replace(['\n', '\r'], " ");
                    self.input.push_str(&t);
                }
            }
            Keysym::BackSpace if ctrl => {
                let trimmed = self.input.trim_end().len();
                self.input.truncate(trimmed);
                let cut = self.input.rfind(' ').map(|i| i + 1).unwrap_or(0);
                self.input.truncate(cut);
            }
            Keysym::BackSpace => {
                self.input.pop();
            }
            Keysym::Up if self.input.is_empty() => self.input = self.last_question.clone(),
            Keysym::Prior => self.scroll += 200.0,
            Keysym::Next => self.scroll = (self.scroll - 200.0).max(0.0),
            _ => {
                if ctrl || self.modifiers.alt || self.modifiers.logo {
                    return;
                }
                match event.utf8.filter(|t| t.chars().all(|c| !c.is_control())) {
                    Some(t) if !t.is_empty() => self.input.push_str(&t),
                    _ => return,
                }
            }
        }
        self.draw();
    }

    /// Avec `--contexte` : la bulle se place à droite de la fenêtre active
    /// s'il y a la place, sinon dans son coin supérieur droit ; sans fenêtre,
    /// elle reste au coin de l'écran, sous son icône de la barre
    fn place_near_window(&mut self, screen_w: i32, screen_h: i32) {
        let Some(w) = self.window.as_ref().filter(|_| self.anchor) else {
            return;
        };
        let (pw, ph) = (self.width as i32, self.height as i32);
        let right_edge = w.x + w.width;
        let margin_right = if right_edge + pw <= screen_w {
            screen_w - right_edge - pw
        } else {
            (screen_w - right_edge).max(0)
        };
        let margin_right = margin_right.clamp(0, (screen_w - pw).max(0));
        let margin_top = w.y.clamp(0, (screen_h - ph).max(0));
        // Les marges comptent depuis le bord de l'écran, pas depuis la barre
        self.layer.set_exclusive_zone(-1);
        self.layer.set_margin(margin_top, margin_right, 0, 0);
        self.layer.commit();
    }

    // --- Dessin -------------------------------------------------------------------

    /// Un dessin par rafraîchissement d'écran au plus (la réponse arrive par petits bouts)
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
        let Some(mut pool) = self.pool.take() else {
            return;
        };
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
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
        let p = theme::palette();
        let (x0, y0) = (MARGIN, MARGIN);
        let r = theme::PANEL_RADIUS * s;
        // Ombre et panneau : dessinés une fois (le plus coûteux d'une image)
        let key = (w, h, p.id);
        match &self.background {
            Some((k, px)) if *k == key => c.buf.copy_from_slice(px),
            _ => {
                c.clear();
                c.shadow(x0 * s, (y0 + 4.0) * s, PW * s, PH * s, r, 22.0 * s, 0.45);
                c.fill_rounded(x0 * s, y0 * s, PW * s, PH * s, r, p.panel.with_alpha(0.97));
                c.stroke_rounded(
                    x0 * s,
                    y0 * s,
                    PW * s,
                    PH * s,
                    r,
                    s.max(1.0),
                    p.panel_border(),
                );
                self.background = Some((key, c.buf.to_vec()));
            }
        }
        if self.drop_hover {
            c.stroke_rounded(x0 * s, y0 * s, PW * s, PH * s, r, 2.0 * s, p.accent);
        }
        let mut ui = Ui {
            c: &mut c,
            s,
            regular: &self.regular,
            bold: &self.bold,
            icons: &self.icons,
            hits: Vec::new(),
            hover: self.hover.clone(),
        };

        // Conversation, sous l'en-tête et au-dessus de la saisie (et des pastilles)
        let top = y0 + HEADER_H;
        let rows = self.chip_rows(&ui);
        let bottom = y0 + PH - INPUT_H - rows.len() as f32 * CHIP_ROW;
        let (cx, cw) = (x0 + PAD, PW - 2.0 * PAD);
        if self.history.is_some() {
            self.history_view(&mut ui, cx, top, cw, bottom);
        } else if self.items.is_empty() {
            self.empty_state(&mut ui, cx, top, cw, bottom);
        } else {
            let lines = self.layout(&ui, cw);
            let total: f32 = lines.iter().map(|l| l.h).sum::<f32>() + 16.0;
            self.content_h = total;
            let view = bottom - top;
            let max_scroll = (total - view).max(0.0);
            self.scroll = self.scroll.min(max_scroll);
            // Collé en bas : la réponse reste visible pendant qu'elle s'écrit
            let mut y = bottom - total + self.scroll + 8.0;
            if total < view {
                y = top + 8.0;
            }
            for line in &lines {
                if y >= top - 2.0 && y + line.h <= bottom + 2.0 {
                    self.draw_line(&mut ui, line, cx, y, cw);
                }
                y += line.h;
            }
        }

        self.header(&mut ui, x0, y0);
        self.input_area(&mut ui, x0, y0, &rows);

        self.hits = ui.hits;
        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(&self.qh, surface.clone());
        self.frame_pending = true;
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
        self.pool = Some(pool);
    }

    fn header(&self, ui: &mut Ui, x0: f32, y0: f32) {
        let p = theme::palette();
        ui.icon(icon::BOT, x0 + 30.0, y0 + 28.0, 18.0, p.accent);
        ui.bold(x0 + 48.0, y0 + 10.0, 36.0, 15.0, "Assistant", p.text);
        let name = agent_name(&self.agent);
        let nx = x0 + 48.0 + ui.measure_bold("Assistant", 15.0) + 12.0;
        let nw = ui.measure(&name, 11.5) + 18.0;
        ui.rounded(nx, y0 + 17.0, nw, 22.0, 11.0, p.ink(0.08));
        ui.text_centered(&name, nx + nw / 2.0, y0 + 17.0, 22.0, 11.5, p.text_soft());
        // À droite : fermer, nouvelle conversation, les conversations précédentes
        let mut bx = x0 + PW - 44.0;
        ui.round_button(bx, y0 + 28.0, icon::CLOSE, Hit::Close, false);
        if !self.items.is_empty() {
            bx -= 36.0;
            ui.round_button(bx, y0 + 28.0, icon::ROTATE_CCW, Hit::New, false);
        }
        if self.store.is_some() {
            bx -= 36.0;
            if self.history.is_some() {
                ui.round_button(bx, y0 + 28.0, icon::CLOCK, Hit::Back, true);
            } else {
                ui.round_button(bx, y0 + 28.0, icon::CLOCK, Hit::History, false);
            }
        }
        ui.rect(x0 + 1.0, y0 + HEADER_H - 1.0, PW - 2.0, 1.0, p.ink(0.07));
    }

    /// La liste des conversations, à la place de la conversation
    fn history_view(&self, ui: &mut Ui, x: f32, top: f32, w: f32, bottom: f32) {
        let p = theme::palette();
        let Some(h) = self.history.as_ref() else {
            return;
        };
        ui.bold(x, top + 10.0, 28.0, 15.0, "Conversations", p.text);
        let count = match h.rows.len() {
            0 if h.search.is_empty() => "aucune".to_string(),
            0 => "aucune ne correspond".to_string(),
            1 => "une".to_string(),
            n => n.to_string(),
        };
        let cw = ui.measure(&count, 12.0);
        ui.text(x + w - cw, top + 10.0, 28.0, 12.0, &count, p.text_dim);
        let rows_top = top + 44.0;
        if h.rows.is_empty() {
            let msg = if h.search.is_empty() {
                "Vos conversations se retrouveront ici.".to_string()
            } else {
                format!("Rien ne contient « {} ».", h.search.trim())
            };
            ui.text_centered(&msg, x + w / 2.0, rows_top + 60.0, 20.0, 13.0, p.text_dim);
            return;
        }
        let mut y = rows_top - h.scroll;
        for (i, row) in h.rows.iter().enumerate() {
            // Lignes entières seulement, comme la conversation
            if y >= rows_top - 2.0 && y + ROW_H - 4.0 <= bottom + 2.0 {
                let hit = Hit::Open(row.id);
                let hovered =
                    ui.hover == Some(hit.clone()) || ui.hover == Some(Hit::Delete(row.id));
                let selected = i == h.selected;
                if selected {
                    ui.rounded(x, y, w, ROW_H - 4.0, 12.0, p.accent.with_alpha(0.14));
                } else if hovered {
                    ui.rounded(x, y, w, ROW_H - 4.0, 12.0, p.tile_hover());
                }
                ui.icon(
                    icon::CLOCK,
                    x + 20.0,
                    y + ROW_H / 2.0 - 2.0,
                    15.0,
                    p.text_dim,
                );
                let title = if row.title.is_empty() {
                    "Sans titre".to_string()
                } else {
                    row.title.clone()
                };
                let title = ui.truncate(&title, 14.0, w - 90.0);
                ui.text(x + 40.0, y + 6.0, LINE, 14.0, &title, p.text);
                let detail = format!(
                    "{} · {} · {}",
                    when(row.modified),
                    agent_label(&row.agent),
                    match row.questions {
                        1 => "une question".to_string(),
                        n => format!("{n} questions"),
                    }
                );
                ui.text(x + 40.0, y + 27.0, 18.0, 11.5, &detail, p.text_dim);
                ui.hit((x, y, w - 44.0, ROW_H - 4.0), hit);
                // Supprimer : visible sur la ligne survolée ou choisie
                if hovered || selected {
                    ui.round_button(
                        x + w - 24.0,
                        y + ROW_H / 2.0 - 2.0,
                        icon::TRASH,
                        Hit::Delete(row.id),
                        false,
                    );
                } else {
                    ui.hit((x + w - 44.0, y, 44.0, ROW_H - 4.0), Hit::Delete(row.id));
                }
            }
            y += ROW_H;
        }
    }

    fn empty_state(&self, ui: &mut Ui, x: f32, top: f32, w: f32, bottom: f32) {
        let p = theme::palette();
        let cy = top + (bottom - top) * 0.36;
        ui.icon(
            icon::SPARKLES,
            x + w / 2.0,
            cy - 30.0,
            34.0,
            p.accent.with_alpha(0.9),
        );
        ui.bold_centered(
            "Que voulez-vous savoir ?",
            x + w / 2.0,
            cy,
            32.0,
            19.0,
            p.text,
        );
        let detail = match self.agent.agent.as_str() {
            "ollama" => "Modèle local : vos questions ne quittent pas la machine.",
            "commande" => "Commande personnalisée (agent.json).",
            _ => "Claude Code, avec les outils du bureau que vous autorisez.",
        };
        ui.text_centered(detail, x + w / 2.0, cy + 30.0, 20.0, 12.5, p.text_dim);
        // Suggestions, deux par ligne
        let gap = 10.0;
        let sw = (w - gap) / 2.0;
        for (i, s) in SUGGESTIONS.iter().enumerate() {
            let (col, row) = (i % 2, i / 2);
            let (sx, sy) = (x + col as f32 * (sw + gap), cy + 74.0 + row as f32 * 50.0);
            let hit = Hit::Suggestion(i);
            let bg = if ui.hover == Some(hit.clone()) {
                p.tile_hover()
            } else {
                p.tile()
            };
            ui.rounded(sx, sy, sw, 40.0, 12.0, bg);
            let t = ui.truncate(s, 13.0, sw - 24.0);
            ui.text(sx + 14.0, sy, 40.0, 13.0, &t, p.text_soft());
            ui.hit((sx, sy, sw, 40.0), hit);
        }
        // Les dernières conversations, à reprendre
        if self.recent.is_empty() {
            return;
        }
        let mut y = cy + 74.0 + 2.0 * 50.0 + 12.0;
        ui.bold(x, y, 24.0, 12.0, "Reprendre", p.text_dim);
        let total = self.store.as_ref().map_or(0, Store::count);
        if total > self.recent.len() {
            let more = format!("toutes ({total})");
            let mw = ui.measure(&more, 12.0);
            let hovered = ui.hover == Some(Hit::History);
            ui.text(
                x + w - mw,
                y,
                24.0,
                12.0,
                &more,
                if hovered { p.text } else { p.text_dim },
            );
            ui.hit((x + w - mw - 8.0, y, mw + 8.0, 24.0), Hit::History);
        }
        y += 28.0;
        for row in &self.recent {
            let hit = Hit::Open(row.id);
            if ui.hover == Some(hit.clone()) {
                ui.rounded(x, y, w, 32.0, 10.0, p.tile_hover());
            }
            ui.icon(icon::CLOCK, x + 14.0, y + 16.0, 12.0, p.text_dim);
            let date = when(row.modified);
            let dw = ui.measure(&date, 11.5);
            ui.text(x + w - dw - 12.0, y, 32.0, 11.5, &date, p.text_dim);
            let title = if row.title.is_empty() {
                "Sans titre".to_string()
            } else {
                row.title.clone()
            };
            let title = ui.truncate(&title, 13.0, w - dw - 50.0);
            ui.text(x + 30.0, y, 32.0, 13.0, &title, p.text_soft());
            ui.hit((x, y, w, 32.0), hit);
            y += 34.0;
        }
    }

    /// Les pastilles au-dessus de la saisie, en rangées : le contexte, puis
    /// les fichiers joints ; chacune avec son icône, son texte, sa croix
    fn chip_rows(&self, ui: &Ui) -> Vec<Vec<(char, String, f32, Hit)>> {
        let max_w = PW - 2.0 * PAD;
        let mut rows: Vec<Vec<(char, String, f32, Hit)>> = Vec::new();
        let mut row_w = 0.0;
        let mut push = |glyph: char, label: String, hit: Hit| {
            let label = ui.truncate(&label, 12.0, 230.0);
            let w = ui.measure(&label, 12.0) + 52.0;
            if rows.is_empty() || row_w + w > max_w {
                rows.push(Vec::new());
                row_w = 0.0;
            }
            row_w += w + 8.0;
            if let Some(last) = rows.last_mut() {
                last.push((glyph, label, w, hit));
            }
        };
        for (i, c) in self.chips.iter().enumerate() {
            let glyph = match c.kind {
                context::Kind::Window => icon::APP,
                context::Kind::Selection => icon::PENCIL,
                context::Kind::Clipboard => icon::COPY,
                context::Kind::Folder => icon::FOLDER,
            };
            push(glyph, c.label.clone(), Hit::RemoveChip(i));
        }
        for (i, f) in self.files.iter().enumerate() {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            push(icon::LINK, name, Hit::RemoveFile(i));
        }
        rows
    }

    fn input_area(&self, ui: &mut Ui, x0: f32, y0: f32, rows: &[Vec<(char, String, f32, Hit)>]) {
        let p = theme::palette();
        let mut y = y0 + PH - INPUT_H + 12.0;
        // Pastilles : contexte (teinte neutre) et fichiers joints (teinte d'accent)
        if !rows.is_empty() {
            y -= rows.len() as f32 * CHIP_ROW;
            for row in rows {
                let mut fx = x0 + PAD;
                for (glyph, label, w, hit) in row {
                    let is_file = matches!(hit, Hit::RemoveFile(_));
                    let (bg, fg) = if is_file {
                        (p.accent.with_alpha(0.16), p.accent)
                    } else {
                        (p.ink(0.08), p.text_soft())
                    };
                    ui.rounded(fx, y, *w, 24.0, 12.0, bg);
                    ui.icon(*glyph, fx + 14.0, y + 12.0, 11.0, fg);
                    ui.text(fx + 24.0, y, 24.0, 12.0, label, p.text);
                    ui.icon(icon::CLOSE, fx + w - 13.0, y + 12.0, 10.0, p.text_dim);
                    ui.hit((fx + w - 24.0, y, 24.0, 24.0), hit.clone());
                    fx += w + 8.0;
                }
                y += CHIP_ROW;
            }
        }
        let (fx, fw, fh) = (x0 + PAD, PW - 2.0 * PAD, 46.0);
        ui.rounded(fx, y, fw, fh, 14.0, p.ink(0.07));
        ui.stroke(fx, y, fw, fh, 14.0, p.accent.with_alpha(0.45));
        let tw = fw - 70.0;
        // Liste des conversations : la saisie cherche
        if let Some(h) = self.history.as_ref() {
            ui.icon(icon::SEARCH, fx + 22.0, y + fh / 2.0, 14.0, p.text_dim);
            if h.search.is_empty() {
                let hint = "Rechercher dans les conversations…";
                ui.text(fx + 40.0, y, fh, 14.0, hint, p.text_dim);
            } else {
                let mut shown = h.search.clone();
                while ui.measure(&shown, 14.0) > tw - 24.0 && !shown.is_empty() {
                    shown.remove(0);
                }
                ui.text(fx + 40.0, y, fh, 14.0, &shown, p.text);
                let cx = fx + 40.0 + ui.measure(&shown, 14.0) + 1.0;
                ui.rect(cx, y + 13.0, 1.5, 20.0, p.accent);
            }
            ui.round_button(fx + fw - 26.0, y + fh / 2.0, icon::CLOSE, Hit::Back, false);
            let hint = "Entrée : reprendre · ↑ ↓ : choisir · Suppr : supprimer, Ctrl+Z : rétablir · Échap : retour";
            ui.text_centered(hint, x0 + PW / 2.0, y + fh + 4.0, 22.0, 11.0, p.text_dim);
            return;
        }
        if self.input.is_empty() {
            let hint = if self.job.is_some() {
                "Réponse en cours… Échap pour l'arrêter"
            } else {
                "Demandez quelque chose à votre bureau…"
            };
            ui.text(fx + 16.0, y, fh, 14.0, hint, p.text_dim);
        } else {
            // Texte long : on montre la fin, là où l'on tape
            let mut shown = self.input.clone();
            while ui.measure(&shown, 14.0) > tw && !shown.is_empty() {
                shown.remove(0);
            }
            ui.text(fx + 16.0, y, fh, 14.0, &shown, p.text);
            let cx = fx + 16.0 + ui.measure(&shown, 14.0) + 1.0;
            ui.rect(cx, y + 13.0, 1.5, 20.0, p.accent);
        }
        let (bx, by) = (fx + fw - 26.0, y + fh / 2.0);
        if self.job.is_some() {
            ui.round_button(bx, by, icon::CLOSE, Hit::Stop, true);
        } else {
            ui.round_button(bx, by, icon::ARROW_UP, Hit::Send, true);
        }
        let hint = "Entrée : envoyer · Échap : fermer · glissez un fichier pour le joindre";
        ui.text_centered(hint, x0 + PW / 2.0, y + fh + 4.0, 22.0, 11.0, p.text_dim);
    }

    /// Lignes de la conversation, prêtes à dessiner
    fn layout(&self, ui: &Ui, w: f32) -> Vec<Line> {
        let mut lines = Vec::new();
        for (k, item) in self.items.iter().enumerate() {
            match item {
                Item::User {
                    text,
                    files,
                    context,
                } => {
                    if k > 0 {
                        lines.push(Line::gap(14.0));
                    }
                    let bw = w * 0.72;
                    let mut wrapped = Vec::new();
                    for para in text.split('\n') {
                        wrapped.extend(ui.wrap(para, 14.0, bw - 28.0));
                    }
                    if !context.is_empty() {
                        wrapped.push(format!("Contexte : {}", context.join(", ")));
                    }
                    if !files.is_empty() {
                        wrapped.push(format!("Joint : {}", files.join(", ")));
                    }
                    let n = wrapped.len().max(1);
                    let widest = wrapped
                        .iter()
                        .map(|l| ui.measure(l, 14.0))
                        .fold(0.0f32, f32::max);
                    lines.push(Line {
                        kind: LineKind::Bubble(wrapped, widest + 28.0),
                        h: n as f32 * LINE + 18.0,
                    });
                    lines.push(Line::gap(10.0));
                }
                Item::Answer(text) => {
                    for para in plain(text).split('\n') {
                        if para.trim().is_empty() {
                            lines.push(Line::gap(8.0));
                            continue;
                        }
                        for l in ui.wrap(para, 14.0, w) {
                            lines.push(Line {
                                kind: LineKind::Text(l),
                                h: LINE,
                            });
                        }
                    }
                    // Réponse finie : la copier, l'insérer dans la fenêtre d'où l'on vient
                    let streaming = self.job.is_some() && k + 1 == self.items.len();
                    if !streaming && !text.trim().is_empty() {
                        lines.push(Line {
                            kind: LineKind::Actions(k),
                            h: 34.0,
                        });
                    }
                }
                Item::Tool { label, state } => lines.push(Line {
                    kind: LineKind::Tool(label.clone(), *state),
                    h: 28.0,
                }),
                Item::Error(e) => {
                    for l in ui.wrap(e, 13.0, w - 24.0) {
                        lines.push(Line {
                            kind: LineKind::Error(l),
                            h: LINE,
                        });
                    }
                }
                Item::Notice(t) => {
                    lines.push(Line::gap(6.0));
                    for l in ui.wrap(t, 12.5, w - 24.0) {
                        lines.push(Line {
                            kind: LineKind::Notice(l),
                            h: LINE,
                        });
                    }
                }
            }
        }
        lines
    }

    fn draw_line(&self, ui: &mut Ui, line: &Line, x: f32, y: f32, w: f32) {
        let p = theme::palette();
        match &line.kind {
            LineKind::Gap => {}
            LineKind::Bubble(text, bw) => {
                let bx = x + w - bw;
                ui.rounded(bx, y, *bw, line.h, 16.0, p.accent.with_alpha(0.2));
                for (i, l) in text.iter().enumerate() {
                    ui.text(bx + 14.0, y + 9.0 + i as f32 * LINE, LINE, 14.0, l, p.text);
                }
            }
            LineKind::Text(l) => ui.text(x, y, LINE, 14.0, l, p.text),
            LineKind::Tool(label, state) => {
                let (glyph, color, suffix) = match state {
                    ToolState::Running => {
                        // Trois points qui s'allument tour à tour
                        let t = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() / 300 % 4)
                            .unwrap_or(0);
                        (icon::LOADER, p.accent, ".".repeat(t as usize))
                    }
                    ToolState::Done => (icon::CHECK, p.success, String::new()),
                    ToolState::Refused => (icon::CLOSE, p.danger, " (refusé)".to_string()),
                    ToolState::Interrupted => {
                        (icon::CLOSE, p.text_dim, " (interrompu)".to_string())
                    }
                };
                ui.rounded(
                    x,
                    y + 3.0,
                    ui.measure(label, 12.0) + 60.0,
                    22.0,
                    11.0,
                    p.ink(0.06),
                );
                ui.icon(glyph, x + 14.0, y + 14.0, 11.0, color);
                ui.text(
                    x + 26.0,
                    y + 3.0,
                    22.0,
                    12.0,
                    &format!("{label}{suffix}"),
                    p.text_soft(),
                );
            }
            LineKind::Error(l) => {
                ui.icon(icon::INFO, x + 8.0, y + LINE / 2.0, 12.0, p.danger);
                ui.text(x + 22.0, y, LINE, 13.0, l, p.danger);
            }
            LineKind::Notice(l) => {
                ui.icon(icon::INFO, x + 8.0, y + LINE / 2.0, 12.0, p.text_dim);
                ui.text(x + 22.0, y, LINE, 12.5, l, p.text_dim);
            }
            LineKind::Actions(k) => {
                let copied = self
                    .copied
                    .is_some_and(|(i, at)| i == *k && at.elapsed() < Duration::from_secs(2));
                let mut bx = x;
                let (glyph, label) = if copied {
                    (icon::CHECK, "Copié".to_string())
                } else {
                    (icon::COPY, "Copier".to_string())
                };
                bx += ui.pill(bx, y + 6.0, glyph, &label, Hit::CopyAnswer(*k)) + 8.0;
                if let Some(w) = self.window.as_ref() {
                    let label = ui.truncate(&format!("Insérer dans {}", w.name), 12.0, 260.0);
                    ui.pill(
                        bx,
                        y + 6.0,
                        icon::ARROW_RIGHT,
                        &label,
                        Hit::InsertAnswer(*k),
                    );
                }
            }
        }
    }
}

enum LineKind {
    Gap,
    Bubble(Vec<String>, f32),
    Text(String),
    Tool(String, ToolState),
    Error(String),
    Notice(String),
    /// Sous une réponse (indice dans la conversation) : Copier, Insérer
    Actions(usize),
}

struct Line {
    kind: LineKind,
    h: f32,
}

impl Line {
    fn gap(h: f32) -> Self {
        Self {
            kind: LineKind::Gap,
            h,
        }
    }
}

/// Dessin en pixels logiques
struct Ui<'a, 'b> {
    c: &'a mut Canvas<'b>,
    s: f32,
    regular: &'a Text,
    bold: &'a Text,
    icons: &'a Text,
    hits: Vec<((f32, f32, f32, f32), Hit)>,
    hover: Option<Hit>,
}

#[allow(clippy::too_many_arguments)]
impl Ui<'_, '_> {
    fn hit(&mut self, r: (f32, f32, f32, f32), h: Hit) {
        self.hits.push((r, h));
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        let s = self.s;
        self.c.fill_rect(x * s, y * s, w * s, h * s, c);
    }
    fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let s = self.s;
        self.c.fill_rounded(x * s, y * s, w * s, h * s, r * s, c);
    }
    fn stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let s = self.s;
        self.c
            .stroke_rounded(x * s, y * s, w * s, h * s, r * s, s.max(1.0), c);
    }
    fn icon(&mut self, g: char, cx: f32, cy: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.icons.icon(self.c, g, cx * s, cy * s, px * s, c);
    }
    fn measure(&self, t: &str, px: f32) -> f32 {
        self.regular.measure(t, px * self.s) / self.s
    }
    fn measure_bold(&self, t: &str, px: f32) -> f32 {
        self.bold.measure(t, px * self.s) / self.s
    }
    fn truncate(&self, t: &str, px: f32, max: f32) -> String {
        self.regular.truncate(t, px * self.s, max * self.s)
    }
    fn wrap(&self, t: &str, px: f32, max: f32) -> Vec<String> {
        if t.is_empty() {
            return vec![String::new()];
        }
        self.regular.wrap(t, px * self.s, max * self.s, 10_000)
    }
    fn text(&mut self, x: f32, y: f32, h: f32, px: f32, t: &str, c: Rgba) {
        let s = self.s;
        self.regular
            .draw_in(self.c, t, x * s, y * s, h * s, px * s, c);
    }
    fn bold(&mut self, x: f32, y: f32, h: f32, px: f32, t: &str, c: Rgba) {
        let s = self.s;
        self.bold.draw_in(self.c, t, x * s, y * s, h * s, px * s, c);
    }
    fn text_centered(&mut self, t: &str, cx: f32, y: f32, h: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.regular
            .draw_centered(self.c, t, cx * s, y * s, h * s, px * s, c);
    }
    fn bold_centered(&mut self, t: &str, cx: f32, y: f32, h: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.bold
            .draw_centered(self.c, t, cx * s, y * s, h * s, px * s, c);
    }
    /// Petit bouton à icône et texte ; renvoie sa largeur
    fn pill(&mut self, x: f32, y: f32, glyph: char, label: &str, hit: Hit) -> f32 {
        let p = theme::palette();
        let hovered = self.hover == Some(hit.clone());
        let w = self.measure(label, 12.0) + 40.0;
        self.rounded(
            x,
            y,
            w,
            24.0,
            12.0,
            if hovered { p.tile_hover() } else { p.ink(0.07) },
        );
        self.icon(glyph, x + 14.0, y + 12.0, 11.0, p.text_soft());
        self.text(x + 26.0, y, 24.0, 12.0, label, p.text_soft());
        self.hit((x, y, w, 24.0), hit);
        w
    }

    /// Bouton rond à icône (plein : accent)
    fn round_button(&mut self, cx: f32, cy: f32, glyph: char, hit: Hit, filled: bool) {
        let p = theme::palette();
        let hovered = self.hover == Some(hit.clone());
        let (bg, fg) = if filled {
            (
                if hovered { p.accent_hover() } else { p.accent },
                p.on_accent,
            )
        } else if hovered {
            (p.tile_hover(), p.text)
        } else {
            (p.ink(0.0), p.text_dim)
        };
        let s = self.s;
        self.c.fill_circle(cx * s, cy * s, 15.0 * s, bg);
        self.icon(glyph, cx, cy, 14.0, fg);
        self.hit((cx - 16.0, cy - 16.0, 32.0, 32.0), hit);
    }
}

// --- Wayland -----------------------------------------------------------------------

impl LayerShellHandler for App {
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

impl CompositorHandler for App {
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

impl KeyboardHandler for App {
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

impl PointerHandler for App {
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
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => self.click(x, y),
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let hover = self.hit_at(x, y);
                    if hover != self.hover {
                        self.hover = hover;
                        self.draw();
                    }
                }
                PointerEventKind::Leave { .. } if self.hover.take().is_some() => self.draw(),
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    let step = vertical.absolute as f32 * 1.5;
                    if let Some(h) = self.history.as_mut() {
                        let view = PH - HEADER_H - INPUT_H - 44.0;
                        let max = (h.rows.len() as f32 * ROW_H - view).max(0.0);
                        h.scroll = (h.scroll + step).clamp(0.0, max);
                    } else {
                        let max = (self.content_h - (PH - HEADER_H - INPUT_H)).max(0.0);
                        self.scroll = (self.scroll - step).clamp(0.0, max);
                    }
                    self.draw();
                }
                _ => {}
            }
        }
    }
}

impl SeatHandler for App {
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
        if self.data_device.is_none()
            && let Some(m) = self.data_manager.as_ref()
        {
            self.data_device = Some(m.get_data_device(qh, &seat));
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app, _, event| app.key(event)),
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

fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

/// Fichiers glissés sur le panneau (depuis Fichiers) : pièces jointes
impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _: f64,
        _: f64,
        _: &wl_surface::WlSurface,
    ) {
        let Some(offer) = drag_offer(device) else {
            return;
        };
        if offer.with_mime_types(|m| m.iter().any(|t| t == "text/uri-list")) {
            offer.accept_mime_type(offer.serial, Some("text/uri-list".into()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
            self.drop_hover = true;
        } else {
            offer.accept_mime_type(offer.serial, None);
        }
        self.draw();
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        if std::mem::take(&mut self.drop_hover) {
            self.draw();
        }
    }
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, conn: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        self.drop_hover = false;
        if let Some(offer) = drag_offer(device) {
            if let Ok(pipe) = offer.receive("text/uri-list".into()) {
                let _ = conn.flush();
                // Pas into_raw_fd() : celui de smithay-client-toolkit 0.19 ferme le descripteur au passage
                let mut file = File::from(std::os::fd::OwnedFd::from(pipe));
                let tx = self.drops.clone();
                std::thread::spawn(move || {
                    let mut text = String::new();
                    if file.read_to_string(&mut text).is_ok() {
                        let _ = tx.send(uri_paths(&text));
                    }
                });
            }
            offer.finish();
            offer.destroy();
        }
        self.draw();
    }
}

/// « file:///home/d/Mon%20fichier.png » → chemins
fn uri_paths(text: &str) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("file://"))
        .map(|p| {
            let b = p.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%'
                    && i + 2 < b.len()
                    && let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16)
                {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            PathBuf::from(std::ffi::OsString::from_vec(out))
        })
        .collect()
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if let Some((sw, sh)) = self.output_state.info(&output).and_then(|i| i.logical_size) {
            self.place_near_window(sw, sh);
        }
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_data_device!(App);
delegate_registry!(App);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown() {
        assert_eq!(
            plain("## Titre\n- **un** point\n`code`"),
            "Titre\n• un point\ncode"
        );
    }
}
