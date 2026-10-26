//! Réglages d'odysseus-desktop : une fenêtre, une barre latérale, sept pages.
//! Apparence (thème), Fond d'écran (galerie, images déposées depuis Fichiers),
//! Écran (définition avec confirmation, fréquence, taille, lumière de nuit),
//! Clavier et langue, Énergie (extinction, verrouillage, veille), Raccourcis,
//! À propos. Tout s'applique aussitôt ; le compositeur relit les réglages.
//!
//! `odysseus-reglages [apparence|fond|ecran|clavier|energie|raccourcis|apropos]`

mod agents;
mod automatismes;
mod compte;
mod fonds;
mod maj;
mod roue;
mod system;
mod trousseau;

use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Image, Text},
    settings::Settings,
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window,
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
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use system::Screen;

// Mise en page, en pixels logiques
const SIDEBAR_W: f32 = 240.0;
const HEADER_H: f32 = 64.0;
/// Centre vertical des boutons de la fenêtre (en haut de la barre latérale)
const CHROME_Y: f32 = 24.0;
/// Décalage de la barre latérale, sous les boutons de la fenêtre
const SIDE_TOP: f32 = 30.0;
const CONTENT_MAX: f32 = 660.0;
const ROW_H: f32 = 52.0;
/// Débord de la zone cliquable d'un curseur, de chaque côté
const SLIDER_PAD: f32 = 10.0;
const BTN_LEFT: u32 = 0x110;
/// Délai pour garder une nouvelle définition, sinon retour à l'ancienne
const CONFIRM: Duration = Duration::from_secs(15);
const TOAST: Duration = Duration::from_secs(4);

const SCALES: [f64; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];
const SCREEN_OFF: [(u32, &str); 6] = [
    (2, "2 min"),
    (5, "5 min"),
    (10, "10 min"),
    (20, "20 min"),
    (30, "30 min"),
    (0, "Jamais"),
];
const REPEAT_DELAY: [(u32, &str); 5] = [
    (200, "Très court"),
    (300, "Court"),
    (450, "Moyen"),
    (650, "Long"),
    (900, "Très long"),
];
const REPEAT_RATE: [(u32, &str); 4] = [
    (15, "Lente"),
    (25, "Normale"),
    (35, "Rapide"),
    (50, "Très rapide"),
];
/// Agents proposés : réglage, nom, précision
const AGENTS: [(&str, &str, &str); 3] = [
    ("claude", "Claude Code", "Anthropic, en ligne"),
    (
        "ollama",
        "Ollama",
        "modèle local : rien ne quitte la machine",
    ),
    (
        "commande",
        "Commande personnalisée",
        "commande définie dans agent.json",
    ),
];
/// Autorisations d'odysseus-mcp : titre, explication
const PERMS: [(&str, &str); 11] = [
    (
        "Lire l'état du système",
        "Charge, températures, mémoire, disques, réseau, écran, réglages, journaux d'odysseus.",
    ),
    (
        "Lire la documentation",
        "Man pages, doc du noyau, manuels info, notes de construction de la machine, ~/Documents/Bibliothèque.",
    ),
    (
        "Lire les fichiers",
        "Vos fichiers et ceux du système, jamais les secrets (clés, mots de passe). Avec un agent en ligne, leur contenu quitte la machine.",
    ),
    (
        "Notes",
        "Lire, écrire et ranger vos notes ; une note supprimée va à la corbeille. Avec un agent en ligne, leur contenu quitte la machine.",
    ),
    (
        "Mémoire",
        "Les faits que vous avez accepté de retenir (préférences, machine), lus à chaque conversation, et vos conversations précédentes. Retenir, mettre à jour ou oublier passe toujours par une notification.",
    ),
    (
        "Voir le bureau",
        "Fenêtres ouvertes et leurs titres, bureaux, historique des notifications.",
    ),
    (
        "Voir l'écran",
        "Captures de l'écran ou d'une fenêtre. Un témoin s'allume dans la barre ; avec un agent en ligne, l'image quitte la machine.",
    ),
    (
        "Agir sur le bureau",
        "Notifier, ouvrir un fichier, changer de bureau ou de fenêtre, régler thème, fond, lumière de nuit, échelle.",
    ),
    (
        "Presse-papiers",
        "Lire ce que vous avez copié, y écrire du texte.",
    ),
    (
        "Exécuter des commandes",
        "Chaque commande vous est montrée dans une notification : rien ne se lance sans votre accord. Jamais en root.",
    ),
    (
        "Commandes sans accord",
        "Option de la précédente, pour un agent de confiance : ses commandes partent sans notification. Chacune reste consignée dans le journal ci-dessous.",
    ),
];
/// Dock : masquage et taille des icônes
const DOCK_HIDE: [(&str, &str); 3] = [
    ("intelligent", "Quand une fenêtre passe dessous"),
    ("toujours", "Toujours"),
    ("jamais", "Jamais"),
];
const DOCK_SIZE: [(u32, &str); 3] = [(40, "Petit"), (48, "Moyen"), (56, "Grand")];
/// Changement de fond au hasard : intervalle en secondes (0 : jamais)
const WALLPAPER_ROTATION: [(u32, &str); 5] = [
    (0, "Jamais"),
    (30, "30 s"),
    (300, "5 min"),
    (1800, "30 min"),
    (3600, "1 h"),
];
const SUSPEND: [(u32, &str); 4] = [(0, "Jamais"), (30, "30 min"), (60, "1 h"), (120, "2 h")];
/// Dispositions proposées : réglage, nom
const KEYBOARDS: [(&str, &str); 8] = [
    ("auto", "Automatique — Mac si un clavier Apple est branché"),
    ("fr", "Français (PC, AZERTY)"),
    ("fr:mac", "Français (Mac, AZERTY)"),
    ("be", "Français (Belgique)"),
    ("ch:fr", "Français (Suisse)"),
    ("us", "Anglais (États-Unis, QWERTY)"),
    ("gb", "Anglais (Royaume-Uni)"),
    ("de", "Allemand (QWERTZ)"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Apparence,
    Compte,
    Fond,
    Ecran,
    Clavier,
    Energie,
    Raccourcis,
    Dock,
    Roue,
    Reception,
    Automatismes,
    Trousseau,
    MisesAJour,
    Agent,
    APropos,
}

/// Accueil (premier lancement, `odysseus-reglages --accueil`) : bienvenue,
/// quelques pages de Réglages, puis l'essentiel en quelques gestes
const WIZARD: [Option<Page>; 7] = [
    None,
    Some(Page::Apparence),
    Some(Page::Fond),
    Some(Page::Ecran),
    Some(Page::Clavier),
    Some(Page::Agent),
    None,
];
/// Pied de page de l'accueil (navigation)
const FOOTER_H: f32 = 72.0;

const PAGES: [Page; 15] = [
    Page::Compte,
    Page::Apparence,
    Page::Fond,
    Page::Ecran,
    Page::Clavier,
    Page::Energie,
    Page::Raccourcis,
    Page::Dock,
    Page::Roue,
    Page::Reception,
    Page::Automatismes,
    Page::Trousseau,
    Page::MisesAJour,
    Page::Agent,
    Page::APropos,
];

impl Page {
    fn info(self) -> (&'static str, char) {
        match self {
            Page::Compte => ("Compte", icon::USER),
            Page::Apparence => ("Apparence", icon::PALETTE),
            Page::Fond => ("Fond d'écran", icon::IMAGE),
            Page::Ecran => ("Écran", icon::MONITOR),
            Page::Clavier => ("Clavier et langue", icon::KEYBOARD),
            Page::Energie => ("Énergie", icon::BATTERY_CHARGING),
            Page::Raccourcis => ("Raccourcis", icon::COMMAND),
            Page::Dock => ("Dock", icon::APP),
            Page::Roue => ("Roue", icon::CIRCLE_DOT),
            Page::Reception => ("Réception", icon::INBOX),
            Page::Automatismes => ("Automatismes", icon::ZAP),
            Page::Trousseau => ("Trousseau", icon::LOCK),
            Page::MisesAJour => ("Mises à jour", icon::ARROW_DOWN_CIRCLE),
            Page::Agent => ("Agent", icon::BOT),
            Page::APropos => ("À propos", icon::INFO),
        }
    }

    fn from_arg(arg: &str) -> Option<Self> {
        Some(match arg {
            "compte" | "utilisateur" | "photo" => Page::Compte,
            "apparence" => Page::Apparence,
            "fond" => Page::Fond,
            "ecran" | "écran" => Page::Ecran,
            "clavier" | "langue" => Page::Clavier,
            "energie" | "énergie" | "veille" => Page::Energie,
            "raccourcis" => Page::Raccourcis,
            "dock" => Page::Dock,
            "roue" => Page::Roue,
            "reception" | "réception" | "localsend" => Page::Reception,
            "automatismes" => Page::Automatismes,
            "trousseau" | "secrets" => Page::Trousseau,
            "mises-a-jour" | "maj" => Page::MisesAJour,
            "agent" | "assistant" => Page::Agent,
            "apropos" | "a-propos" => Page::APropos,
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    Page(Page),
    AutoValidate(usize),
    AutoPause(usize),
    AutoRun(usize),
    AutoEdit(usize),
    AutoDelete(usize),
    AutoFolder,
    KeyringLock(usize),
    KeyringUnlock(usize),
    SecretDelete(usize, usize),
    UpdatesCheck,
    UpdateOdysseus,
    UpdatePrepare(usize),
    UpdateIgnore(usize),
    Theme(usize),
    /// Thème personnalisé : base sombre (vrai) ou claire, accent, fond
    CustomBase(bool),
    CustomAccent(u32),
    CustomBg(u32),
    Fond(usize),
    FondUni,
    /// Changement de fond au hasard, toutes les N secondes (0 : jamais)
    FondRotation(u32),
    /// Croix sur une de vos images : la mettre à la corbeille
    FondDelete(usize),
    Scale(usize),
    Resolution(i32, i32),
    Refresh(i32),
    Night(&'static str),
    NightFrom(i32),
    NightTo(i32),
    NightTemp,
    Keyboard(usize),
    RepeatDelay(u32),
    RepeatRate(u32),
    Locale(usize),
    ScreenOff(u32),
    LockToggle,
    /// Flou d'arrière-plan activé ou non (Apparence)
    BlurToggle,
    /// Transparence des surfaces floutées (curseur, qu'on peut faire glisser)
    BlurLevel,
    ReceptionToggle,
    /// Réglages > Roue : choisir une place, y mettre un élément du catalogue,
    /// la vider, la déplacer, reprendre les favoris du Dock ; en faire un
    /// dossier, l'ouvrir, en revenir, le renommer
    /// Réglages > Compte : une image récente comme photo, retirer la photo,
    /// modifier le nom, changer le mot de passe
    ComptePhoto(usize),
    CompteRemovePhoto,
    CompteName,
    ComptePassword,
    RoueSlot(usize),
    RoueAdd(usize),
    RoueRemove,
    RoueMove(i8),
    RoueReset,
    RoueFolder,
    RoueOpen,
    RoueBack,
    RoueRename,
    Suspend(u32),
    DockToggle,
    DockHide(&'static str),
    DockSize(u32),
    SleepNow,
    Keep,
    Revert,
    CopyInfo,
    /// Accueil : étape précédente, suivante (ou terminer), le relancer
    WizardPrev,
    WizardNext,
    WizardStart,
    AgentKind(usize),
    InstallOllama,
    PullModel(String),
    DeleteModel(String),
    UseModel(String),
    InstallClaude,
    UpdateClaude,
    ClaudeLogin,
    EditAgentJson,
    Perm(usize),
    CopyMcp,
    /// Mémoire de l'Assistant : oublier un fait (deux clics), le garder à la
    /// relecture, ouvrir memoire.md
    Forget(String),
    KeepFact(String),
    EditMemory,
}

type Rect = (f32, f32, f32, f32);

/// Messages des fils (vignettes, fonds calculés, dépôts, langue)
enum Msg {
    Thumb(PathBuf, Option<Image>),
    Generated(usize),
    Dropped(Vec<PathBuf>),
    Locale(Result<String, String>),
    /// Fin de la mise à jour d'odysseus (récupération, compilation)
    Upgraded(Result<String, String>),
    /// Réglages > Compte : vignette d'une image récente, photo posée
    Photo(PathBuf, Option<Image>),
    Avatar(Result<(), String>),
}

/// Nouvelle définition en attente de confirmation
struct Confirm {
    previous: (i32, i32, i32),
    chosen: (i32, i32, i32),
    since: Instant,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: Option<SlotPool>,
    window: Window,
    loop_handle: LoopHandle<'static, App>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    modifiers: Modifiers,
    data_manager: Option<DataDeviceManagerState>,
    data_device: Option<DataDevice>,
    regular: Text,
    bold: Text,
    icons: Text,
    msgs: channel::Sender<Msg>,
    page: Page,
    /// Accueil en cours : étape (index dans WIZARD)
    wizard: Option<usize>,
    settings: Settings,
    /// Thème personnalisé (Apparence), tel qu'enregistré
    custom: theme::Custom,
    /// Page Roue, chargée à son ouverture
    roue: Option<roue::RouePage>,
    /// Agent du bureau : ~/.config/odysseus/agent.json (modifiable à la main)
    agent: common::agent::AgentConfig,
    screen: Option<Screen>,
    night: String,
    locales: Vec<String>,
    locale: Option<String>,
    fonds: Vec<fonds::Fond>,
    about: Vec<(&'static str, String)>,
    user: (String, String),
    /// Photo de l'utilisateur (~/.face), dans l'en-tête de la barre latérale
    avatar: Option<Image>,
    /// Page Compte, chargée à son ouverture
    compte: Option<compte::ComptePage>,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    scroll: f32,
    content_h: f32,
    /// Barre de défilement : survolée, et saisie (décalage du pointeur dans le curseur)
    scrollbar_hover: bool,
    scrollbar_drag: Option<f32>,
    /// Curseur saisi (sa zone), suivi tant que le bouton est tenu
    slider_drag: Option<Rect>,
    /// Défilement progressif (molette, clavier) : position visée
    scroll_target: Option<f32>,
    /// Redessins regroupés : au plus un par rafraîchissement de l'écran
    qh: QueueHandle<App>,
    frame_pending: bool,
    dirty: bool,
    last_frame: Instant,
    confirm: Option<Confirm>,
    toast: Option<(String, Instant)>,
    typed: String,
    /// Agents : état (programmes, service, modèles), tâches longues en cours,
    /// modèle dont la suppression est armée (second clic)
    agent_status: agents::Status,
    tasks: Vec<(agents::Task, f32, String)>,
    news: channel::Sender<agents::News>,
    delete_armed: Option<(String, Instant)>,
    /// Fait dont l'oubli attend un second clic
    forget_armed: Option<(String, Instant)>,
    /// Automatismes (~/.config/odysseus/automatismes) et leur état, fichiers illisibles
    autos: Vec<(common::auto::Automatism, common::auto::Status)>,
    /// Trousseaux (Secret Service) et noms des secrets rangés
    keyrings: trousseau::State,
    keyrings_read: Option<Instant>,
    /// Mises à jour trouvées par odysseus-veille
    updates: maj::State,
    autos_bad: Vec<(String, String)>,
    drop_hover: bool,
    /// Boutons de la fenêtre, et dernier clic dans l'en-tête (double-clic : agrandir)
    chrome: common::chrome::Chrome,
    last_header_click: Option<Instant>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

/// Accueil terminé : noté dans ~/.config/odysseus/accueil (la session ne le relance plus)
fn finish_welcome() {
    if let Some(path) = theme::config_path().map(|p| p.with_file_name("accueil")) {
        let _ = std::fs::write(
            path,
            format!(
                "Accueil terminé le {}\n",
                chrono::Local::now().format("%d/%m/%Y")
            ),
        );
    }
}

/// Une seule fenêtre de Réglages par bureau (affichage Wayland)
fn pid_file() -> Option<PathBuf> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let display = display.rsplit('/').next().unwrap_or("").to_string();
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join(format!("odysseus-reglages.{display}.pid")))
}

/// Déjà ouvert : rien à faire (la fenêtre existe), on renvoie vrai
fn already_running() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    pid != std::process::id()
        && stat.contains("(odysseus-regla")
        && !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z')
}

fn main() {
    // Posée sur le flou du compositeur : palette translucide si le flou est actif
    common::theme::use_vibrancy();
    if already_running() {
        return;
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    let page = std::env::args()
        .nth(1)
        .and_then(|a| Page::from_arg(&a))
        .unwrap_or(Page::Apparence);

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<App> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestClient, &qh);
    window.set_app_id("odysseus-reglages");
    window.set_title("Réglages");
    window.set_min_size(Some((860, 560)));
    window.commit();

    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let (news, news_rx) = channel::channel::<agents::News>();
    event_loop
        .handle()
        .insert_source(news_rx, |event, _, app| {
            if let ChannelEvent::Msg(news) = event {
                app.on_news(news);
            }
        })
        .expect("canal des installations");
    let (msgs, rx) = channel::channel::<Msg>();
    event_loop
        .handle()
        .insert_source(rx, |event, _, app| {
            if let ChannelEvent::Msg(msg) = event {
                app.on_msg(msg);
            }
        })
        .expect("canal des fils");

    // Thème changé ailleurs (ou ici) : redessin
    if let Some(fd) = theme::watch() {
        use smithay_client_toolkit::reexports::calloop::{
            Interest, Mode, PostAction, generic::Generic,
        };
        use std::os::fd::AsFd;
        let _ = event_loop.handle().insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, fd, app| {
                theme::drain(fd.as_fd());
                theme::reload();
                // agent.json ou reglages modifiés à la main : la page suit
                app.agent = common::agent::AgentConfig::load();
                app.settings = Settings::load();
                app.draw();
                Ok(PostAction::Continue)
            },
        );
    }
    // Compte à rebours de confirmation et messages passagers
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

    let (fonds, missing) = fonds::list();
    let mut app = App {
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
        data_manager: DataDeviceManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        msgs,
        page,
        wizard: std::env::args().any(|a| a == "--accueil").then_some(0),
        settings: Settings::load(),
        custom: theme::Custom::load(),
        roue: None,
        agent: common::agent::AgentConfig::load(),
        screen: system::screen(),
        night: system::night_mode(),
        locales: system::locales(),
        locale: system::current_locale(),
        fonds,
        about: Vec::new(),
        user: (common::account::display_name(), common::account::login()),
        avatar: common::account::load_avatar(),
        compte: None,
        hits: Vec::new(),
        hover: None,
        scroll: 0.0,
        content_h: 0.0,
        scrollbar_hover: false,
        scrollbar_drag: None,
        slider_drag: None,
        scroll_target: None,
        qh: qh.clone(),
        frame_pending: false,
        dirty: false,
        last_frame: Instant::now(),
        confirm: None,
        toast: None,
        typed: String::new(),
        agent_status: agents::Status::default(),
        tasks: Vec::new(),
        news,
        delete_armed: None,
        forget_armed: None,
        autos: Vec::new(),
        keyrings: trousseau::State::Unknown,
        keyrings_read: None,
        updates: maj::State::default(),
        autos_bad: Vec::new(),
        drop_hover: false,
        chrome: common::chrome::Chrome::default(),
        last_header_click: None,
        width: 1060,
        height: 720,
        scale: 1,
        configured: false,
        exit: false,
    };
    app.about = about(&app.screen);
    if app.page == Page::Agent {
        app.agent_status = agents::status();
    }
    if app.page == Page::Roue {
        app.roue = Some(roue::RouePage::load());
    }
    if app.page == Page::Compte {
        app.compte = Some(compte::ComptePage::load(app.msgs.clone()));
    }
    app.load_thumbs();
    // Fonds d'odysseus pas encore calculés : un fil, l'un après l'autre
    if !missing.is_empty() {
        let tx = app.msgs.clone();
        std::thread::spawn(move || {
            for i in missing {
                if fonds::generate(i).is_some() {
                    let _ = tx.send(Msg::Generated(i));
                }
            }
        });
    }

    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

/// Fiche « À propos » : ce que la machine dit d'elle-même
fn about(screen: &Option<Screen>) -> Vec<(&'static str, String)> {
    use common::sysinfo;
    let gb = |b: u64| format!("{:.1} Go", b as f64 / (1u64 << 30) as f64).replace('.', ",");
    let mut rows = vec![
        ("Machine", sysinfo::hostname()),
        ("Système", sysinfo::os_name()),
        ("Noyau", format!("Linux {}", sysinfo::kernel())),
        (
            "Processeur",
            format!(
                "{} · {} cœurs logiques",
                sysinfo::cpu_model(),
                sysinfo::cpu_threads()
            ),
        ),
    ];
    if let Some(dir) = sysinfo::gpu_device() {
        let vram = std::fs::read_to_string(dir.join("mem_info_vram_total"))
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| format!(" · {}", gb(v)))
            .unwrap_or_default();
        rows.push((
            "Carte graphique",
            format!("{}{vram}", sysinfo::gpu_name(&dir)),
        ));
    }
    rows.push(("Mémoire", gb(sysinfo::memory_total())));
    if let Some((total, free)) = sysinfo::statvfs("/") {
        rows.push(("Stockage", format!("{} libres sur {}", gb(free), gb(total))));
    }
    if let Some(s) = screen {
        rows.push((
            "Écran",
            format!(
                "{} · {} × {} à {} Hz · échelle {} %",
                s.name,
                s.current.0,
                s.current.1,
                s.current.2,
                (s.scale * 100.0).round()
            ),
        ));
    }
    rows.push((
        "Bureau",
        format!(
            "odysseus-desktop {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("ODYSSEUS_COMMIT").unwrap_or("version de développement")
        ),
    ));
    rows
}

/// Commande qui déclare odysseus-mcp dans Claude Code
fn mcp_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("odysseus-mcp")))
        .unwrap_or_else(|| PathBuf::from("odysseus-mcp"));
    format!("claude mcp add odysseus -s user -- {}", exe.display())
}

/// odysseus-mcp déjà déclaré dans la configuration de Claude Code de l'utilisateur
fn mcp_declared() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    std::fs::read_to_string(PathBuf::from(home).join(".claude.json"))
        .is_ok_and(|t| t.contains("odysseus-mcp"))
}

/// « 22 oct. 2026 »
fn fact_date(t: i64) -> String {
    const MOIS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    use chrono::Datelike;
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| {
            let d = d.with_timezone(&chrono::Local);
            format!("{} {} {}", d.day(), MOIS[d.month0() as usize], d.year())
        })
        .unwrap_or_default()
}

/// Derniers appels des agents (journal d'odysseus-mcp), du plus récent au plus ancien :
/// heure, accordé, outil, paramètres
fn agent_activity() -> Vec<(String, bool, String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let text = std::fs::read_to_string(PathBuf::from(home).join(".local/state/odysseus/agent.log"))
        .unwrap_or_default();
    text.lines()
        .rev()
        .take(8)
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            let time = f.next()?;
            let granted = f.next()? == "accordé";
            let tool = f.next()?.to_string();
            let detail = f.next().unwrap_or("").to_string();
            let time = time.get(11..19).unwrap_or(time).to_string();
            Some((time, granted, tool, detail))
        })
        .collect()
}

/// Lecture d'une liste d'URI déposée (dans un fil : l'autre application écrit)
fn read_drop(
    pipe: smithay_client_toolkit::data_device_manager::ReadPipe,
    tx: channel::Sender<Msg>,
) {
    // Pas into_raw_fd() : celui de smithay-client-toolkit 0.19 ferme le descripteur au passage
    let mut file = File::from(std::os::fd::OwnedFd::from(pipe));
    std::thread::spawn(move || {
        let mut text = String::new();
        if file.read_to_string(&mut text).is_ok() {
            let _ = tx.send(Msg::Dropped(fonds::paths_from_uri_list(&text)));
        }
    });
}

impl App {
    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn save_agent(&mut self) {
        if let Err(e) = self.agent.save() {
            self.toast(format!("agent.json non enregistré : {e}"));
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.settings.save() {
            self.toast(format!("Réglages non enregistrés : {e}"));
        }
    }

    /// Relit la liste des fonds ; les vignettes déjà calculées sont gardées
    fn reload_fonds(&mut self) {
        let (list, _) = fonds::list();
        let old = std::mem::replace(&mut self.fonds, list);
        for f in self.fonds.iter_mut() {
            if let Some(o) = old.iter().find(|o| o.path == f.path) {
                f.thumb = o.thumb.clone();
            }
        }
        self.load_thumbs();
    }

    /// Retire une de vos images (à la corbeille : elle se récupère) ; si
    /// c'était le fond actuel, on revient au premier fond d'odysseus
    fn delete_fond(&mut self, i: usize) {
        let Some(f) = self.fonds.get(i) else {
            return;
        };
        let (path, name) = (f.path.clone(), f.name.clone());
        if let Err(e) = common::trash::trash(&path) {
            return self.toast(format!("Image non retirée : {e}"));
        }
        let current = self.settings.wallpaper.as_deref() == Some(&*path.to_string_lossy());
        self.reload_fonds();
        if current {
            let first = self
                .fonds
                .first()
                .map(|f| f.path.to_string_lossy().into_owned());
            self.set_wallpaper(first);
        }
        self.toast(format!("« {name} » mis à la corbeille"));
    }

    fn load_thumbs(&self) {
        let paths: Vec<PathBuf> = self
            .fonds
            .iter()
            .filter(|f| f.thumb.is_none() && f.path.exists())
            .map(|f| f.path.clone())
            .collect();
        let tx = self.msgs.clone();
        std::thread::spawn(move || {
            for p in paths {
                let thumb = fonds::thumbnail(&p);
                let _ = tx.send(Msg::Thumb(p, thumb));
            }
        });
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Upgraded(result) => self.on_upgraded(result),
            Msg::Photo(path, thumb) => {
                if let Some(c) = self.compte.as_mut() {
                    c.on_photo(path, thumb);
                }
            }
            Msg::Avatar(result) => {
                if let Some(c) = self.compte.as_mut() {
                    let msg = c.on_avatar(result);
                    self.toast(msg);
                }
                self.avatar = common::account::load_avatar();
            }
            Msg::Thumb(path, thumb) => {
                if let Some(f) = self.fonds.iter_mut().find(|f| f.path == path) {
                    f.thumb = thumb;
                }
            }
            Msg::Generated(i) => {
                if let Some(f) = self.fonds.get(i) {
                    let p = f.path.clone();
                    let tx = self.msgs.clone();
                    std::thread::spawn(move || {
                        let thumb = fonds::thumbnail(&p);
                        let _ = tx.send(Msg::Thumb(p, thumb));
                    });
                }
            }
            // Sur la page Compte, une image déposée devient la photo
            Msg::Dropped(paths) if self.page == Page::Compte => {
                match paths.iter().find(|p| common::account::is_photo(p)) {
                    Some(path) => {
                        if let Some(c) = self.compte.as_mut() {
                            c.set_avatar(path);
                        }
                    }
                    None => self.toast("Déposez une photo (JPEG, PNG ou HEIC)"),
                }
            }
            Msg::Dropped(paths) => {
                let mut last = None;
                for p in paths {
                    match fonds::import(&p) {
                        Ok(dest) => last = Some(dest),
                        Err(e) => self.toast(e),
                    }
                }
                if let Some(dest) = last {
                    self.reload_fonds();
                    self.set_wallpaper(Some(dest.to_string_lossy().into_owned()));
                    self.toast("Image ajoutée à vos fonds d'écran");
                }
            }
            Msg::Locale(result) => match result {
                Ok(lang) => {
                    self.locale = Some(lang.clone());
                    self.toast(format!(
                        "Langue du système : {} — à la prochaine connexion",
                        system::locale_name(&lang)
                    ));
                }
                Err(e) => self.toast(format!("Langue non changée : {e}")),
            },
        }
        self.draw();
    }

    fn start(&mut self, task: agents::Task) {
        if !self.tasks.iter().any(|(t, ..)| *t == task) {
            self.tasks.push((task, -1.0, "Démarrage…".into()));
        }
    }

    fn on_news(&mut self, news: agents::News) {
        match news {
            agents::News::Step(task, frac, text) => {
                match self.tasks.iter_mut().find(|(t, ..)| *t == task) {
                    Some(entry) => {
                        entry.1 = frac;
                        entry.2 = text;
                    }
                    None => self.tasks.push((task, frac, text)),
                }
            }
            agents::News::Done(task, result) => {
                self.tasks.retain(|(t, ..)| *t != task);
                match result {
                    Ok(text) => {
                        // Premier modèle téléchargé : c'est lui que l'agent utilisera
                        if let agents::Task::Pull(m) = &task
                            && !self
                                .agent_status
                                .models
                                .iter()
                                .any(|(n, _)| *n == self.agent.ollama_model)
                        {
                            self.agent.ollama_model = m.clone();
                            self.save_agent();
                        }
                        self.toast(text);
                    }
                    Err(e) => self.toast(format!("Échec : {e}")),
                }
                self.agent_status = agents::status();
            }
        }
        self.draw();
    }

    /// Ouvre un fichier dans l'éditeur ($EDITOR, vim par défaut), dans un terminal
    fn edit_in_terminal(&mut self, path: &std::path::Path, title: &str) {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
        let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
        use std::os::unix::process::CommandExt;
        match std::process::Command::new(term)
            .arg(format!("--title={title}"))
            .arg(editor)
            .arg(path)
            .process_group(0)
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
                self.toast("Enregistrez : Réglages suit le fichier aussitôt");
            }
            Err(e) => self.toast(format!("Terminal introuvable : {e}")),
        }
    }

    fn tick(&mut self) {
        // Page Agent : le journal d'activité se met à jour tout seul
        let mut redraw = self.page == Page::Agent;
        // Page Automatismes : un automatisme écrit par l'agent apparaît aussitôt
        if self.page == Page::Automatismes {
            self.refresh_autos();
            redraw = true;
        }
        // Page Trousseau : ouverture par la fenêtre de mot de passe, secrets ajoutés
        if self.page == Page::Trousseau
            && self
                .keyrings_read
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            self.refresh_keyrings();
            redraw = true;
        }
        // Page Mises à jour : vérification en cours, rapport réécrit par la veille
        if self.page == Page::MisesAJour && (self.updates.poll() | self.updates.refresh()) {
            redraw = true;
        }
        if let Some(c) = self.confirm.as_ref() {
            if c.since.elapsed() >= CONFIRM {
                self.revert_mode();
            }
            redraw = true;
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST)
        {
            self.toast = None;
            redraw = true;
        }
        if redraw {
            self.draw();
        }
    }

    fn refresh_screen(&mut self) {
        // Le compositeur change de mode à l'image suivante : on relit un peu après
        let _ = self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_millis(400)),
            |_, _, app| {
                app.screen = system::screen();
                app.about = about(&app.screen);
                app.draw();
                TimeoutAction::Drop
            },
        );
    }

    /// Enregistre le thème personnalisé et le met en vigueur
    fn set_custom(&mut self, c: theme::Custom) {
        self.custom = c;
        if let Err(e) = theme::set_custom(&c) {
            self.toast(format!("Thème : {e}"));
        }
    }

    fn set_wallpaper(&mut self, value: Option<String>) {
        self.settings.wallpaper = value;
        self.save();
    }

    fn set_mode(&mut self, mode: (i32, i32, i32)) {
        let Some(screen) = self.screen.as_ref() else {
            return;
        };
        if mode == screen.current {
            return;
        }
        let previous = self
            .confirm
            .as_ref()
            .map(|c| c.previous)
            .unwrap_or(screen.current);
        match system::command(&format!("output mode {}x{}@{}", mode.0, mode.1, mode.2)) {
            Ok(()) => {
                self.confirm = Some(Confirm {
                    previous,
                    chosen: mode,
                    since: Instant::now(),
                });
                self.refresh_screen();
            }
            Err(e) => self.toast(e),
        }
    }

    fn keep_mode(&mut self) {
        if let Some(c) = self.confirm.take() {
            self.settings.mode = Some(format!("{}x{}@{}", c.chosen.0, c.chosen.1, c.chosen.2));
            self.save();
        }
    }

    fn revert_mode(&mut self) {
        if let Some(c) = self.confirm.take() {
            let (w, h, r) = c.previous;
            if let Err(e) = system::command(&format!("output mode {w}x{h}@{r}")) {
                self.toast(e);
            }
            self.refresh_screen();
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<(Rect, Hit)> {
        self.hits
            .iter()
            .rev()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .cloned()
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

    /// Ouvre une page (et relit ce qu'elle affiche)
    fn open_page(&mut self, p: Page) {
        self.page = p;
        self.set_scroll(0.0);
        if p == Page::Agent {
            self.agent_status = agents::status();
        }
        if p == Page::Automatismes {
            self.refresh_autos();
        }
        if p == Page::Trousseau {
            self.refresh_keyrings();
        }
        if p == Page::Roue {
            self.roue = Some(roue::RouePage::load());
        }
        if p == Page::Compte {
            self.compte = Some(compte::ComptePage::load(self.msgs.clone()));
        }
        if p == Page::MisesAJour {
            self.updates.refresh();
        }
        if p == Page::Ecran || p == Page::APropos {
            self.screen = system::screen();
            self.about = about(&self.screen);
            self.night = system::night_mode();
        }
    }

    /// Barre de défilement, quand la page dépasse la fenêtre : abscisse du
    /// milieu, rail (haut, hauteur), curseur (haut, hauteur)
    fn scrollbar_geometry(&self, content_h: f32) -> Option<(f32, f32, f32, f32, f32)> {
        let height = self.height as f32;
        let max = content_h - height;
        if max <= 1.0 {
            return None;
        }
        let bottom = if self.wizard.is_some() { FOOTER_H } else { 0.0 };
        let track_y = HEADER_H + 6.0;
        let track_h = height - track_y - bottom - 6.0;
        if track_h < 40.0 {
            return None;
        }
        // Curseur proportionnel à la partie visible, 36 points au moins
        let thumb_h = (track_h * height / content_h).clamp(36.0, track_h);
        let thumb_y = track_y + (track_h - thumb_h) * (self.scroll / max).clamp(0.0, 1.0);
        Some((self.width as f32 - 9.0, track_y, track_h, thumb_y, thumb_h))
    }

    /// Le pointeur est-il sur la barre (bande de 16 points au bord droit) ?
    fn on_scrollbar(&self, x: f32, y: f32) -> bool {
        self.scrollbar_geometry(self.content_h)
            .is_some_and(|(bx, ty, th, _, _)| (x - bx).abs() <= 8.0 && y >= ty && y <= ty + th)
    }

    /// Défilement immédiat (changement de page, barre saisie)
    fn set_scroll(&mut self, y: f32) {
        self.scroll = y;
        self.scroll_target = None;
    }

    /// Défilement progressif : la page rejoint la position visée en quelques images
    fn scroll_by(&mut self, delta: f32) {
        let max = (self.content_h - self.height as f32).max(0.0);
        let base = self.scroll_target.unwrap_or(self.scroll);
        self.scroll_target = Some((base + delta).clamp(0.0, max));
    }

    /// Image suivante (rappel du compositeur) : défilement progressif, puis
    /// redessin s'il a été demandé entre-temps
    fn next_frame(&mut self) {
        self.frame_pending = false;
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
        if let Some(target) = self.scroll_target {
            let max = (self.content_h - self.height as f32).max(0.0);
            let target = target.clamp(0.0, max);
            self.scroll += (target - self.scroll) * (1.0 - (-dt / 0.06).exp());
            if (target - self.scroll).abs() < 0.5 {
                self.set_scroll(target);
            }
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.render();
        }
    }

    /// Place le haut du curseur en `thumb_top` (points) et défile en conséquence
    fn scroll_to_thumb(&mut self, thumb_top: f32) {
        if let Some((_, ty, th, _, thumb_h)) = self.scrollbar_geometry(self.content_h) {
            let max = (self.content_h - self.height as f32).max(0.0);
            let k = ((thumb_top - ty) / (th - thumb_h).max(1.0)).clamp(0.0, 1.0);
            self.set_scroll(k * max);
            self.draw();
        }
    }

    /// Transparence d'après la place du pointeur sur le curseur, par pas de 5 ;
    /// enregistrée (et suivie par les applis) seulement si elle change
    fn set_blur_level(&mut self, x: f32, rect: Rect) {
        let frac = ((x - rect.0 - SLIDER_PAD) / (rect.2 - 2.0 * SLIDER_PAD)).clamp(0.0, 1.0);
        let level = (frac * 20.0).round() as u32 * 5;
        if level != self.settings.transparency {
            self.settings.transparency = level;
            self.save();
        }
    }

    fn click(&mut self, x: f32, y: f32, serial: u32) {
        if let Some(b) = self.chrome.press(x, y, CHROME_Y) {
            return self.chrome_action(b);
        }
        let Some((rect, hit)) = self.hit_at(x, y) else {
            // En-tête vide : double-clic agrandit, glisser déplace la fenêtre
            if y < HEADER_H {
                let double = self
                    .last_header_click
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(400));
                self.last_header_click = Some(Instant::now());
                if double {
                    self.last_header_click = None;
                    self.chrome_action(common::chrome::Button::Maximize);
                } else if let Some(seat) = self.seat.as_ref() {
                    self.window.move_(seat, serial);
                }
            }
            return;
        };
        match hit {
            Hit::Page(p) => self.open_page(p),
            Hit::WizardStart => {
                self.wizard = Some(0);
                self.set_scroll(0.0);
            }
            Hit::WizardPrev | Hit::WizardNext => {
                let step = self.wizard.unwrap_or(0);
                let next = if hit == Hit::WizardPrev {
                    step.saturating_sub(1)
                } else {
                    step + 1
                };
                if next >= WIZARD.len() {
                    finish_welcome();
                    self.wizard = None;
                    self.exit = std::env::args().any(|a| a == "--accueil");
                } else {
                    self.wizard = Some(next);
                    self.set_scroll(0.0);
                    if let Some(page) = WIZARD[next] {
                        self.open_page(page);
                    }
                }
            }
            Hit::Theme(i) if i == theme::THEMES.len() => self.set_custom(self.custom),
            Hit::Theme(i) => {
                if let Err(e) = theme::set(theme::THEMES[i].id) {
                    self.toast(format!("Thème : {e}"));
                }
            }
            Hit::CustomBase(dark) => {
                let mut c = self.custom;
                if c.dark != dark {
                    // Changer de base change de fond : le premier de la gamme
                    c.dark = dark;
                    c.background = custom_backgrounds(dark)[0].0;
                }
                self.set_custom(c);
            }
            Hit::CustomAccent(accent) => {
                let c = theme::Custom {
                    accent,
                    ..self.custom
                };
                self.set_custom(c);
            }
            Hit::CustomBg(background) => {
                let c = theme::Custom {
                    background,
                    ..self.custom
                };
                self.set_custom(c);
            }
            Hit::Fond(i) => {
                if let Some(f) = self.fonds.get(i).filter(|f| f.path.exists()) {
                    let path = f.path.to_string_lossy().into_owned();
                    self.set_wallpaper(Some(path));
                }
            }
            Hit::FondUni => self.set_wallpaper(Some("uni".into())),
            Hit::FondRotation(every) => {
                self.settings.wallpaper_rotation = every;
                self.save();
            }
            Hit::FondDelete(i) => self.delete_fond(i),
            Hit::Scale(i) => {
                let scale = SCALES[i];
                match system::command(&format!("output scale {scale}")) {
                    Ok(()) => {
                        self.settings.scale = Some(scale);
                        self.save();
                        self.refresh_screen();
                    }
                    Err(e) => self.toast(e),
                }
            }
            Hit::Resolution(w, h) => {
                if let Some(screen) = self.screen.as_ref() {
                    // Même fréquence si possible, sinon la plus haute
                    let rates = screen.refreshes(w, h);
                    let r = rates
                        .iter()
                        .copied()
                        .find(|r| *r == screen.current.2)
                        .or(rates.first().copied())
                        .unwrap_or(60);
                    self.set_mode((w, h, r));
                }
            }
            Hit::Refresh(r) => {
                if let Some(s) = self.screen.as_ref() {
                    let (w, h, _) = s.current;
                    self.set_mode((w, h, r));
                }
            }
            Hit::Keep => self.keep_mode(),
            Hit::Revert => self.revert_mode(),
            Hit::Night(mode) => match system::command(&format!("nightlight {mode}")) {
                Ok(()) => self.night = mode.to_string(),
                Err(e) => self.toast(e),
            },
            Hit::NightFrom(d) | Hit::NightTo(d) => {
                let from = matches!(hit, Hit::NightFrom(_));
                let current = if from {
                    &self.settings.night_from
                } else {
                    &self.settings.night_to
                };
                let default = if from { 21 * 60 } else { 7 * 60 };
                let m = current
                    .as_deref()
                    .and_then(common::settings::parse_hm)
                    .unwrap_or(default) as i32;
                let m = (m + d).rem_euclid(24 * 60);
                let text = format!("{:02}:{:02}", m / 60, m % 60);
                if from {
                    self.settings.night_from = Some(text);
                } else {
                    self.settings.night_to = Some(text);
                }
                self.save();
            }
            Hit::NightTemp => {
                let frac = ((x - rect.0) / rect.2).clamp(0.0, 1.0);
                // De 2500 K (très chaud) à 5500 K (presque neutre), par pas de 100 K
                let k = (2500.0 + frac * 3000.0) / 100.0;
                self.settings.night_temp = Some(k.round() as u32 * 100);
                self.save();
            }
            Hit::Keyboard(i) => {
                self.settings.keyboard = KEYBOARDS[i].0.to_string();
                self.save();
            }
            Hit::RepeatDelay(ms) => {
                self.settings.repeat_delay = ms;
                self.save();
            }
            Hit::RepeatRate(r) => {
                self.settings.repeat_rate = r;
                self.save();
            }
            Hit::Locale(i) => {
                if let Some(lang) = self.locales.get(i).cloned() {
                    self.toast("Changement de langue…");
                    let tx = self.msgs.clone();
                    std::thread::spawn(move || {
                        let r = system::set_locale(&lang).map(|_| lang);
                        let _ = tx.send(Msg::Locale(r));
                    });
                }
            }
            Hit::ScreenOff(min) => {
                self.settings.screen_off_min = min;
                self.save();
            }
            Hit::BlurToggle => {
                self.settings.blur = !self.settings.blur;
                self.save();
            }
            Hit::BlurLevel => {
                self.slider_drag = Some(rect);
                self.set_blur_level(x, rect);
            }
            Hit::LockToggle => {
                self.settings.lock_on_idle = !self.settings.lock_on_idle;
                self.save();
            }
            Hit::ComptePhoto(_)
            | Hit::CompteRemovePhoto
            | Hit::CompteName
            | Hit::ComptePassword => {
                if let Some(msg) = self.compte.as_mut().and_then(|c| c.click(&hit)) {
                    self.toast(msg);
                }
                self.user.0 = common::account::display_name();
                self.avatar = common::account::load_avatar();
            }
            Hit::RoueSlot(_)
            | Hit::RoueAdd(_)
            | Hit::RoueRemove
            | Hit::RoueMove(_)
            | Hit::RoueReset
            | Hit::RoueFolder
            | Hit::RoueOpen
            | Hit::RoueBack
            | Hit::RoueRename => {
                if let Some(msg) = self.roue.as_mut().and_then(|r| r.click(&hit)) {
                    self.toast(msg);
                }
            }
            Hit::ReceptionToggle => {
                self.settings.reception = !self.settings.reception;
                self.save();
            }
            Hit::Suspend(min) => {
                self.settings.suspend_min = min;
                self.save();
            }
            Hit::DockToggle => {
                self.settings.dock = !self.settings.dock;
                self.save();
                // Réactivé : il s'était arrêté, on le relance
                if self.settings.dock {
                    system::spawn_sibling("odysseus-dock", &[]);
                }
            }
            Hit::DockHide(mode) => {
                self.settings.dock_hide = mode.into();
                self.save();
            }
            Hit::DockSize(size) => {
                self.settings.dock_size = size;
                self.save();
            }
            Hit::SleepNow => system::suspend_now(),
            Hit::AgentKind(i) => {
                self.agent.agent = AGENTS[i].0.to_string();
                self.save_agent();
            }
            Hit::InstallOllama => {
                self.start(agents::Task::InstallOllama);
                agents::install_ollama(self.news.clone());
            }
            Hit::PullModel(m) => {
                self.start(agents::Task::Pull(m.clone()));
                agents::pull(m, self.news.clone());
            }
            Hit::DeleteModel(m) => {
                let armed = self
                    .delete_armed
                    .as_ref()
                    .is_some_and(|(a, at)| *a == m && at.elapsed() < Duration::from_secs(4));
                if armed {
                    self.delete_armed = None;
                    match agents::delete(&m) {
                        Ok(()) => self.toast(format!("{m} supprimé")),
                        Err(e) => self.toast(e),
                    }
                    self.agent_status = agents::status();
                } else {
                    self.delete_armed = Some((m, Instant::now()));
                }
            }
            Hit::Forget(f) => {
                let armed = self
                    .forget_armed
                    .as_ref()
                    .is_some_and(|(a, at)| *a == f && at.elapsed() < Duration::from_secs(4));
                if armed {
                    self.forget_armed = None;
                    let done = common::memory::Memory::open()
                        .ok_or_else(|| "HOME inconnu".to_string())
                        .and_then(|m| m.remove(&f));
                    match done {
                        Ok(true) => self.toast("Oublié"),
                        Ok(false) => self.toast("Ce fait n'y était plus"),
                        Err(e) => self.toast(e),
                    }
                } else {
                    self.forget_armed = Some((f, Instant::now()));
                }
            }
            Hit::KeepFact(f) => {
                let done = common::memory::Memory::open()
                    .ok_or_else(|| "HOME inconnu".to_string())
                    .and_then(|m| m.keep(&f));
                match done {
                    Ok(true) => self.toast("Gardé : on en reparle dans six mois"),
                    Ok(false) => self.toast("Ce fait n'y était plus"),
                    Err(e) => self.toast(e),
                }
            }
            Hit::EditMemory => {
                // Fichier créé avec son en-tête s'il n'existe pas, puis ouvert dans l'éditeur
                if let Some(m) = common::memory::Memory::open() {
                    if let Err(e) = m.ensure() {
                        self.toast(e);
                        return;
                    }
                    let path = m.file().to_path_buf();
                    self.edit_in_terminal(&path, "memoire.md");
                }
            }
            Hit::UseModel(m) => {
                self.agent.ollama_model = m;
                self.agent.agent = "ollama".into();
                self.save_agent();
            }
            Hit::InstallClaude | Hit::UpdateClaude => {
                let update = hit == Hit::UpdateClaude;
                self.start(if update {
                    agents::Task::UpdateClaude
                } else {
                    agents::Task::InstallClaude
                });
                agents::install_claude(update, self.news.clone());
            }
            Hit::AutoValidate(_)
            | Hit::AutoPause(_)
            | Hit::AutoRun(_)
            | Hit::AutoEdit(_)
            | Hit::AutoDelete(_)
            | Hit::AutoFolder => self.auto_click(hit),
            Hit::KeyringLock(_) | Hit::KeyringUnlock(_) | Hit::SecretDelete(_, _) => {
                self.keyring_click(hit)
            }
            Hit::UpdatesCheck
            | Hit::UpdateOdysseus
            | Hit::UpdatePrepare(_)
            | Hit::UpdateIgnore(_) => self.updates_click(hit),
            Hit::EditAgentJson => {
                // Fichier créé s'il n'existe pas encore, puis ouvert dans l'éditeur
                if common::agent::path().is_none_or(|p| !p.exists()) {
                    self.save_agent();
                }
                if let Some(path) = common::agent::path() {
                    self.edit_in_terminal(&path, "agent.json");
                }
            }
            Hit::ClaudeLogin => match agents::claude_login() {
                Ok(()) => self.toast("Dans le terminal : /login, puis suivez les indications"),
                Err(e) => self.toast(e),
            },
            Hit::Perm(i) => {
                let p = &mut self.agent.perms;
                let flag = match i {
                    0 => &mut p.read,
                    1 => &mut p.docs,
                    2 => &mut p.files,
                    3 => &mut p.notes,
                    4 => &mut p.memory,
                    5 => &mut p.desktop,
                    6 => &mut p.screen,
                    7 => &mut p.act,
                    8 => &mut p.clipboard,
                    9 => &mut p.run,
                    _ => &mut p.run_auto,
                };
                *flag = !*flag;
                self.save_agent();
            }
            Hit::CopyMcp => match system::copy_text(&mcp_command()) {
                Ok(()) => self.toast("Commande copiée — collez-la dans un terminal"),
                Err(e) => self.toast(e),
            },
            Hit::CopyInfo => {
                let text: String = self
                    .about
                    .iter()
                    .map(|(k, v)| format!("{k} : {v}\n"))
                    .collect();
                match system::copy_text(&text) {
                    Ok(()) => self.toast("Informations copiées"),
                    Err(e) => self.toast(e),
                }
            }
        }
        self.draw();
    }

    fn key(&mut self, event: KeyEvent) {
        // Nom affiché en cours de saisie (Réglages > Compte)
        if self.page == Page::Compte
            && let Some(c) = self.compte.as_mut().filter(|c| c.editing())
        {
            if let Some(msg) = c.key(&event) {
                self.toast(msg);
            }
            self.user.0 = common::account::display_name();
            return self.draw();
        }
        // Nom d'un dossier de la roue en cours de saisie
        if self.page == Page::Roue
            && let Some(r) = self.roue.as_mut().filter(|r| r.renaming())
        {
            if let Some(msg) = r.key(&event) {
                self.toast(msg);
            }
            return self.draw();
        }
        match event.keysym {
            Keysym::q | Keysym::w if self.modifiers.ctrl => self.exit = true,
            Keysym::Escape if self.confirm.is_some() => self.revert_mode(),
            Keysym::Escape => self.exit = true,
            // Défilement au clavier (hors zone d'essai du clavier)
            Keysym::Down
            | Keysym::Up
            | Keysym::Next
            | Keysym::Prior
            | Keysym::Home
            | Keysym::End
                if self.page != Page::Clavier || self.typed.is_empty() =>
            {
                let page = self.height as f32 - HEADER_H - 60.0;
                let delta = match event.keysym {
                    Keysym::Down => 60.0,
                    Keysym::Up => -60.0,
                    Keysym::Next => page,
                    Keysym::Prior => -page,
                    Keysym::Home => -1.0e6,
                    _ => 1.0e6,
                };
                self.scroll_by(delta);
            }
            _ if self.page == Page::Clavier => {
                // Zone d'essai du clavier
                if event.keysym == Keysym::BackSpace {
                    self.typed.pop();
                } else if let Some(t) = event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                {
                    if self.typed.chars().count() < 48 {
                        self.typed.push_str(&t);
                    }
                } else {
                    return;
                }
            }
            _ => return,
        }
        self.draw();
    }

    // --- Dessin ---------------------------------------------------------------

    /// Redessin demandé : tout de suite, ou à la prochaine image si une est déjà
    /// en route (des centaines de mouvements de souris par seconde ne font pas
    /// des centaines de dessins : la fenêtre ne prend plus de retard)
    fn draw(&mut self) {
        // Pas de rappel depuis 250 ms (fenêtre réduite, autre bureau) : on dessine
        if self.frame_pending && self.last_frame.elapsed() < Duration::from_millis(250) {
            self.dirty = true;
        } else {
            self.render();
        }
    }

    fn render(&mut self) {
        if !self.configured {
            return;
        }
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
        // Sous la barre latérale, rien d'opaque : elle est translucide sur le flou
        // du compositeur (theme::use_vibrancy) ; le contenu, lui, reste opaque
        let side_px = if self.wizard.is_some() {
            0.0
        } else {
            SIDEBAR_W * s
        };
        c.clear();
        c.fill_rect(side_px, 0.0, w as f32 - side_px, h as f32, p.content);
        let mut ui = Ui {
            c: &mut c,
            s,
            regular: &self.regular,
            bold: &self.bold,
            icons: &self.icons,
            hits: Vec::new(),
            hover: self.hover.clone(),
        };

        // Contenu de la page, qui défile sous l'en-tête ; l'accueil n'a pas de barre latérale
        let side = if self.wizard.is_some() {
            0.0
        } else {
            SIDEBAR_W
        };
        let avail = self.width as f32 - side;
        let cw = CONTENT_MAX.min(avail - 64.0);
        let cx = side + ((avail - cw) / 2.0).max(32.0);
        let top = HEADER_H + 20.0 - self.scroll;
        let step = self.wizard;
        let bottom = match (step, self.page) {
            (Some(0), _) => self.page_bienvenue(&mut ui, cx, top, cw),
            (Some(i), _) if i + 1 == WIZARD.len() => self.page_fin(&mut ui, cx, top, cw),
            (_, page) => self.page_content(page, &mut ui, cx, top, cw),
        };
        let content_h =
            bottom + self.scroll + 32.0 + if self.wizard.is_some() { FOOTER_H } else { 0.0 };

        // En-tête : titre de la page (de l'étape), par-dessus le contenu qui défile
        let title = match step {
            Some(0) => "Bienvenue",
            Some(i) if i + 1 == WIZARD.len() => "C'est prêt",
            _ => self.page.info().0,
        };
        ui.rect(side, 0.0, avail, HEADER_H, p.content);
        ui.bold_text(title, cx, 12.0, 44.0, 22.0, p.text);
        ui.rect(side, HEADER_H - 1.0, avail, 1.0, p.ink(0.06));
        // Les zones cliquables cachées sous l'en-tête ne comptent pas
        ui.hits.retain(|((_, y, _, h), _)| y + h > HEADER_H);

        match step {
            Some(i) => {
                self.chrome.draw(ui.c, CHROME_Y, s);
                self.wizard_footer(&mut ui, i, cx, cw);
            }
            None => self.sidebar(&mut ui),
        }
        // Barre de défilement, sous les messages passagers
        if let Some((bx, track_y, track_h, thumb_y, thumb_h)) = self.scrollbar_geometry(content_h) {
            let active = self.scrollbar_hover || self.scrollbar_drag.is_some();
            let bw = if active { 8.0 } else { 6.0 };
            if active {
                ui.rounded(bx - bw / 2.0, track_y, bw, track_h, bw / 2.0, p.ink(0.05));
            }
            let ink = if self.scrollbar_drag.is_some() {
                0.45
            } else if active {
                0.35
            } else {
                0.2
            };
            ui.rounded(bx - bw / 2.0, thumb_y, bw, thumb_h, bw / 2.0, p.ink(ink));
        }
        self.overlays(&mut ui, cx, cw);

        self.hits = ui.hits;
        self.content_h = content_h;
        let surface = self.window.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            // La prochaine image au prochain rafraîchissement de l'écran
            surface.frame(&self.qh, surface.clone());
            self.frame_pending = true;
            self.last_frame = Instant::now();
            self.window.commit();
        }
        self.pool = Some(pool);
    }

    /// Contenu d'une page de Réglages
    fn page_content(&self, page: Page, ui: &mut Ui, cx: f32, top: f32, cw: f32) -> f32 {
        match page {
            Page::Apparence => self.page_apparence(ui, cx, top, cw),
            Page::Fond => self.page_fond(ui, cx, top, cw),
            Page::Ecran => self.page_ecran(ui, cx, top, cw),
            Page::Clavier => self.page_clavier(ui, cx, top, cw),
            Page::Energie => self.page_energie(ui, cx, top, cw),
            Page::Raccourcis => self.page_raccourcis(ui, cx, top, cw),
            Page::Dock => self.page_dock(ui, cx, top, cw),
            Page::Compte => match self.compte.as_ref() {
                Some(c) => c.draw(ui, cx, top, cw),
                None => top,
            },
            Page::Roue => match self.roue.as_ref() {
                Some(r) => r.draw(ui, cx, top, cw),
                None => top,
            },
            Page::Reception => self.page_reception(ui, cx, top, cw),
            Page::Automatismes => self.page_automatismes(ui, cx, top, cw),
            Page::Trousseau => self.page_trousseau(ui, cx, top, cw),
            Page::MisesAJour => self.page_maj(ui, cx, top, cw),
            Page::Agent => self.page_agent(ui, cx, top, cw),
            Page::APropos => self.page_apropos(ui, cx, top, cw),
        }
    }

    fn sidebar(&self, ui: &mut Ui) {
        let p = theme::palette();
        ui.rect(0.0, 0.0, SIDEBAR_W, self.height as f32, p.sidebar);
        ui.rect(SIDEBAR_W - 1.0, 0.0, 1.0, self.height as f32, p.ink(0.06));
        // Carte de l'utilisateur
        let (full, login) = &self.user;
        let s = ui.s;
        self.chrome.draw(ui.c, CHROME_Y, s);
        let t = SIDE_TOP;
        // Photo (~/.face), sinon les initiales ; un clic mène à Réglages > Compte
        match self.avatar.as_ref() {
            Some(img) => ui.image(img, 20.0, 24.0 + t, 40.0, 40.0, 20.0),
            None => {
                ui.circle(40.0, 44.0 + t, 20.0, p.accent);
                let initials = common::account::initials(full);
                ui.centered(&initials, 40.0, 34.0 + t, 20.0, 14.0, true, p.on_accent);
            }
        }
        ui.hit(
            (14.0, 18.0 + t, SIDEBAR_W - 28.0, 52.0),
            Hit::Page(Page::Compte),
        );
        ui.bold_text(full, 70.0, 24.0 + t, 22.0, 14.0, p.text);
        ui.text(
            &format!("{login} · session odysseus"),
            70.0,
            44.0 + t,
            18.0,
            11.5,
            p.text_dim,
        );
        let mut y = 88.0 + t;
        // Lignes resserrées quand la fenêtre est trop basse pour toutes les pages
        let step = ((self.height as f32 - y - 8.0) / PAGES.len() as f32).clamp(30.0, 40.0);
        let row = step - 2.0;
        for page in PAGES {
            let (label, glyph) = page.info();
            let selected = page == self.page;
            let hovered = ui.hover == Some(Hit::Page(page));
            if selected {
                ui.rounded(
                    10.0,
                    y,
                    SIDEBAR_W - 20.0,
                    row,
                    8.0,
                    p.accent.with_alpha(0.2),
                );
            } else if hovered {
                ui.rounded(10.0, y, SIDEBAR_W - 20.0, row, 8.0, p.ink(0.05));
            }
            // Icônes monochromes, comme dans la barre : l'accent pour la page ouverte
            let icon_color = if selected { p.accent } else { p.text_soft() };
            ui.icon(glyph, 32.0, y + row / 2.0, 16.0, icon_color);
            let font_color = if selected { p.text } else { p.text_soft() };
            if selected {
                ui.bold_text(label, 54.0, y, row, 13.5, font_color);
            } else {
                ui.text(label, 54.0, y, row, 13.5, font_color);
            }
            ui.hit((10.0, y, SIDEBAR_W - 20.0, row), Hit::Page(page));
            y += step;
        }
    }

    /// Bandeau de confirmation (définition) et message passager, en bas
    fn overlays(&self, ui: &mut Ui, cx: f32, cw: f32) {
        let p = theme::palette();
        let wh = self.height as f32;
        if let Some(c) = self.confirm.as_ref() {
            let left = CONFIRM.saturating_sub(c.since.elapsed()).as_secs() + 1;
            let (bh, by) = (64.0, wh - 64.0 - 20.0);
            ui.shadow(cx, by, cw, bh);
            ui.rounded(cx, by, cw, bh, 14.0, p.panel);
            ui.stroke(cx, by, cw, bh, 14.0, p.panel_border());
            ui.bold_text(
                "Garder cette définition ?",
                cx + 20.0,
                by + 12.0,
                20.0,
                14.0,
                p.text,
            );
            ui.text(
                &format!("Retour à l'ancienne dans {left} s"),
                cx + 20.0,
                by + 32.0,
                18.0,
                12.0,
                p.text_dim,
            );
            let bx = cx + cw - 20.0 - 100.0;
            ui.button(bx, by + 16.0, 100.0, "Garder", true, Hit::Keep);
            ui.button(bx - 110.0, by + 16.0, 100.0, "Revenir", false, Hit::Revert);
        } else if let Some((text, _)) = self.toast.as_ref() {
            let tw = ui.measure(text, 13.0) + 40.0;
            let (bx, by) = (cx + (cw - tw) / 2.0, wh - 64.0);
            ui.shadow(bx, by, tw, 40.0);
            ui.rounded(bx, by, tw, 40.0, 20.0, p.panel);
            ui.stroke(bx, by, tw, 40.0, 20.0, p.panel_border());
            ui.centered(text, bx + tw / 2.0, by, 40.0, 13.0, false, p.text);
        }
    }

    fn page_apparence(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Thème");
        let gap = 14.0;
        let n = theme::THEMES.len() + 1;
        let cw = (w - gap * (n - 1) as f32) / n as f32;
        let ch = cw * 0.62;
        // Les thèmes d'odysseus, puis le vôtre
        let custom = self.custom.palette();
        let themes = theme::THEMES.iter().chain(std::iter::once(&custom));
        for (i, t) in themes.enumerate() {
            let tx = x + i as f32 * (cw + gap);
            let selected = t.id == p.id;
            mockup(
                ui,
                tx,
                y,
                cw,
                ch,
                t,
                selected,
                ui.hover == Some(Hit::Theme(i)),
            );
            let label_color = if selected { p.accent } else { p.text };
            ui.centered(
                t.name,
                tx + cw / 2.0,
                y + ch + 8.0,
                22.0,
                13.0,
                selected,
                label_color,
            );
            ui.hit((tx, y, cw, ch + 32.0), Hit::Theme(i));
        }
        y += ch + 44.0;
        y = ui.note(
            x,
            y,
            w,
            "Le thème s'applique aussitôt à tout le bureau : barre, panneaux, Fichiers, Aperçu et liserés des fenêtres. Le fond d'écran se choisit à part.",
        );

        // Le vôtre : base, accent, fond ; toucher un choix le met en vigueur
        y = ui.section(x, y + 12.0, "Thème personnalisé");
        let c = self.custom;
        let row = 56.0;
        ui.card(x, y, w, 3.0 * row);
        let label_w = 110.0;
        ui.text("Base", x + 18.0, y, row, 13.0, p.text);
        ui.segmented(
            x + label_w,
            y + (row - 34.0) / 2.0,
            220.0,
            &["Sombre", "Claire"],
            Some(if c.dark { 0 } else { 1 }),
            |i| Hit::CustomBase(i == 0),
        );
        let swatches = |ui: &mut Ui,
                        y: f32,
                        label: &str,
                        colors: &[(u32, &str)],
                        current: u32,
                        hit: &dyn Fn(u32) -> Hit| {
            ui.rect(x + 16.0, y, w - 32.0, 1.0, p.ink(0.06));
            ui.text(label, x + 18.0, y, row, 13.0, p.text);
            for (k, (color, _)) in colors.iter().enumerate() {
                let cx = x + label_w + 15.0 + k as f32 * 36.0;
                let cy = y + row / 2.0;
                let h = hit(*color);
                if *color == current {
                    ui.circle(cx, cy, 15.0, p.accent);
                    ui.circle(cx, cy, 13.0, p.background);
                } else if ui.hover == Some(h.clone()) {
                    ui.circle(cx, cy, 15.0, p.ink(0.25));
                }
                ui.circle(cx, cy, 11.0, theme::Rgba::hex(*color, 1.0));
                // Liseré : un fond proche de celui de la carte reste visible
                ui.stroke(cx - 11.0, cy - 11.0, 22.0, 22.0, 11.0, p.ink(0.18));
                ui.hit((cx - 16.0, cy - 16.0, 32.0, 32.0), h);
            }
        };
        swatches(
            ui,
            y + row,
            "Accent",
            CUSTOM_ACCENTS,
            c.accent,
            &Hit::CustomAccent,
        );
        swatches(
            ui,
            y + 2.0 * row,
            "Fond",
            custom_backgrounds(c.dark),
            c.background,
            &Hit::CustomBg,
        );
        y += 3.0 * row + 12.0;
        // Nom de la couleur survolée, ou celles en vigueur
        let named = |list: &[(u32, &'static str)], v: u32| {
            list.iter().find(|(c, _)| *c == v).map_or("", |(_, n)| *n)
        };
        let hovered = match ui.hover {
            Some(Hit::CustomAccent(v)) => Some(format!("Accent : {}", named(CUSTOM_ACCENTS, v))),
            Some(Hit::CustomBg(v)) => {
                Some(format!("Fond : {}", named(custom_backgrounds(c.dark), v)))
            }
            _ => None,
        };
        let text = hovered.unwrap_or_else(|| {
            format!(
                "Accent {} sur fond {}. Le texte, les panneaux, la barre latérale et les liserés s'accordent d'eux-mêmes ; toucher un choix met ce thème en vigueur.",
                lower_first(named(CUSTOM_ACCENTS, c.accent)),
                lower_first(named(custom_backgrounds(c.dark), c.background))
            )
        });
        y = ui.note(x, y, w, &text) + 24.0;

        // Flou d'arrière-plan (« Réduire la transparence » de macOS, à l'envers)
        let p = theme::palette();
        y = ui.section(x, y, "Transparence");
        let rows = if self.settings.blur { 2.0 } else { 1.0 };
        ui.card(x, y, w, rows * ROW_H);
        ui.text(
            "Flou sous la barre, le Dock et les barres latérales",
            x + 18.0,
            y,
            ROW_H,
            13.5,
            p.text,
        );
        ui.switch(
            x + w - 18.0 - 40.0,
            y + 14.0,
            self.settings.blur,
            Hit::BlurToggle,
        );
        y += ROW_H;
        if self.settings.blur {
            ui.rect(x + 16.0, y, w - 32.0, 1.0, p.ink(0.06));
            ui.text("Niveau", x + 18.0, y, ROW_H, 13.5, p.text);
            let (tx, tw) = (x + 120.0, w - 120.0 - 90.0);
            let level = self.settings.transparency;
            ui.slider(tx, y + 20.0, tw, level as f32 / 100.0, Hit::BlurLevel);
            let label = match level {
                0 => "opaque".to_string(),
                50 => "50 % (défaut)".to_string(),
                l => format!("{l} %"),
            };
            ui.text(&label, x + w - 78.0, y, ROW_H, 12.5, p.text_dim);
            y += ROW_H;
        }
        y += 8.0;
        ui.note(
            x,
            y,
            w,
            "Le dessous transparaît, flouté, à travers la barre, le Dock, la roue, les panneaux et les barres latérales de Fichiers, Réglages et Notes (on y devine la fenêtre posée dessous). Le curseur règle la transparence, d'opaque à très transparent ; désactivé, tout redevient opaque.",
        )
    }

    fn page_fond(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let current = self.settings.wallpaper.clone();
        let mut y = ui.section(x, y, "Fonds d'odysseus");
        let cols = 3;
        let gap = 14.0;
        let tw = (w - gap * (cols - 1) as f32) / cols as f32;
        let th = tw * 9.0 / 16.0;
        let builtin = self
            .fonds
            .iter()
            .take_while(|f| !f.path.starts_with(fonds::user_dir().unwrap_or_default()))
            .count();
        // Uni, puis les dégradés d'odysseus
        let mut items: Vec<(Option<usize>, String)> = vec![(None, "Uni".into())];
        items.extend((0..builtin).map(|i| (Some(i), self.fonds[i].name.clone())));
        let grid = |ui: &mut Ui, y0: f32, items: &[(Option<usize>, String)]| -> f32 {
            let mut bottom = y0;
            for (k, (index, name)) in items.iter().enumerate() {
                let (col, row) = (k % cols, k / cols);
                let tx = x + col as f32 * (tw + gap);
                let ty = y0 + row as f32 * (th + 40.0);
                let (hit, selected) = match index {
                    None => (Hit::FondUni, current.as_deref() == Some("uni")),
                    Some(i) => (
                        Hit::Fond(*i),
                        current.as_deref() == Some(&*self.fonds[*i].path.to_string_lossy()),
                    ),
                };
                // Vos images : une croix au survol pour les retirer
                let removable = index.filter(|i| *i >= builtin);
                let delete_hovered =
                    removable.is_some_and(|i| ui.hover == Some(Hit::FondDelete(i)));
                let hovered = ui.hover == Some(hit.clone()) || delete_hovered;
                if selected {
                    ui.rounded(tx - 4.0, ty - 4.0, tw + 8.0, th + 8.0, 14.0, p.accent);
                }
                match index.and_then(|i| self.fonds[i].thumb.as_ref()) {
                    Some(img) => ui.image(img, tx, ty, tw, th, 10.0),
                    None if index.is_none() => {
                        ui.rounded(tx, ty, tw, th, 10.0, p.background);
                        ui.stroke(tx, ty, tw, th, 10.0, p.ink(0.15));
                    }
                    None => {
                        ui.rounded(tx, ty, tw, th, 10.0, p.ink(0.06));
                        ui.centered("…", tx + tw / 2.0, ty, th, 16.0, false, p.text_dim);
                    }
                }
                if hovered && !selected {
                    ui.stroke(tx, ty, tw, th, 10.0, p.ink(0.35));
                }
                let color = if selected { p.accent } else { p.text_soft() };
                let label = ui.truncate(name, 12.5, tw);
                ui.centered(
                    &label,
                    tx + tw / 2.0,
                    ty + th + 6.0,
                    22.0,
                    12.5,
                    selected,
                    color,
                );
                ui.hit((tx, ty, tw, th + 28.0), hit);
                if let Some(i) = removable.filter(|_| hovered) {
                    let (cx, cy) = (tx + tw - 16.0, ty + 16.0);
                    let fill = if delete_hovered {
                        p.accent
                    } else {
                        common::theme::Rgba::hex(0x000000, 0.6)
                    };
                    ui.circle(cx, cy, 11.0, fill);
                    ui.icon(
                        icon::CLOSE,
                        cx,
                        cy,
                        12.0,
                        common::theme::Rgba::hex(0xffffff, 1.0),
                    );
                    // Après la vignette : la croix l'emporte sous le pointeur
                    ui.hit((cx - 13.0, cy - 13.0, 26.0, 26.0), Hit::FondDelete(i));
                }
                bottom = ty + th + 40.0;
            }
            bottom
        };
        y = grid(ui, y, &items) + 8.0;

        // Images de l'utilisateur, et zone de dépôt
        y = ui.section(x, y, "Vos images");
        let user: Vec<(Option<usize>, String)> = (builtin..self.fonds.len())
            .map(|i| (Some(i), self.fonds[i].name.clone()))
            .collect();
        if !user.is_empty() {
            y = grid(ui, y, &user) + 8.0;
        }
        let dh = 96.0;
        let border = if self.drop_hover {
            p.accent
        } else {
            p.ink(0.18)
        };
        if self.drop_hover {
            ui.rounded(x, y, w, dh, 12.0, p.accent.with_alpha(0.12));
        }
        ui.dashed(x, y, w, dh, 12.0, border);
        ui.icon(
            icon::IMAGE_PLUS,
            x + w / 2.0,
            y + 32.0,
            22.0,
            if self.drop_hover {
                p.accent
            } else {
                p.text_dim
            },
        );
        ui.centered(
            "Glissez une image ici depuis Fichiers",
            x + w / 2.0,
            y + 50.0,
            22.0,
            13.0,
            false,
            p.text_soft(),
        );
        ui.centered(
            "Elle est copiée dans vos fonds et appliquée aussitôt",
            x + w / 2.0,
            y + 70.0,
            18.0,
            11.5,
            false,
            p.text_dim,
        );
        y += dh + 28.0;

        // Changement au hasard parmi vos images
        y = ui.section(x, y, "Changer au hasard");
        let sel = WALLPAPER_ROTATION
            .iter()
            .position(|(s, _)| *s == self.settings.wallpaper_rotation);
        ui.segmented(
            x,
            y,
            w,
            &WALLPAPER_ROTATION.iter().map(|r| r.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::FondRotation(WALLPAPER_ROTATION[i].0),
        );
        y += 52.0;
        let n = self.fonds.len().saturating_sub(builtin);
        let note = match n {
            0 => "Ajoutez d'abord des images dans « Vos images » : le hasard pioche parmi elles."
                .to_string(),
            1 => "Une seule image dans « Vos images » : ajoutez-en d'autres pour qu'elle change."
                .to_string(),
            n => format!(
                "Le fond change au hasard parmi vos {n} images, en fondu enchaîné. Rien ne change pendant que l'écran est éteint ou verrouillé."
            ),
        };
        ui.note(x, y, w, &note)
    }

    fn page_ecran(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let Some(screen) = self.screen.as_ref() else {
            return ui.note(
                x,
                y,
                w,
                "Le compositeur ne répond pas : réglages de l'écran indisponibles.",
            );
        };
        // Illustration : l'écran, avec le fond d'écran actuel dedans
        let (mw, mh) = (300.0, 169.0);
        let mx = x + (w - mw) / 2.0;
        ui.rounded(
            mx - 8.0,
            y - 8.0,
            mw + 16.0,
            mh + 16.0,
            14.0,
            Rgba::hex(0x15161b, 1.0),
        );
        // Cadre sombre quel que soit le thème, détaché par un liseré
        ui.stroke(mx - 8.0, y - 8.0, mw + 16.0, mh + 16.0, 14.0, p.ink(0.18));
        let wallpaper = self
            .fonds
            .iter()
            .find(|f| self.settings.wallpaper.as_deref() == Some(&*f.path.to_string_lossy()));
        match wallpaper.and_then(|f| f.thumb.as_ref()) {
            Some(img) => ui.image(img, mx, y, mw, mh, 6.0),
            None => ui.rounded(mx, y, mw, mh, 6.0, p.background),
        }
        ui.rect(mx, y, mw, 8.0, p.surface);
        ui.rounded(
            mx + mw / 2.0 - 30.0,
            y + mh + 8.0,
            60.0,
            22.0,
            3.0,
            Rgba::hex(0x6e737d, 1.0),
        );
        ui.rounded(
            mx + mw / 2.0 - 60.0,
            y + mh + 28.0,
            120.0,
            6.0,
            3.0,
            Rgba::hex(0x8a8f9a, 1.0),
        );
        let (cw, ch, cr) = screen.current;
        let size = screen
            .inches()
            .map(|i| format!(" · {:.0} pouces", i))
            .unwrap_or_default();
        ui.centered(
            &format!("{}{size}", screen.name),
            x + w / 2.0,
            y + mh + 42.0,
            22.0,
            14.0,
            true,
            p.text,
        );
        ui.centered(
            &format!(
                "{cw} × {ch} à {cr} Hz · échelle {} %",
                (screen.scale * 100.0).round()
            ),
            x + w / 2.0,
            y + mh + 64.0,
            18.0,
            12.0,
            false,
            p.text_dim,
        );
        let mut y = y + mh + 100.0;

        // Taille du texte et des fenêtres
        y = ui.section(x, y, "Taille du texte et des fenêtres");
        let labels: Vec<String> = SCALES
            .iter()
            .map(|s| format!("{} %", (s * 100.0) as u32))
            .collect();
        let selected = SCALES.iter().position(|s| (s - screen.scale).abs() < 0.01);
        ui.segmented(
            x,
            y,
            w,
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            selected,
            Hit::Scale,
        );
        let looks = format!(
            "Ressemble à {} × {}",
            (cw as f64 / screen.scale).round(),
            (ch as f64 / screen.scale).round()
        );
        ui.text("Plus d'espace", x, y + 38.0, 18.0, 11.5, p.text_dim);
        ui.centered(
            &looks,
            x + w / 2.0,
            y + 38.0,
            18.0,
            11.5,
            false,
            p.text_soft(),
        );
        let lw = ui.measure("Plus grand", 11.5);
        ui.text("Plus grand", x + w - lw, y + 38.0, 18.0, 11.5, p.text_dim);
        y += 72.0;

        // Définition
        y = ui.section(x, y, "Définition");
        let resolutions: Vec<(i32, i32)> = screen.resolutions().into_iter().take(8).collect();
        let card_y = y;
        ui.card(x, y, w, resolutions.len() as f32 * 44.0);
        for (i, (rw, rh)) in resolutions.iter().enumerate() {
            let ry = card_y + i as f32 * 44.0;
            let hit = Hit::Resolution(*rw, *rh);
            if ui.hover == Some(hit.clone()) {
                ui.rounded(x + 4.0, ry + 3.0, w - 8.0, 38.0, 8.0, p.ink(0.05));
            }
            let current = (*rw, *rh) == (cw, ch);
            ui.text(
                &format!("{rw} × {rh}"),
                x + 18.0,
                ry,
                44.0,
                13.5,
                if current { p.text } else { p.text_soft() },
            );
            if (*rw, *rh) == (screen.preferred.0, screen.preferred.1) {
                let tx = x + 18.0 + ui.measure(&format!("{rw} × {rh}"), 13.5) + 10.0;
                ui.pill(tx, ry + 12.0, "recommandée");
            }
            if current {
                ui.icon(icon::CHECK, x + w - 24.0, ry + 22.0, 15.0, p.accent);
            }
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.hit((x, ry, w, 44.0), hit);
        }
        y += resolutions.len() as f32 * 44.0 + 24.0;

        // Fréquence
        let rates = screen.refreshes(cw, ch);
        if rates.len() > 1 {
            y = ui.section(x, y, "Fréquence de rafraîchissement");
            let labels: Vec<String> = rates.iter().map(|r| format!("{r} Hz")).collect();
            let sel = rates.iter().position(|r| *r == cr);
            let rates_c = rates.clone();
            ui.segmented(
                x,
                y,
                w.min(120.0 * rates.len() as f32),
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
                sel,
                move |i| Hit::Refresh(rates_c[i]),
            );
            y += 56.0;
        }

        // Lumière de nuit
        y = ui.section(x, y, "Lumière de nuit");
        let modes = [
            ("off", "Désactivée"),
            ("auto", "Automatique"),
            ("on", "Activée"),
        ];
        let sel = modes.iter().position(|(m, _)| *m == self.night);
        ui.segmented(
            x,
            y,
            w,
            &modes.iter().map(|m| m.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::Night(modes[i].0),
        );
        y += 48.0;
        ui.card(x, y, w, 2.0 * ROW_H);
        // Horaires
        let from = self
            .settings
            .night_from
            .clone()
            .unwrap_or_else(|| "21:00".into());
        let to = self
            .settings
            .night_to
            .clone()
            .unwrap_or_else(|| "07:00".into());
        ui.text(
            "Horaires (mode automatique)",
            x + 18.0,
            y,
            ROW_H,
            13.5,
            p.text,
        );
        let sx = x + w - 18.0 - 2.0 * 112.0 - 28.0;
        ui.stepper(sx, y + 10.0, &from, Hit::NightFrom(-30), Hit::NightFrom(30));
        ui.text("à", sx + 118.0, y, ROW_H, 13.0, p.text_dim);
        ui.stepper(
            sx + 140.0,
            y + 10.0,
            &to,
            Hit::NightTo(-30),
            Hit::NightTo(30),
        );
        ui.rect(x + 16.0, y + ROW_H, w - 32.0, 1.0, p.ink(0.06));
        // Température
        let temp = self.settings.night_temp.unwrap_or(3400);
        ui.text("Teinte", x + 18.0, y + ROW_H, ROW_H, 13.5, p.text);
        let (tx, tw) = (x + 120.0, w - 120.0 - 90.0);
        let frac = ((temp as f32 - 2500.0) / 3000.0).clamp(0.0, 1.0);
        ui.warm_slider(tx, y + ROW_H + 20.0, tw, frac, Hit::NightTemp);
        ui.text(
            &format!("{temp} K"),
            x + w - 70.0,
            y + ROW_H,
            ROW_H,
            12.5,
            p.text_dim,
        );
        y += 2.0 * ROW_H + 8.0;
        ui.note(x, y, w, "Plus la teinte est chaude, plus l'écran tire vers l'orangé le soir. Les horaires valent pour le mode automatique.")
    }

    fn page_clavier(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Disposition du clavier");
        let sel = KEYBOARDS
            .iter()
            .position(|(k, _)| *k == self.settings.keyboard);
        y = ui.radio_list(
            x,
            y,
            w,
            &KEYBOARDS
                .iter()
                .map(|k| k.1.to_string())
                .collect::<Vec<_>>(),
            sel,
            Hit::Keyboard,
        );
        // Zone d'essai
        y += 14.0;
        ui.rounded(x, y, w, 44.0, 10.0, p.ink(0.05));
        ui.stroke(x, y, w, 44.0, 10.0, p.accent.with_alpha(0.5));
        if self.typed.is_empty() {
            ui.text(
                "Essayez ici : tapez quelques touches (@, <, accents…)",
                x + 16.0,
                y,
                44.0,
                13.0,
                p.text_dim,
            );
        } else {
            ui.text(&self.typed, x + 16.0, y, 44.0, 14.0, p.text);
        }
        let caret = x + 16.0 + ui.measure(&self.typed, 14.0) + 2.0;
        if !self.typed.is_empty() {
            ui.rect(caret, y + 12.0, 1.5, 20.0, p.accent);
        }
        y += 44.0 + 28.0;

        y = ui.section(x, y, "Délai avant répétition d'une touche maintenue");
        let sel = REPEAT_DELAY
            .iter()
            .position(|(d, _)| *d == self.settings.repeat_delay);
        ui.segmented(
            x,
            y,
            w,
            &REPEAT_DELAY.iter().map(|d| d.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::RepeatDelay(REPEAT_DELAY[i].0),
        );
        y += 52.0;
        y = ui.section(x, y, "Vitesse de répétition");
        let sel = REPEAT_RATE
            .iter()
            .position(|(r, _)| *r == self.settings.repeat_rate);
        ui.segmented(
            x,
            y,
            w,
            &REPEAT_RATE.iter().map(|r| r.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::RepeatRate(REPEAT_RATE[i].0),
        );
        y += 60.0;

        y = ui.section(x, y, "Langue du système");
        let names: Vec<String> = self
            .locales
            .iter()
            .map(|l| system::locale_name(l))
            .collect();
        let sel = self
            .locales
            .iter()
            .position(|l| Some(l) == self.locale.as_ref());
        y = ui.radio_list(x, y, w, &names, sel, Hit::Locale);
        ui.note(
            x,
            y + 8.0,
            w,
            "La langue s'applique aux applications (navigateur, terminal…) à la prochaine connexion. Les applications d'odysseus restent en français.",
        )
    }

    fn page_dock(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = y;
        ui.card(x, y, w, ROW_H);
        ui.text("Afficher le Dock", x + 18.0, y, ROW_H, 13.5, p.text);
        ui.switch(
            x + w - 18.0 - 40.0,
            y + 14.0,
            self.settings.dock,
            Hit::DockToggle,
        );
        y += ROW_H + 8.0;
        if !self.settings.dock {
            return ui.note(
                x,
                y,
                w,
                "Sans Dock, la roue (Super+R) et le lanceur (Super+Espace) ouvrent les applis ; choisir dans la roue une appli dont la fenêtre est réduite la ramène.",
            );
        }
        y += 20.0;
        let mut y = ui.section(x, y, "Masquer le Dock");
        let sel = DOCK_HIDE
            .iter()
            .position(|(m, _)| *m == self.settings.dock_hide);
        ui.segmented(
            x,
            y,
            w,
            &DOCK_HIDE.iter().map(|d| d.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::DockHide(DOCK_HIDE[i].0),
        );
        y += 52.0;
        y = ui.note(
            x,
            y,
            w,
            "Masqué, il revient quand la souris touche le bas de l'écran. Il ne réserve jamais de place : une fenêtre agrandie prend tout l'écran.",
        ) + 20.0;
        y = ui.section(x, y, "Taille des icônes");
        let sel = DOCK_SIZE
            .iter()
            .position(|(s, _)| *s == self.settings.dock_size);
        ui.segmented(
            x,
            y,
            w,
            &DOCK_SIZE.iter().map(|d| d.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::DockSize(DOCK_SIZE[i].0),
        );
        y += 52.0;
        ui.note(
            x,
            y,
            w,
            "Clic droit sur une icône : garder dans le Dock ou l'en retirer, nouvelle fenêtre, quitter. Une fenêtre réduite rentre dans l'icône de son appli (un anneau la signale) ; un clic la ramène.",
        )
    }

    fn page_reception(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Recevoir des fichiers");
        ui.card(x, y, w, ROW_H);
        ui.text(
            "Accepter les envois des appareils du réseau local",
            x + 18.0,
            y,
            ROW_H,
            13.5,
            p.text,
        );
        ui.switch(
            x + w - 18.0 - 40.0,
            y + 14.0,
            self.settings.reception,
            Hit::ReceptionToggle,
        );
        y += ROW_H + 12.0;
        let name = std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "odysseus".into());
        y = ui.note(
            x,
            y,
            w,
            &format!(
                "Sur le Mac, l'iPhone ou un autre ordinateur, installez LocalSend (gratuit, sur l'App Store et localsend.org). «\u{a0}{name}\u{a0}» apparaît dans la liste de ses appareils : glissez-y des fichiers ou un dossier."
            ),
        ) + 12.0;
        ui.note(
            x,
            y,
            w,
            "Chaque envoi vous est demandé dans une notification (Accepter ou Refuser) ; les fichiers arrivent dans Téléchargements, sans jamais en remplacer un autre. Un message texte s'affiche avec un bouton « Copier ». Les échanges sont chiffrés, et seuls les appareils du réseau local sont entendus.",
        )
    }

    fn page_energie(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Éteindre l'écran après");
        let sel = SCREEN_OFF
            .iter()
            .position(|(m, _)| *m == self.settings.screen_off_min);
        ui.segmented(
            x,
            y,
            w,
            &SCREEN_OFF.iter().map(|s| s.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::ScreenOff(SCREEN_OFF[i].0),
        );
        y += 52.0;
        ui.card(x, y, w, ROW_H);
        ui.text(
            "Verrouiller la session quand l'écran s'éteint",
            x + 18.0,
            y,
            ROW_H,
            13.5,
            p.text,
        );
        ui.switch(
            x + w - 18.0 - 40.0,
            y + 14.0,
            self.settings.lock_on_idle,
            Hit::LockToggle,
        );
        y += ROW_H + 28.0;

        y = ui.section(x, y, "Mettre l'ordinateur en veille après");
        let sel = SUSPEND
            .iter()
            .position(|(m, _)| *m == self.settings.suspend_min);
        ui.segmented(
            x,
            y,
            w,
            &SUSPEND.iter().map(|s| s.1).collect::<Vec<_>>(),
            sel,
            |i| Hit::Suspend(SUSPEND[i].0),
        );
        y += 52.0;
        ui.button(
            x,
            y,
            230.0,
            "Mettre en veille maintenant",
            false,
            Hit::SleepNow,
        );
        y += 56.0;
        ui.note(
            x,
            y,
            w,
            "Une vidéo en lecture empêche l'extinction de l'écran. Écran verrouillé, il se rééteint au bout de 30 secondes.",
        )
    }

    fn page_raccourcis(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        use common::shortcuts::{LEFT, RIGHT};
        let mut y = y;
        for (title, rows) in LEFT.iter().chain(RIGHT.iter()) {
            y = ui.section(x, y, title);
            ui.card(x, y, w, rows.len() as f32 * 40.0);
            for (i, (keys, label)) in rows.iter().enumerate() {
                let ry = y + i as f32 * 40.0;
                if i > 0 {
                    ui.rect(x + 16.0, ry, w - 32.0, 1.0, theme::palette().ink(0.06));
                }
                ui.text(
                    label,
                    x + 18.0,
                    ry,
                    40.0,
                    13.0,
                    theme::palette().text_soft(),
                );
                ui.keycaps(x + w - 16.0, ry + 9.0, keys);
            }
            y += rows.len() as f32 * 40.0 + 24.0;
        }
        y
    }

    /// Les faits de la mémoire, une ligne chacun avec date et origine ; à la
    /// relecture (`review`), un bouton Garder s'ajoute à Oublier. Renvoie le bas.
    fn memory_rows(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        w: f32,
        facts: &[common::memory::Fact],
        review: bool,
    ) -> f32 {
        let p = theme::palette();
        let rh = 58.0;
        let buttons = if review { 2.0 * 110.0 + 8.0 } else { 110.0 };
        ui.card(x, y, w, facts.len() as f32 * rh);
        for (i, f) in facts.iter().enumerate() {
            let ry = y + i as f32 * rh;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            let text = ui.truncate(&f.text, 13.0, w - 50.0 - buttons);
            ui.text(&text, x + 18.0, ry + 8.0, 22.0, 13.0, p.text);
            let mut origin = match (f.date, &f.origin) {
                (None, None) => "écrit à la main".to_string(),
                (Some(t), None) => format!("retenu le {}", fact_date(t)),
                (Some(t), Some(o)) => format!("retenu le {} · {o}", fact_date(t)),
                (None, Some(o)) => o.clone(),
            };
            if let Some(t) = f.updated {
                origin.push_str(&format!(" · mis à jour le {}", fact_date(t)));
            }
            if let Some(t) = f.kept {
                origin.push_str(&format!(" · gardé le {}", fact_date(t)));
            }
            let origin = ui.truncate(&origin, 11.5, w - 50.0 - buttons);
            ui.text(&origin, x + 18.0, ry + 31.0, 18.0, 11.5, p.text_dim);
            let armed = self
                .forget_armed
                .as_ref()
                .is_some_and(|(a, at)| *a == f.text && at.elapsed() < Duration::from_secs(4));
            let bx = x + w - 18.0 - 110.0;
            ui.button(
                bx,
                ry + 12.0,
                110.0,
                if armed { "Confirmer ?" } else { "Oublier" },
                false,
                Hit::Forget(f.text.clone()),
            );
            if review {
                ui.button(
                    bx - 118.0,
                    ry + 12.0,
                    110.0,
                    "Garder",
                    true,
                    Hit::KeepFact(f.text.clone()),
                );
            }
        }
        y + facts.len() as f32 * rh
    }

    fn page_agent(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.section(x, y, "Agent du bureau");
        let st = &self.agent_status;
        let task = |t: &agents::Task| self.tasks.iter().find(|(k, ..)| k == t).cloned();

        // Claude Code
        let claude_task = task(&agents::Task::InstallClaude).or(task(&agents::Task::UpdateClaude));
        let ch = 112.0 + if claude_task.is_some() { 30.0 } else { 0.0 };
        ui.card(x, y, w, ch);
        let pill = match &st.claude_version {
            Some(v) => (format!("version {v}"), p.success),
            None => ("non installé".into(), p.text_dim),
        };
        agent_header(
            ui,
            x,
            y,
            w,
            0,
            self.agent.agent == "claude",
            AGENTS[0].1,
            AGENTS[0].2,
            &pill,
        );
        let line = match (&st.claude_version, st.claude_logged_in) {
            (None, _) => "Installeur officiel d'Anthropic, dans ~/.local/bin, sans droits root.",
            (Some(_), true) => {
                "Connecté. Les questions et ce que vous autorisez partent chez Anthropic."
            }
            (Some(_), false) => {
                "Pas encore connecté : « Se connecter » ouvre un terminal (/login)."
            }
        };
        ui.text(line, x + 44.0, y + 46.0, 18.0, 11.5, p.text_dim);
        let by = y + 70.0;
        match &claude_task {
            Some((_, frac, text)) => progress(ui, x + 44.0, by + 4.0, w - 62.0, *frac, text),
            None if st.claude_version.is_none() => ui.button(
                x + 44.0,
                by,
                170.0,
                "Installer Claude Code",
                true,
                Hit::InstallClaude,
            ),
            None => {
                ui.button(
                    x + 44.0,
                    by,
                    130.0,
                    "Mettre à jour",
                    false,
                    Hit::UpdateClaude,
                );
                ui.button(
                    x + 184.0,
                    by,
                    130.0,
                    "Se connecter",
                    !st.claude_logged_in,
                    Hit::ClaudeLogin,
                );
            }
        }
        y += ch + 12.0;

        // Ollama
        let install = task(&agents::Task::InstallOllama);
        let oh = if st.ollama_running {
            70.0 + 36.0 + self.agent.models.len() as f32 * 56.0 + 2.0
        } else {
            112.0
        } + if install.is_some() { 30.0 } else { 0.0 };
        ui.card(x, y, w, oh);
        let pill = if st.ollama_running {
            (format!("version {}", st.ollama_version), p.success)
        } else if st.ollama_installed {
            ("service arrêté".into(), p.danger)
        } else {
            ("non installé".into(), p.text_dim)
        };
        agent_header(
            ui,
            x,
            y,
            w,
            1,
            self.agent.agent == "ollama",
            AGENTS[1].1,
            AGENTS[1].2,
            &pill,
        );
        let line = if st.ollama_running {
            match &st.acceleration {
                Some(acc) => format!("Calcul : {acc}"),
                None => "Calcul : connu au premier usage d'un modèle".to_string(),
            }
        } else if st.ollama_installed {
            "Installé ; démarrez-le : systemctl --user start ollama".to_string()
        } else {
            "Archive officielle (1,4 Go, empreinte vérifiée), dans ~/.local/opt/ollama, sans droits root.".to_string()
        };
        let line = ui.truncate(&line, 11.5, w - 62.0);
        ui.text(&line, x + 44.0, y + 46.0, 18.0, 11.5, p.text_dim);
        let mut by = y + 70.0;
        if let Some((_, frac, text)) = &install {
            progress(ui, x + 44.0, by + 4.0, w - 62.0, *frac, text);
            by += 30.0;
        } else if !st.ollama_installed {
            ui.button(
                x + 44.0,
                by,
                160.0,
                "Installer Ollama",
                true,
                Hit::InstallOllama,
            );
        }
        if st.ollama_running {
            ui.text(
                "Modèles conseillés pour votre carte (16 Go)",
                x + 44.0,
                by,
                24.0,
                12.0,
                p.text_soft(),
            );
            let mut ry = by + 36.0;
            for card in &self.agent.models {
                let (name, label, size, why) = (
                    card.model.as_str(),
                    card.name.as_str(),
                    card.size.as_str(),
                    card.notes.as_str(),
                );
                ui.rect(x + 44.0, ry - 4.0, w - 62.0, 1.0, p.ink(0.06));
                let installed = st.models.iter().any(|(n, _)| n == name);
                let chosen = installed && self.agent.ollama_model == name;
                // Pastille de choix : seulement pour un modèle présent
                let (cx, cy) = (x + 54.0, ry + 20.0);
                if chosen {
                    ui.circle(cx, cy, 7.0, p.accent);
                    ui.circle(cx, cy, 2.5, p.on_accent);
                } else if installed {
                    ui.circle(cx, cy, 7.0, p.ink(0.25));
                    ui.circle(cx, cy, 5.5, p.tile());
                    ui.hit((x + 40.0, ry, 30.0, 40.0), Hit::UseModel(name.to_string()));
                }
                ui.bold_text(label, x + 70.0, ry + 2.0, 20.0, 13.0, p.text);
                let lw = ui.measure(label, 13.0);
                ui.text(size, x + 78.0 + lw, ry + 2.0, 20.0, 11.5, p.text_dim);
                let why = ui.truncate(why, 11.5, w - 260.0);
                ui.text(&why, x + 70.0, ry + 21.0, 18.0, 11.5, p.text_dim);
                let pull = task(&agents::Task::Pull(name.to_string()));
                let bx = x + w - 18.0 - 130.0;
                match pull {
                    Some((_, frac, text)) => progress(ui, bx - 40.0, ry + 10.0, 170.0, frac, &text),
                    None if installed => {
                        let armed = self.delete_armed.as_ref().is_some_and(|(m, at)| {
                            m == name && at.elapsed() < Duration::from_secs(4)
                        });
                        let label = if armed { "Confirmer ?" } else { "Supprimer" };
                        ui.button(
                            bx,
                            ry + 3.0,
                            130.0,
                            label,
                            false,
                            Hit::DeleteModel(name.to_string()),
                        );
                    }
                    // Seul le modèle recommandé est mis en avant
                    None => ui.button(
                        bx,
                        ry + 3.0,
                        130.0,
                        "Télécharger",
                        Some(name) == self.agent.models.first().map(|m| m.model.as_str()),
                        Hit::PullModel(name.to_string()),
                    ),
                }
                ry += 56.0;
            }
        }
        y += oh + 12.0;

        // Commande personnalisée
        ui.card(x, y, w, 56.0);
        let pill = (String::new(), p.text_dim);
        agent_header(
            ui,
            x,
            y,
            w,
            2,
            self.agent.agent == "commande",
            AGENTS[2].1,
            AGENTS[2].2,
            &pill,
        );
        y += 56.0 + 8.0;
        y = ui.note(
            x,
            y,
            w,
            "L'agent choisi répond dans le panneau Assistant (icône de la barre ou Super+A). Tout agent compatible MCP peut aussi se brancher sur odysseus-mcp.",
        ) + 4.0;
        ui.button(
            x,
            y,
            210.0,
            "Modifier agent.json",
            false,
            Hit::EditAgentJson,
        );
        let hint = "modèles, commandes, adresse d'Ollama, autorisations, suggestions";
        ui.text(hint, x + 222.0, y, 34.0, 11.5, p.text_dim);
        y += 34.0 + 24.0;

        // Autorisations
        y = ui.section(x, y, "Ce que l'agent peut faire sur ce bureau");
        let rh = 66.0;
        ui.card(x, y, w, PERMS.len() as f32 * rh);
        let perms = self.agent.perms;
        let values = [
            perms.read,
            perms.docs,
            perms.files,
            perms.notes,
            perms.memory,
            perms.desktop,
            perms.screen,
            perms.act,
            perms.clipboard,
            perms.run,
            perms.run_auto,
        ];
        for (i, (title, detail)) in PERMS.iter().enumerate() {
            let ry = y + i as f32 * rh;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.text(title, x + 18.0, ry + 8.0, 22.0, 13.5, p.text);
            let lines = ui.regular.wrap(detail, 11.5 * ui.s, (w - 110.0) * ui.s, 2);
            for (k, line) in lines.iter().enumerate() {
                ui.text(
                    line,
                    x + 18.0,
                    ry + 30.0 + k as f32 * 15.0,
                    15.0,
                    11.5,
                    p.text_dim,
                );
            }
            ui.switch(
                x + w - 18.0 - 40.0,
                ry + (rh - 24.0) / 2.0,
                values[i],
                Hit::Perm(i),
            );
        }
        y += PERMS.len() as f32 * rh + 28.0;

        // Mémoire : les faits retenus, à oublier d'un clic (deux, le second
        // confirme) ; ceux qui ont six mois sans nouvelles d'abord, à relire
        y = ui.section(x, y, "Mémoire de l'Assistant");
        let memory = common::memory::Memory::open();
        let due: Vec<common::memory::Fact> =
            memory.as_ref().map(|m| m.review()).unwrap_or_default();
        let facts: Vec<common::memory::Fact> = memory
            .as_ref()
            .map(|m| m.facts())
            .unwrap_or_default()
            .into_iter()
            .filter(|f| !due.contains(f))
            .collect();
        if due.is_empty() && facts.is_empty() {
            y = ui.note(
                x,
                y,
                w,
                "Rien encore. Quand vous confiez à l'Assistant quelque chose de durable sur vous \
                 ou votre machine, il propose de le retenir ; vous décidez dans une notification. \
                 Vous pouvez aussi écrire les faits vous-même, une ligne chacun.",
            ) + 4.0;
        } else {
            if !due.is_empty() {
                ui.text(
                    &format!(
                        "À relire : {} sans nouvelles depuis six mois. Toujours vrai ?",
                        if due.len() == 1 {
                            "un fait".to_string()
                        } else {
                            format!("{} faits", due.len())
                        }
                    ),
                    x + 2.0,
                    y,
                    22.0,
                    12.5,
                    p.text,
                );
                y += 28.0;
                y = self.memory_rows(ui, x, y, w, &due, true) + 16.0;
            }
            if !facts.is_empty() {
                y = self.memory_rows(ui, x, y, w, &facts, false) + 8.0;
            }
            y = ui.note(
                x,
                y,
                w,
                "Lus en tête de chaque conversation. L'Assistant ne retient ni n'oublie rien \
                 sans votre accord ; jamais un mot de passe ni une clé. Rien n'est oublié tout \
                 seul : un fait qui a passé six mois sans servir vous est proposé à la relecture.",
            ) + 4.0;
        }
        ui.button(x, y, 210.0, "Modifier memoire.md", false, Hit::EditMemory);
        ui.text(
            "une ligne par fait, lue à chaque conversation",
            x + 222.0,
            y,
            34.0,
            11.5,
            p.text_dim,
        );
        y += 34.0 + 24.0;

        // Brancher Claude Code sur odysseus-mcp
        y = ui.section(x, y, "Brancher Claude Code sur ce bureau");
        let cmd = mcp_command();
        ui.rounded(x, y, w, 44.0, 10.0, p.ink(0.06));
        let shown = ui.truncate(&cmd, 12.0, w - 130.0);
        ui.text(&shown, x + 14.0, y, 44.0, 12.0, p.text_soft());
        ui.button(x + w - 104.0, y + 5.0, 96.0, "Copier", false, Hit::CopyMcp);
        y += 52.0;
        let declared = if mcp_declared() {
            "Déjà déclaré dans votre Claude Code : vérifiez avec /mcp dans une session."
        } else {
            "À lancer une fois dans un terminal ; Claude Code démarre ensuite odysseus-mcp lui-même."
        };
        y = ui.note(x, y, w, declared) + 16.0;

        // Activité
        y = ui.section(x, y, "Activité récente des agents");
        let activity = agent_activity();
        if activity.is_empty() {
            return ui.note(x, y, w, "Aucun appel pour l'instant.");
        }
        let rh = 38.0;
        ui.card(x, y, w, activity.len() as f32 * rh);
        for (i, (time, granted, tool, detail)) in activity.iter().enumerate() {
            let ry = y + i as f32 * rh;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.text(time, x + 16.0, ry, rh, 12.0, p.text_dim);
            ui.bold_text(tool, x + 92.0, ry, rh, 12.5, p.text);
            let dx = x + 92.0 + ui.measure(tool, 12.5) + 10.0;
            let d = ui.truncate(detail, 11.5, (x + w - 110.0 - dx).max(0.0));
            ui.text(&d, dx, ry, rh, 11.5, p.text_dim);
            let (label, color) = if *granted {
                ("accordé", p.success)
            } else {
                ("refusé", p.danger)
            };
            let lw = ui.measure(label, 11.0) + 16.0;
            ui.rounded(
                x + w - 14.0 - lw,
                ry + 9.0,
                lw,
                20.0,
                10.0,
                color.with_alpha(0.16),
            );
            ui.centered(
                label,
                x + w - 14.0 - lw / 2.0,
                ry + 9.0,
                20.0,
                11.0,
                false,
                color,
            );
        }
        y + activity.len() as f32 * rh + 16.0
    }

    /// Accueil, première étape
    fn page_bienvenue(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = y + 24.0;
        ui.circle(x + w / 2.0, y + 40.0, 40.0, p.accent.with_alpha(0.16));
        ui.icon(icon::SPARKLES, x + w / 2.0, y + 40.0, 36.0, p.accent);
        y += 100.0;
        ui.centered(
            "Bienvenue sur odysseus",
            x + w / 2.0,
            y,
            36.0,
            24.0,
            true,
            p.text,
        );
        y += 44.0;
        y = ui.note(
            x,
            y,
            w,
            "Un poste de pilotage sobre. Quelques réglages pour qu'il vous ressemble : \
             apparence, fond d'écran, écran, clavier et agent. Tout se retrouve ensuite \
             dans Réglages, et cet accueil reste accessible depuis Réglages > À propos.",
        ) + 20.0;
        let steps = [
            (icon::PALETTE, "Apparence", "le thème du bureau"),
            (
                icon::IMAGE,
                "Fond d'écran",
                "un dégradé d'odysseus ou votre image",
            ),
            (icon::MONITOR, "Écran", "la taille de ce qui s'affiche"),
            (icon::KEYBOARD, "Clavier", "la disposition des touches"),
            (
                icon::BOT,
                "Agent",
                "l'assistant qui peut piloter le bureau, si vous le voulez",
            ),
        ];
        let rh = 52.0;
        ui.card(x, y, w, steps.len() as f32 * rh);
        for (i, (glyph, title, detail)) in steps.iter().enumerate() {
            let ry = y + i as f32 * rh;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.icon(*glyph, x + 30.0, ry + rh / 2.0, 16.0, p.accent);
            ui.bold_text(title, x + 54.0, ry + 7.0, 20.0, 13.5, p.text);
            ui.text(detail, x + 54.0, ry + 27.0, 18.0, 12.0, p.text_dim);
        }
        y + steps.len() as f32 * rh
    }

    /// Accueil, dernière étape : l'essentiel en quelques gestes
    fn page_fin(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.note(
            x,
            y,
            w,
            "Le bureau est prêt. L'essentiel en quelques gestes :",
        ) + 10.0;
        let gestures = [
            (
                "Super + Espace",
                "lancer une appli, ou poser une question avec « ? »",
            ),
            (
                "Le Dock, en bas",
                "vos applis ; une fenêtre réduite rentre dans son icône",
            ),
            ("Super + A", "l'Assistant"),
            (
                "Super + Q / M / F",
                "fermer, réduire, agrandir la fenêtre (ou ses boutons)",
            ),
            ("Super + 1 … 9", "changer de bureau"),
            ("Impr. écran", "capture d'une zone ; Espace : une fenêtre"),
            ("Super + K", "tous les raccourcis"),
        ];
        let rh = 44.0;
        ui.card(x, y, w, gestures.len() as f32 * rh);
        for (i, (keys, what)) in gestures.iter().enumerate() {
            let ry = y + i as f32 * rh;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.bold_text(keys, x + 18.0, ry, rh, 13.0, p.text);
            let what = ui.truncate(what, 12.5, w - 200.0);
            ui.text(&what, x + 190.0, ry, rh, 12.5, p.text_soft());
        }
        y += gestures.len() as f32 * rh + 16.0;
        ui.note(
            x,
            y,
            w,
            "Tout se règle ensuite dans Réglages (icône d'engrenage du Dock, ou centre de contrôle).",
        )
    }

    /// Pied de page de l'accueil : étapes, précédent, suivant
    fn wizard_footer(&self, ui: &mut Ui, step: usize, cx: f32, cw: f32) {
        let p = theme::palette();
        let (w, h) = (self.width as f32, self.height as f32);
        let y = h - FOOTER_H;
        ui.rect(0.0, y, w, FOOTER_H, p.content);
        ui.rect(0.0, y, w, 1.0, p.ink(0.06));
        // Points des étapes
        let n = WIZARD.len() as f32;
        let x0 = cx + cw / 2.0 - (n - 1.0) * 9.0;
        for i in 0..WIZARD.len() {
            let color = if i == step { p.accent } else { p.ink(0.20) };
            ui.circle(x0 + i as f32 * 18.0, y + FOOTER_H / 2.0, 4.0, color);
        }
        let by = y + (FOOTER_H - 34.0) / 2.0;
        if step > 0 {
            ui.button(cx, by, 130.0, "Précédent", false, Hit::WizardPrev);
        }
        let last = step + 1 == WIZARD.len();
        ui.button(
            cx + cw - 150.0,
            by,
            150.0,
            if last {
                "C'est parti"
            } else if step == 0 {
                "Commencer"
            } else {
                "Suivant"
            },
            true,
            Hit::WizardNext,
        );
        // Les zones cliquables du contenu cachées sous le pied de page ne comptent pas
        ui.hits.retain(|((_, hy, _, _), hit)| {
            *hy < y || matches!(hit, Hit::WizardPrev | Hit::WizardNext)
        });
    }

    fn page_apropos(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        // Emblème : carré arrondi en dégradé d'accent, initiale d'odysseus
        let (ix, iy, isz) = (x + (w - 96.0) / 2.0, y + 8.0, 96.0);
        ui.gradient_square(ix, iy, isz, p.accent, p.accent.mix(p.ink(1.0), 0.35));
        ui.centered(
            "o",
            ix + isz / 2.0,
            iy + 8.0,
            isz - 16.0,
            56.0,
            true,
            p.on_accent,
        );
        ui.centered(
            "odysseus-desktop",
            x + w / 2.0,
            iy + isz + 14.0,
            32.0,
            22.0,
            true,
            p.text,
        );
        ui.centered(
            "Poste de pilotage minimaliste, écrit en Rust",
            x + w / 2.0,
            iy + isz + 46.0,
            20.0,
            13.0,
            false,
            p.text_dim,
        );
        let mut y = iy + isz + 86.0;
        let rows = self.about.len() as f32;
        ui.card(x, y, w, rows * 44.0);
        for (i, (k, v)) in self.about.iter().enumerate() {
            let ry = y + i as f32 * 44.0;
            if i > 0 {
                ui.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            ui.text(k, x + 18.0, ry, 44.0, 13.0, p.text_dim);
            let v = ui.truncate(v, 13.0, w - 170.0);
            ui.text(&v, x + 150.0, ry, 44.0, 13.0, p.text);
        }
        y += rows * 44.0 + 16.0;
        ui.button(x, y, 220.0, "Copier ces informations", false, Hit::CopyInfo);
        ui.button(
            x + 232.0,
            y,
            180.0,
            "Revoir l'accueil",
            false,
            Hit::WizardStart,
        );
        y += 56.0;
        ui.note(
            x,
            y,
            w,
            "MIT — © 2026 David Courtin · github.com/dcourtin/odysseus-desktop. Travail d'exploration, à ne pas utiliser en production.",
        )
    }
}

/// En-tête d'une fiche d'agent : choix, nom, précision, étiquette d'état
#[allow(clippy::too_many_arguments)]
fn agent_header(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    index: usize,
    on: bool,
    name: &str,
    detail: &str,
    pill: &(String, Rgba),
) {
    let p = theme::palette();
    let (cx, cy) = (x + 26.0, y + 26.0);
    if on {
        ui.circle(cx, cy, 8.0, p.accent);
        ui.circle(cx, cy, 3.0, p.on_accent);
    } else {
        ui.circle(cx, cy, 8.0, p.ink(0.25));
        ui.circle(cx, cy, 6.5, p.tile());
    }
    ui.bold_text(name, x + 44.0, y + 8.0, 22.0, 14.0, p.text);
    let nw = ui.measure(name, 14.0);
    ui.text(detail, x + 52.0 + nw, y + 9.0, 22.0, 11.5, p.text_dim);
    if !pill.0.is_empty() {
        let lw = ui.measure(&pill.0, 11.0) + 16.0;
        ui.rounded(
            x + w - 18.0 - lw,
            y + 14.0,
            lw,
            20.0,
            10.0,
            pill.1.with_alpha(0.16),
        );
        ui.centered(
            &pill.0,
            x + w - 18.0 - lw / 2.0,
            y + 14.0,
            20.0,
            11.0,
            false,
            pill.1,
        );
    }
    ui.hit((x, y, w * 0.6, 44.0), Hit::AgentKind(index));
}

/// Barre d'avancement et son texte (avancement inconnu : un segment qui va et vient)
fn progress(ui: &mut Ui, x: f32, y: f32, w: f32, frac: f32, text: &str) {
    let p = theme::palette();
    ui.rounded(x, y, w, 6.0, 3.0, p.ink(0.1));
    if frac >= 0.0 {
        ui.rounded(x, y, (w * frac.min(1.0)).max(6.0), 6.0, 3.0, p.accent);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() % 2000)
            .unwrap_or(0) as f32
            / 2000.0;
        let phase = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
        ui.rounded(x + (w - w * 0.3) * phase, y, w * 0.3, 6.0, 3.0, p.accent);
    }
    let t = ui.truncate(text, 11.5, w);
    ui.text(&t, x, y + 8.0, 18.0, 11.5, p.text_dim);
}

/// Aperçu d'un thème : bureau en miniature (barre, fenêtre, accent)
#[allow(clippy::too_many_arguments)]
fn mockup(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    t: &theme::Palette,
    selected: bool,
    hovered: bool,
) {
    let p = theme::palette();
    if selected {
        ui.rounded(x - 4.0, y - 4.0, w + 8.0, h + 8.0, 16.0, p.accent);
    } else if hovered {
        ui.rounded(x - 3.0, y - 3.0, w + 6.0, h + 6.0, 15.0, p.ink(0.25));
    }
    ui.rounded(x, y, w, h, 12.0, t.background);
    ui.rect(x + 6.0, y + 4.0, w - 12.0, 9.0, t.surface);
    ui.circle(x + 14.0, y + 8.5, 3.0, t.accent);
    // Fenêtre : barre latérale et contenu
    let (wx, wy, ww, wh) = (x + w * 0.14, y + h * 0.24, w * 0.62, h * 0.62);
    ui.rounded(wx, wy, ww, wh, 6.0, t.content);
    ui.rounded(wx, wy, ww * 0.3, wh, 6.0, t.sidebar);
    for k in 0..3 {
        let ly = wy + 8.0 + k as f32 * 9.0;
        ui.rounded(
            wx + ww * 0.36,
            ly,
            ww * 0.5 - k as f32 * 8.0,
            4.0,
            2.0,
            t.ink(0.35),
        );
    }
    ui.rounded(wx + ww * 0.36, wy + wh - 16.0, 30.0, 9.0, 4.5, t.accent);
    // Panneau flottant à droite
    let (px, py) = (x + w * 0.68, y + h * 0.18);
    ui.rounded(px, py, w * 0.26, h * 0.42, 6.0, t.panel);
    ui.circle(px + 9.0, py + 10.0, 4.0, t.accent);
    ui.rounded(px + 16.0, py + 8.0, w * 0.26 - 24.0, 4.0, 2.0, t.ink(0.4));
}

/// Dessin d'une image : échelle, polices, zones cliquables, survol
struct Ui<'a, 'b> {
    c: &'a mut Canvas<'b>,
    s: f32,
    regular: &'a Text,
    bold: &'a Text,
    icons: &'a Text,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
}

#[allow(clippy::too_many_arguments)]
impl Ui<'_, '_> {
    fn hit(&mut self, r: Rect, hit: Hit) {
        self.hits.push((r, hit));
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
    fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let s = self.s;
        self.c
            .shadow(x * s, (y + 4.0) * s, w * s, h * s, 14.0 * s, 18.0 * s, 0.35);
    }
    fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        let s = self.s;
        self.c.fill_circle(cx * s, cy * s, r * s, c);
    }
    fn image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let s = self.s;
        self.c.image(img, x * s, y * s, w * s, h * s, r * s);
    }
    fn icon(&mut self, glyph: char, cx: f32, cy: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.icons.icon(self.c, glyph, cx * s, cy * s, px * s, c);
    }
    fn measure(&self, t: &str, px: f32) -> f32 {
        self.regular.measure(t, px * self.s) / self.s
    }
    fn truncate(&self, t: &str, px: f32, max: f32) -> String {
        self.regular.truncate(t, px * self.s, max * self.s)
    }
    fn text(&mut self, t: &str, x: f32, y: f32, h: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.regular
            .draw_in(self.c, t, x * s, y * s, h * s, px * s, c);
    }
    fn bold_text(&mut self, t: &str, x: f32, y: f32, h: f32, px: f32, c: Rgba) {
        let s = self.s;
        self.bold.draw_in(self.c, t, x * s, y * s, h * s, px * s, c);
    }
    fn centered(&mut self, t: &str, cx: f32, y: f32, h: f32, px: f32, bold: bool, c: Rgba) {
        let s = self.s;
        let font = if bold { self.bold } else { self.regular };
        font.draw_centered(self.c, t, cx * s, y * s, h * s, px * s, c);
    }

    /// Titre de section ; renvoie le haut de son contenu
    fn section(&mut self, x: f32, y: f32, title: &str) -> f32 {
        let p = theme::palette();
        self.bold_text(title, x + 2.0, y, 24.0, 13.0, p.text);
        y + 32.0
    }

    /// Fond d'un groupe de lignes
    fn card(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.rounded(x, y, w, h, 12.0, theme::palette().tile());
    }

    /// Texte explicatif, sur plusieurs lignes ; renvoie le bas
    fn note(&mut self, x: f32, y: f32, w: f32, text: &str) -> f32 {
        let p = theme::palette();
        let lines = self.regular.wrap(text, 12.0 * self.s, w * self.s, 4);
        let mut y = y;
        for line in lines {
            self.text(&line, x + 2.0, y, 18.0, 12.0, p.text_dim);
            y += 18.0;
        }
        y + 8.0
    }

    /// Choix exclusif en segments
    fn segmented(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        labels: &[&str],
        selected: Option<usize>,
        hit: impl Fn(usize) -> Hit,
    ) {
        let p = theme::palette();
        let h = 34.0;
        self.rounded(x, y, w, h, 10.0, p.ink(0.07));
        let n = labels.len().max(1);
        let sw = (w - 6.0) / n as f32;
        for (i, label) in labels.iter().enumerate() {
            let sx = x + 3.0 + i as f32 * sw;
            let on = selected == Some(i);
            let this = hit(i);
            if on {
                self.rounded(sx, y + 3.0, sw, h - 6.0, 8.0, p.accent);
            } else if self.hover == Some(this.clone()) {
                self.rounded(sx, y + 3.0, sw, h - 6.0, 8.0, p.ink(0.08));
            }
            let color = if on { p.on_accent } else { p.text_soft() };
            self.centered(label, sx + sw / 2.0, y, h, 12.5, on, color);
            self.hit((sx, y, sw, h), this);
        }
    }

    /// Liste à choix unique dans une carte ; renvoie le bas
    fn radio_list(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        labels: &[String],
        selected: Option<usize>,
        hit: impl Fn(usize) -> Hit,
    ) -> f32 {
        let p = theme::palette();
        let rh = 42.0;
        self.card(x, y, w, labels.len() as f32 * rh);
        for (i, label) in labels.iter().enumerate() {
            let ry = y + i as f32 * rh;
            let this = hit(i);
            if self.hover == Some(this.clone()) {
                self.rounded(x + 4.0, ry + 3.0, w - 8.0, rh - 6.0, 8.0, p.ink(0.05));
            }
            if i > 0 {
                self.rect(x + 16.0, ry, w - 32.0, 1.0, p.ink(0.06));
            }
            let on = selected == Some(i);
            // Pastille de choix
            let (cx, cy) = (x + 26.0, ry + rh / 2.0);
            if on {
                self.circle(cx, cy, 8.0, p.accent);
                self.circle(cx, cy, 3.0, p.on_accent);
            } else {
                self.circle(cx, cy, 8.0, p.ink(0.25));
                self.circle(cx, cy, 6.5, p.content);
            }
            let color = if on { p.text } else { p.text_soft() };
            self.text(label, x + 44.0, ry, rh, 13.5, color);
            self.hit((x, ry, w, rh), this);
        }
        y + labels.len() as f32 * rh
    }

    fn switch(&mut self, x: f32, y: f32, on: bool, hit: Hit) {
        let p = theme::palette();
        let (w, h) = (40.0, 24.0);
        let bg = if on { p.accent } else { p.ink(0.2) };
        self.rounded(x, y, w, h, h / 2.0, bg);
        let kx = if on { x + w - h / 2.0 } else { x + h / 2.0 };
        self.circle(kx, y + h / 2.0, h / 2.0 - 2.5, Rgba::hex(0xffffff, 1.0));
        self.hit((x - 8.0, y - 8.0, w + 16.0, h + 16.0), hit);
    }

    fn button(&mut self, x: f32, y: f32, w: f32, label: &str, primary: bool, hit: Hit) {
        let p = theme::palette();
        let h = 34.0;
        let hovered = self.hover == Some(hit.clone());
        let (bg, fg) = if primary {
            (
                if hovered { p.accent_hover() } else { p.accent },
                p.on_accent,
            )
        } else {
            (if hovered { p.tile_hover() } else { p.ink(0.08) }, p.text)
        };
        self.rounded(x, y, w, h, 9.0, bg);
        self.centered(label, x + w / 2.0, y, h, 13.0, primary, fg);
        self.hit((x, y, w, h), hit);
    }

    /// « −  21:00  + »
    fn stepper(&mut self, x: f32, y: f32, value: &str, minus: Hit, plus: Hit) {
        let p = theme::palette();
        self.rounded(x, y, 108.0, 32.0, 8.0, p.ink(0.07));
        for (bx, glyph, hit) in [(x, "−", minus), (x + 76.0, "+", plus)] {
            if self.hover == Some(hit.clone()) {
                self.rounded(bx + 2.0, y + 2.0, 28.0, 28.0, 7.0, p.ink(0.1));
            }
            self.centered(glyph, bx + 16.0, y, 32.0, 16.0, true, p.text);
            self.hit((bx, y, 32.0, 32.0), hit);
        }
        self.centered(value, x + 54.0, y, 32.0, 13.5, true, p.text);
    }

    /// Curseur à la couleur d'accent : rempli jusqu'à `frac` ; la zone cliquable
    /// déborde de `SLIDER_PAD` de chaque côté (voir `set_blur_level`)
    fn slider(&mut self, x: f32, y: f32, w: f32, frac: f32, hit: Hit) {
        let p = theme::palette();
        let h = 6.0;
        self.rounded(x, y, w, h, h / 2.0, p.ink(0.12));
        self.rounded(x, y, (w * frac).max(h), h, h / 2.0, p.accent);
        let kx = x + w * frac;
        self.circle(kx, y + h / 2.0, 11.0, Rgba::hex(0x000000, 0.2));
        self.circle(kx, y + h / 2.0, 10.0, Rgba::hex(0xffffff, 1.0));
        self.hit(
            (x - SLIDER_PAD, y - 16.0, w + 2.0 * SLIDER_PAD, h + 32.0),
            hit,
        );
    }

    /// Curseur de teinte : dégradé de l'orangé au blanc
    fn warm_slider(&mut self, x: f32, y: f32, w: f32, frac: f32, hit: Hit) {
        let s = self.s;
        let warm = Rgba::hex(0xff9a3c, 1.0);
        let cool = Rgba::hex(0xfff4e6, 1.0);
        let h = 8.0;
        let steps = (w * s) as i32;
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            self.c
                .fill_rect(x * s + i as f32, y * s, 1.0, h * s, warm.mix(cool, t));
        }
        // Coins arrondis : on repeint les bouts de la couleur du fond de carte
        let kx = x + w * frac;
        self.circle(kx, y + h / 2.0, 11.0, Rgba::hex(0x000000, 0.2));
        self.circle(kx, y + h / 2.0, 10.0, Rgba::hex(0xffffff, 1.0));
        self.circle(kx, y + h / 2.0, 6.0, warm.mix(cool, frac));
        self.hit((x - 10.0, y - 14.0, w + 20.0, h + 28.0), hit);
    }

    /// Petite étiquette (« recommandée »)
    fn pill(&mut self, x: f32, y: f32, label: &str) {
        let p = theme::palette();
        let w = self.measure(label, 11.0) + 16.0;
        self.rounded(x, y, w, 20.0, 10.0, p.accent.with_alpha(0.18));
        self.centered(label, x + w / 2.0, y, 20.0, 11.0, false, p.accent);
    }

    /// Cadre en tirets (zone de dépôt)
    fn dashed(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let (dash, gap) = (10.0f32, 7.0f32);
        let mut t = x + r;
        while t < x + w - r {
            let len = dash.min(x + w - r - t);
            self.rect(t, y, len, 1.5, c);
            self.rect(t, y + h - 1.5, len, 1.5, c);
            t += dash + gap;
        }
        let mut t = y + r;
        while t < y + h - r {
            let len = dash.min(y + h - r - t);
            self.rect(x, t, 1.5, len, c);
            self.rect(x + w - 1.5, t, 1.5, len, c);
            t += dash + gap;
        }
    }

    /// Touches dessinées comme des touches de clavier, alignées à droite sur `right`
    fn keycaps(&mut self, right: f32, y: f32, keys: &[&str]) {
        let p = theme::palette();
        let s = self.s;
        let widths: Vec<f32> = keys
            .iter()
            .map(|k| self.bold.measure(k, 12.0 * s) / s + 14.0)
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + 6.0 * keys.len().saturating_sub(1) as f32;
        let mut kx = right - total;
        for (k, kw) in keys.iter().zip(widths) {
            self.rounded(kx, y, kw, 22.0, 6.0, p.tile_hover());
            self.rounded(kx, y + 20.0, kw, 2.0, 1.0, Rgba::hex(0x000000, 0.25));
            self.centered(k, kx + kw / 2.0, y, 22.0, 12.0, true, p.text);
            kx += kw + 6.0;
        }
    }

    /// Carré arrondi en dégradé vertical (emblème)
    fn gradient_square(&mut self, x: f32, y: f32, size: f32, top: Rgba, bottom: Rgba) {
        let s = self.s;
        let rows = (size * s) as i32;
        for i in 0..rows {
            let t = i as f32 / rows as f32;
            let c = top.mix(bottom, t);
            // Largeur de la ligne selon les coins arrondis (rayon 24)
            let r = 24.0 * s;
            let fy = i as f32 + 0.5;
            let inset = if fy < r {
                r - (r * r - (r - fy) * (r - fy)).max(0.0).sqrt()
            } else if fy > rows as f32 - r {
                let d = fy - (rows as f32 - r);
                r - (r * r - d * d).max(0.0).sqrt()
            } else {
                0.0
            };
            self.c.fill_rect(
                x * s + inset,
                y * s + i as f32,
                size * s - 2.0 * inset,
                1.0,
                c,
            );
        }
    }
}

// --- Fenêtre, clavier, souris, glisser-déposer ---------------------------------

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
        self.next_frame();
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

impl WindowHandler for App {
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
        self.seat = Some(seat.clone());
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
                // Barre de défilement : saisir le curseur, ou cliquer sur le rail pour y sauter
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } if self.on_scrollbar(x, y) => {
                    if let Some((_, _, _, thumb_y, thumb_h)) =
                        self.scrollbar_geometry(self.content_h)
                    {
                        let grab = if y >= thumb_y && y <= thumb_y + thumb_h {
                            y - thumb_y
                        } else {
                            thumb_h / 2.0
                        };
                        self.scrollbar_drag = Some(grab);
                        self.scroll_to_thumb(y - grab);
                    }
                }
                PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } => self.click(x, y, serial),
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } if self.scrollbar_drag.is_some() => {
                    self.scrollbar_drag = None;
                    self.scrollbar_hover = self.on_scrollbar(x, y);
                    self.draw();
                }
                PointerEventKind::Release {
                    button: BTN_LEFT, ..
                } if self.slider_drag.is_some() => self.slider_drag = None,
                PointerEventKind::Motion { .. } if self.slider_drag.is_some() => {
                    if let Some(rect) = self.slider_drag {
                        self.set_blur_level(x, rect);
                    }
                }
                PointerEventKind::Motion { .. } if self.scrollbar_drag.is_some() => {
                    let grab = self.scrollbar_drag.unwrap_or(0.0);
                    self.scroll_to_thumb(y - grab);
                }
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let on_bar = self.on_scrollbar(x, y);
                    if on_bar != self.scrollbar_hover {
                        self.scrollbar_hover = on_bar;
                        self.draw();
                    }
                    let hover = self.hit_at(x, y).map(|(_, h)| h);
                    if self.chrome.motion(x, y, CHROME_Y) {
                        self.draw();
                    }
                    if hover != self.hover {
                        self.hover = hover;
                        self.draw();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    let bar = std::mem::take(&mut self.scrollbar_hover);
                    let chrome = self.chrome.leave();
                    if self.hover.take().is_some() || chrome || bar {
                        self.draw();
                    }
                }
                PointerEventKind::Axis { vertical, .. } if vertical.absolute != 0.0 => {
                    self.scroll_by(vertical.absolute as f32 * 1.5);
                    self.draw();
                }
                _ => {}
            }
        }
    }
}

fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

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
        // Page des fonds : une liste d'URI (image glissée depuis Fichiers) est acceptée
        let Some(offer) = drag_offer(device) else {
            return;
        };
        let uri = offer.with_mime_types(|m| m.iter().any(|t| t == "text/uri-list"));
        if self.page == Page::Fond && uri {
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
            if self.page == Page::Fond
                && let Ok(pipe) = offer.receive("text/uri-list".into())
            {
                // La demande doit partir avant que l'autre application n'écrive
                let _ = conn.flush();
                read_drop(pipe, self.msgs.clone());
            }
            offer.finish();
            offer.destroy();
        }
        self.draw();
    }
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
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_data_device!(App);
delegate_registry!(App);

/// Accents du thème personnalisé : teintes de la Méditerranée
const CUSTOM_ACCENTS: &[(u32, &str)] = &[
    (0xd97757, "Terre cuite"),
    (0xe3a24f, "Ocre"),
    (0xc8a26b, "Sable"),
    (0x8fa35a, "Olivier"),
    (0x4eb39a, "Lagune"),
    (0x5b9bd5, "Mer Égée"),
    (0x8087e8, "Lavande"),
    (0xb07cd9, "Pourpre"),
    (0xd9779c, "Laurier-rose"),
    (0xc4473a, "Rouge pompéien"),
];

/// Fonds du thème personnalisé, selon la base
fn custom_backgrounds(dark: bool) -> &'static [(u32, &'static str)] {
    if dark {
        &[
            (0x262624, "Graphite"),
            (0x1e1e2e, "Nuit"),
            (0x1f2528, "Ardoise"),
            (0x2a221e, "Brun de Sienne"),
            (0x1e2420, "Maquis"),
            (0x141414, "Encre"),
        ]
    } else {
        &[
            (0xfaf9f5, "Ivoire"),
            (0xf3eee3, "Lin"),
            (0xeef1f4, "Brume"),
            (0xf6efe6, "Poudre"),
            (0xedf1ea, "Sauge"),
            (0xffffff, "Blanc"),
        ]
    }
}

/// « Mer Égée » → « mer Égée » : seule l'initiale passe en minuscule
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}
