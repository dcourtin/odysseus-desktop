//! Assistant d'odysseus-desktop : panneau ouvert par Super+A (ou « ? » dans le
//! lanceur), qui pose les questions à l'agent choisi dans Réglages > Agent
//! (Claude Code, Ollama, commande). La réponse s'écrit au fil de l'eau ; les
//! outils du bureau qu'il utilise (odysseus-mcp) s'affichent au passage.
//! On y glisse des fichiers depuis Fichiers (pièces jointes).
//!
//! `odysseus-assistant [--joindre FICHIER]… [QUESTION]`
//! Clavier : Entrée envoie, Échap arrête la réponse puis ferme, ↑ reprend la
//! dernière question, Ctrl+V colle, Ctrl+L nouvelle conversation.

mod backend;
mod mcp;

use std::{fs::File, io::Read, path::PathBuf, time::Duration};

use backend::{Context, Event, Job};
use common::{
    agent::AgentConfig,
    draw::{Canvas, Text},
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

const SUGGESTIONS: [&str; 4] = [
    "Qu'est-ce qui chauffe en ce moment ?",
    "Quelles fenêtres sont ouvertes ?",
    "Résume mes notifications",
    "Pourquoi la machine rame ?",
];

/// Élément de la conversation
enum Item {
    User { text: String, files: Vec<String> },
    Answer(String),
    Tool { label: String, ok: Option<bool> },
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    Suggestion(usize),
    Send,
    Stop,
    New,
    Close,
    RemoveFile(usize),
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
    events: channel::Sender<Event>,
    drops: channel::Sender<Vec<PathBuf>>,
    agent: AgentConfig,
    items: Vec<Item>,
    input: String,
    files: Vec<PathBuf>,
    last_question: String,
    job: Option<Job>,
    context: Context,
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
    // Arguments : pièces jointes et question
    let mut files = Vec::new();
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--joindre" => {
                if let Some(f) = args.next() {
                    files.push(PathBuf::from(f));
                }
            }
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
    let (events, rx) = channel::channel::<Event>();
    event_loop
        .handle()
        .insert_source(rx, |event, _, app| {
            if let ChannelEvent::Msg(e) = event {
                app.on_event(e);
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
                let tool_running = app
                    .items
                    .iter()
                    .any(|i| matches!(i, Item::Tool { ok: None, .. }));
                if app.job.is_some() && tool_running {
                    app.draw();
                }
                TimeoutAction::ToDuration(Duration::from_millis(300))
            },
        )
        .expect("minuterie");

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
        last_question: String::new(),
        job: None,
        context: Context::default(),
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
    if let Some(job) = app.job.take() {
        job.stop();
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, std::sync::atomic::Ordering::Relaxed);
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

impl App {
    fn send(&mut self) {
        if self.job.is_some() {
            return;
        }
        let question = self.input.trim().to_string();
        if question.is_empty() && self.files.is_empty() {
            return;
        }
        // L'agent choisi peut avoir changé (Réglages, agent.json)
        let agent = AgentConfig::load();
        if agent.agent != self.agent.agent {
            self.context = Context::default();
        }
        self.agent = agent;
        let files: Vec<PathBuf> = std::mem::take(&mut self.files);
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
        });
        self.last_question = question.clone();
        self.input.clear();
        self.scroll = 0.0;
        self.job = Some(backend::ask(
            self.agent.clone(),
            question,
            files,
            self.context.clone(),
            self.events.clone(),
        ));
        self.draw();
    }

    fn stop(&mut self) {
        if let Some(job) = self.job.as_ref() {
            job.stop();
        }
    }

    fn new_conversation(&mut self) {
        self.stop();
        self.job = None;
        self.items.clear();
        self.context = Context::default();
        self.scroll = 0.0;
        self.draw();
    }

    fn on_event(&mut self, e: Event) {
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
            Event::Tool(label) => self.items.push(Item::Tool { label, ok: None }),
            Event::ToolDone(ok) => {
                if let Some(Item::Tool { ok: state, .. }) = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|i| matches!(i, Item::Tool { ok: None, .. }))
                {
                    *state = Some(ok);
                }
            }
            Event::Session(id) => self.context.claude_session = Some(id),
            Event::History(h) => self.context.ollama_history = h,
            Event::Done(result) => {
                self.job = None;
                if let Err(e) = result {
                    self.items.push(Item::Error(e));
                }
                // Réponse terminée : les blancs de fin ne comptent pas
                if let Some(Item::Answer(text)) = self.items.last_mut() {
                    let trimmed = text.trim_end().len();
                    text.truncate(trimmed);
                }
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
            None => {}
        }
    }

    fn key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        match event.keysym {
            Keysym::Escape if self.job.is_some() => self.stop(),
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.send(),
            Keysym::l if ctrl => self.new_conversation(),
            Keysym::v if ctrl => {
                // Texte du presse-papiers, sur une ligne
                if let Ok(out) = std::process::Command::new("wl-paste")
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

        // Conversation, sous l'en-tête et au-dessus de la saisie
        let top = y0 + HEADER_H;
        let bottom = y0 + PH - INPUT_H - if self.files.is_empty() { 0.0 } else { 30.0 };
        let (cx, cw) = (x0 + PAD, PW - 2.0 * PAD);
        if self.items.is_empty() {
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
        self.input_area(&mut ui, x0, y0);

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
        // À droite : nouvelle conversation, fermer
        let bx = x0 + PW - 44.0;
        ui.round_button(bx, y0 + 28.0, icon::CLOSE, Hit::Close, false);
        if !self.items.is_empty() {
            ui.round_button(bx - 36.0, y0 + 28.0, icon::ROTATE_CCW, Hit::New, false);
        }
        ui.rect(x0 + 1.0, y0 + HEADER_H - 1.0, PW - 2.0, 1.0, p.ink(0.07));
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
    }

    fn input_area(&self, ui: &mut Ui, x0: f32, y0: f32) {
        let p = theme::palette();
        let mut y = y0 + PH - INPUT_H + 12.0;
        // Pièces jointes
        if !self.files.is_empty() {
            y -= 30.0;
            let mut fx = x0 + PAD;
            for (i, f) in self.files.iter().enumerate() {
                let name = f
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = ui.truncate(&name, 12.0, 200.0);
                let fw = ui.measure(&name, 12.0) + 52.0;
                ui.rounded(fx, y, fw, 24.0, 12.0, p.accent.with_alpha(0.16));
                ui.icon(icon::LINK, fx + 14.0, y + 12.0, 11.0, p.accent);
                ui.text(fx + 24.0, y, 24.0, 12.0, &name, p.text);
                ui.icon(icon::CLOSE, fx + fw - 13.0, y + 12.0, 10.0, p.text_dim);
                ui.hit((fx + fw - 24.0, y, 24.0, 24.0), Hit::RemoveFile(i));
                fx += fw + 8.0;
            }
            y += 30.0;
        }
        let (fx, fw, fh) = (x0 + PAD, PW - 2.0 * PAD, 46.0);
        ui.rounded(fx, y, fw, fh, 14.0, p.ink(0.07));
        ui.stroke(fx, y, fw, fh, 14.0, p.accent.with_alpha(0.45));
        let tw = fw - 70.0;
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
                Item::User { text, files } => {
                    if k > 0 {
                        lines.push(Line::gap(14.0));
                    }
                    let bw = w * 0.72;
                    let mut wrapped = Vec::new();
                    for para in text.split('\n') {
                        wrapped.extend(ui.wrap(para, 14.0, bw - 28.0));
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
                }
                Item::Tool { label, ok } => lines.push(Line {
                    kind: LineKind::Tool(label.clone(), *ok),
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
            LineKind::Tool(label, ok) => {
                let (glyph, color, suffix) = match ok {
                    None => {
                        // Trois points qui s'allument tour à tour
                        let t = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() / 300 % 4)
                            .unwrap_or(0);
                        (icon::LOADER, p.accent, ".".repeat(t as usize))
                    }
                    Some(true) => (icon::CHECK, p.success, String::new()),
                    Some(false) => (icon::CLOSE, p.danger, " (refusé)".to_string()),
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
        }
    }
}

enum LineKind {
    Gap,
    Bubble(Vec<String>, f32),
    Text(String),
    Tool(String, Option<bool>),
    Error(String),
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
                    let max = (self.content_h - (PH - HEADER_H - INPUT_H)).max(0.0);
                    self.scroll = (self.scroll - vertical.absolute as f32 * 1.5).clamp(0.0, max);
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
