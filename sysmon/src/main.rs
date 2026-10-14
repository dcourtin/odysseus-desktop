//! Moniteur système d'odysseus-desktop : panneau ouvert depuis la barre (icône
//! jauge). Deux colonnes de tuiles : processeur (charge, courbe sur 60 s,
//! cœurs, fréquence, température), mémoire, disques ; carte graphique
//! (jauges de charge, mémoire vidéo et température, consommation,
//! ventilateur), réseau (débits et courbes), processus les plus actifs (un
//! clic sur × arme l'arrêt, un second l'envoie).
//! Mesures une fois par seconde dans un fil ; les valeurs glissent de l'une à
//! l'autre. Se ferme avec Échap ou dès qu'on clique ailleurs.

use common::stats;

use std::{
    f32::consts::TAU,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            channel::{self, Event as ChannelEvent},
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
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use stats::{HISTORY, Snapshot};

// Mise en page, en pixels logiques
const MARGIN: f32 = 16.0;
const PAD: f32 = 18.0;
const COL_W: f32 = 364.0;
const GAP: f32 = 12.0;
const HEADER_H: f32 = 52.0;
const CPU_H: f32 = 196.0;
const MEM_H: f32 = 132.0;
const GPU_H: f32 = 196.0;
const NET_H: f32 = 148.0;
const PROC_ROW: f32 = 34.0;
const PROC_ROWS: usize = 6;
const BTN_LEFT: u32 = 0x110;
/// Glissement des valeurs d'une mesure à la suivante
const GLIDE: Duration = Duration::from_millis(450);
/// Intervalle entre deux images pendant un glissement
const FRAME: Duration = Duration::from_millis(33);
/// Arrêt d'un processus armé : le second clic doit suivre dans ce délai
const ARM_TIME: Duration = Duration::from_secs(3);

fn panel_w() -> f32 {
    2.0 * PAD + 2.0 * COL_W + GAP
}

fn disks_h(n: usize) -> f32 {
    46.0 + n.max(1) as f32 * 50.0 + 26.0
}

fn proc_h() -> f32 {
    46.0 + PROC_ROWS as f32 * PROC_ROW + 8.0
}

fn right_column_h() -> f32 {
    GPU_H + GAP + NET_H + GAP + proc_h()
}

fn panel_h(disks: usize) -> f32 {
    let left = CPU_H + GAP + MEM_H + GAP + disks_h(disks);
    2.0 * PAD + HEADER_H + left.max(right_column_h())
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-moniteur.pid"))
}

/// Déjà ouvert : on le ferme (bascule) et on renvoie vrai.
fn toggle_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return false;
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let alive = stat.contains("(odysseus-monit")
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

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Kill(i32),
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    qh: QueueHandle<App>,
    loop_handle: LoopHandle<'static, App>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
    icons: Text,
    /// Mesure précédente et courante ; les valeurs glissent de l'une à l'autre
    prev: Snapshot,
    cur: Snapshot,
    arrived: Instant,
    hits: Vec<((f32, f32, f32, f32), Hit)>,
    hover: Option<Hit>,
    armed: Option<(i32, Instant)>,
    message: Option<String>,
    /// Fond (ombre, panneau) déjà dessiné pour cette taille de tampon
    background: Option<(String, Vec<u8>)>,
    frame_pending: bool,
    dirty: bool,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
}

fn main() {
    if toggle_existing() {
        return;
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    // Arrêt demandé (la barre bascule le panneau) : on sort proprement
    unsafe { libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t) };

    let mut sampler = stats::Sampler::new();
    let first = sampler.sample();
    let (tx, rx) = channel::channel::<Snapshot>();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if tx.send(sampler.sample()).is_err() {
                break;
            }
        }
    });

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<App> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let width = (panel_w() + 2.0 * MARGIN) as u32;
    let height = (panel_h(first.disks.len()) + 2.0 * MARGIN) as u32;
    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("odysseus-moniteur"), None);
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_size(width, height);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.commit();

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");
    event_loop
        .handle()
        .insert_source(rx, |event, _, app| {
            if let ChannelEvent::Msg(snapshot) = event {
                app.prev = std::mem::replace(&mut app.cur, snapshot);
                app.arrived = Instant::now();
                app.draw();
                // Glissement vers la nouvelle mesure, à 30 images par seconde
                let _ = app
                    .loop_handle
                    .insert_source(Timer::from_duration(FRAME), |_, _, app| {
                        app.draw();
                        if app.arrived.elapsed() < GLIDE {
                            TimeoutAction::ToDuration(FRAME)
                        } else {
                            TimeoutAction::Drop
                        }
                    });
            }
        })
        .expect("canal des mesures");

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(4 * 1024 * 1024, &shm).expect("pool shm"),
        shm,
        layer,
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        prev: first.clone(),
        cur: first,
        arrived: Instant::now() - GLIDE,
        hits: Vec::new(),
        hover: None,
        armed: None,
        message: None,
        background: None,
        frame_pending: false,
        dirty: false,
        width,
        height,
        scale: 1,
        configured: false,
        exit: false,
    };
    while !app.exit && !TERM.load(std::sync::atomic::Ordering::Relaxed) {
        if event_loop
            .dispatch(Some(Duration::from_millis(250)), &mut app)
            .is_err()
        {
            break;
        }
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Courbe d'atténuation : départ rapide, arrivée douce
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Octets lisibles : « 7,4 Go », « 512 Mo »
fn bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let mut v = n;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let text = if u == 0 || v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    };
    format!("{} {}", text.replace('.', ","), UNITS[u])
}

fn rate(n: f64) -> String {
    format!("{}/s", bytes(n))
}

fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    match (d, h) {
        (0, 0) => format!("allumé depuis {m} min"),
        (0, _) => format!("allumé depuis {h} h {m:02}"),
        _ => format!("allumé depuis {d} j {h} h"),
    }
}

/// Couleur d'une charge : accent, puis orangé, puis rouge
fn load_color(v: f32) -> Rgba {
    let p = theme::palette();
    if v >= 0.9 {
        p.danger
    } else if v >= 0.7 {
        p.accent.mix(p.danger, 0.5)
    } else {
        p.accent
    }
}

impl App {
    /// Demande un dessin : au plus un par rafraîchissement d'écran
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
        // Fond : ombre, panneau, tuiles et titres, dessinés une fois (refaits si
        // la taille, le thème ou un titre change)
        let key = format!(
            "{w}x{h} {} {} {} {} {} {}",
            theme::palette().id,
            self.cur.cpu.model,
            self.cur.memory.total,
            self.cur.gpu.as_ref().map(|g| g.name.as_str()).unwrap_or(""),
            self.cur.net.interface,
            self.cur.disks.len()
        );
        match &self.background {
            Some((k, px)) if *k == key => c.buf.copy_from_slice(px),
            _ => {
                draw_static(
                    &mut c,
                    s,
                    self.height,
                    (&self.regular, &self.bold, &self.icons),
                    &self.cur,
                );
                self.background = Some((key, c.buf.to_vec()));
            }
        }

        let t = ease(self.arrived.elapsed().as_secs_f32() / GLIDE.as_secs_f32());
        let mut d = Drawer {
            c: &mut c,
            s,
            t,
            fixed: false,
            regular: &self.regular,
            bold: &self.bold,
            icons: &self.icons,
            hits: Vec::new(),
            hover: self.hover,
            armed: self
                .armed
                .filter(|(_, at)| at.elapsed() < ARM_TIME)
                .map(|(p, _)| p),
        };
        // En-tête : durée depuis l'allumage, ou message d'erreur
        let p = theme::palette();
        let (x0, y0) = (MARGIN + PAD, MARGIN + PAD);
        let right = x0 + 2.0 * COL_W + GAP;
        let note = self
            .message
            .clone()
            .unwrap_or_else(|| uptime(self.cur.uptime));
        let color = if self.message.is_some() {
            p.danger
        } else {
            p.text_dim
        };
        d.text_right(false, &note, right, y0, 32.0, 12.0, color);
        sections(&mut d, &self.prev, &self.cur);

        self.hits = d.hits;
        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(&self.qh, surface.clone());
        self.frame_pending = true;
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits
            .iter()
            .find(|((hx, hy, hw, hh), _)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh)
            .map(|(_, h)| *h)
    }

    fn click(&mut self, x: f32, y: f32) {
        let Some(Hit::Kill(pid)) = self.hit_at(x, y) else {
            return;
        };
        let armed = self
            .armed
            .is_some_and(|(p, at)| p == pid && at.elapsed() < ARM_TIME);
        if !armed {
            self.armed = Some((pid, Instant::now()));
            self.message = None;
            return self.draw();
        }
        self.armed = None;
        let name = self
            .cur
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.message = if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
            None
        } else {
            Some(format!(
                "Impossible d'arrêter « {name} » : {}",
                std::io::Error::last_os_error()
            ))
        };
        self.draw();
    }
}

/// Partie fixe de l'image : ombre, panneau, en-tête, tuiles et titres
fn draw_static(
    c: &mut Canvas,
    s: f32,
    height: u32,
    (regular, bold, icons): (&Text, &Text, &Text),
    cur: &Snapshot,
) {
    let p = theme::palette();
    let (pw, ph) = (panel_w(), height as f32 - 2.0 * MARGIN);
    c.clear();
    let r = theme::PANEL_RADIUS * s;
    c.shadow(
        MARGIN * s,
        (MARGIN + 4.0) * s,
        pw * s,
        ph * s,
        r,
        24.0 * s,
        0.45,
    );
    c.fill_rounded(MARGIN * s, MARGIN * s, pw * s, ph * s, r, p.panel);
    c.stroke_rounded(
        MARGIN * s,
        MARGIN * s,
        pw * s,
        ph * s,
        r,
        s.max(1.0),
        p.panel_border(),
    );
    let (x0, y0) = (MARGIN + PAD, MARGIN + PAD);
    icons.icon(
        c,
        icon::ACTIVITY,
        (x0 + 10.0) * s,
        (y0 + 16.0) * s,
        18.0 * s,
        p.accent,
    );
    bold.draw_in(
        c,
        "Moniteur",
        (x0 + 30.0) * s,
        y0 * s,
        32.0 * s,
        17.0 * s,
        p.text,
    );
    let mut d = Drawer {
        c,
        s,
        t: 1.0,
        fixed: true,
        regular,
        bold,
        icons,
        hits: Vec::new(),
        hover: None,
        armed: None,
    };
    sections(&mut d, cur, cur);
}

/// Les six tuiles, sur deux colonnes
fn sections(d: &mut Drawer, prev: &Snapshot, cur: &Snapshot) {
    let (x0, top) = (MARGIN + PAD, MARGIN + PAD + HEADER_H);
    let mut y = top;
    d.cpu(x0, y, prev, cur);
    y += CPU_H + GAP;
    d.memory(x0, y, prev, cur);
    y += MEM_H + GAP;
    let bottom = top + right_column_h();
    d.disks(x0, y, (bottom - y).max(disks_h(cur.disks.len())), cur);
    let x1 = x0 + COL_W + GAP;
    let mut y = top;
    d.gpu(x1, y, prev, cur);
    y += GPU_H + GAP;
    d.network(x1, y, prev, cur);
    y += NET_H + GAP;
    d.processes(x1, y, cur);
}

/// Dessin d'une image : canevas, échelle, avancement du glissement, polices.
/// `fixed` : passe de la partie fixe (tuiles, titres, repères) seulement.
struct Drawer<'a, 'b> {
    c: &'a mut Canvas<'b>,
    s: f32,
    t: f32,
    fixed: bool,
    regular: &'a Text,
    bold: &'a Text,
    icons: &'a Text,
    hits: Vec<((f32, f32, f32, f32), Hit)>,
    hover: Option<Hit>,
    armed: Option<i32>,
}

impl Drawer<'_, '_> {
    fn tile(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if !self.fixed {
            return;
        }
        let s = self.s;
        self.c.fill_rounded(
            x * s,
            y * s,
            w * s,
            h * s,
            theme::TILE_RADIUS * s,
            theme::palette().tile(),
        );
    }

    /// Titre de tuile : icône, libellé en petites capitales, précision à droite
    fn title(&mut self, x: f32, y: f32, glyph: char, label: &str, detail: &str) {
        if !self.fixed {
            return;
        }
        let s = self.s;
        let p = theme::palette();
        self.icons.icon(
            self.c,
            glyph,
            (x + 22.0) * s,
            (y + 22.0) * s,
            14.0 * s,
            p.text_dim,
        );
        self.bold.draw_in(
            self.c,
            &label.to_uppercase(),
            (x + 36.0) * s,
            (y + 10.0) * s,
            24.0 * s,
            11.0 * s,
            p.text_dim,
        );
        if !detail.is_empty() {
            let max = COL_W - 150.0;
            let detail = self.regular.truncate(detail, 12.0 * s, max * s);
            let dw = self.regular.measure(&detail, 12.0 * s) / s;
            self.regular.draw_in(
                self.c,
                &detail,
                (x + COL_W - 14.0 - dw) * s,
                (y + 10.0) * s,
                24.0 * s,
                12.0 * s,
                p.text_dim,
            );
        }
    }

    /// Texte aligné à droite sur `right`
    #[allow(clippy::too_many_arguments)]
    fn text_right(
        &mut self,
        font_bold: bool,
        text: &str,
        right: f32,
        y: f32,
        h: f32,
        px: f32,
        c: Rgba,
    ) {
        let s = self.s;
        let font = if font_bold { self.bold } else { self.regular };
        let w = font.measure(text, px * s) / s;
        font.draw_in(self.c, text, (right - w) * s, y * s, h * s, px * s, c);
    }

    /// Pastille d'information : icône et valeur
    fn chip(&mut self, x: f32, y: f32, glyph: char, text: &str) -> f32 {
        let s = self.s;
        let p = theme::palette();
        let w = self.regular.measure(text, 12.0 * s) / s + 36.0;
        self.c
            .fill_rounded(x * s, y * s, w * s, 24.0 * s, 12.0 * s, p.ink(0.06));
        self.icons.icon(
            self.c,
            glyph,
            (x + 14.0) * s,
            (y + 12.0) * s,
            12.0 * s,
            p.text_dim,
        );
        self.regular.draw_in(
            self.c,
            text,
            (x + 26.0) * s,
            y * s,
            24.0 * s,
            12.0 * s,
            p.text,
        );
        w
    }

    /// Courbe pleine sur 60 s, qui glisse vers la gauche à chaque mesure.
    /// `values` : de la plus ancienne à la plus récente, entre 0 et 1.
    #[allow(clippy::too_many_arguments)]
    fn area(&mut self, x: f32, y: f32, w: f32, h: f32, values: &[f32], color: Rgba, fill: bool) {
        let s = self.s;
        let (bx, by, bw, bh) = (x * s, y * s, w * s, h * s);
        if values.len() < 2 {
            return;
        }
        let step = bw / (HISTORY - 1) as f32;
        let n = values.len();
        // Le point le plus récent arrive de la droite pendant le glissement
        let shift = (1.0 - self.t) * step;
        let x_of = |i: usize| bx + bw - (n - 1 - i) as f32 * step + shift;
        let mut prev_top: Option<f32> = None;
        for px in (bx.floor() as i32)..((bx + bw).ceil() as i32) {
            let fx = px as f32 + 0.5;
            // Segment qui contient cette colonne
            let k = (0..n - 1).find(|&i| fx >= x_of(i) && fx <= x_of(i + 1));
            let Some(k) = k else {
                prev_top = None;
                continue;
            };
            let (x_a, x_b) = (x_of(k), x_of(k + 1));
            let u = ((fx - x_a) / (x_b - x_a)).clamp(0.0, 1.0);
            let v = lerp(values[k], values[k + 1], u).clamp(0.0, 1.0);
            let top = by + bh - v * (bh - s);
            if fill {
                // Dégradé : plus dense près de la courbe
                let mut py = top.ceil();
                while py < by + bh {
                    let fade = 1.0 - (py - top) / bh;
                    self.c.blend(px, py as i32, color, 0.28 * fade);
                    py += 1.0;
                }
            }
            // Trait de la courbe, épais de 2 px logiques, relié à la colonne précédente
            let (a, b) = match prev_top {
                Some(pt) => (pt.min(top), pt.max(top)),
                None => (top, top),
            };
            let mut py = (a - s).floor();
            while py <= b + s {
                let cov = if py < a - s || py > b + s {
                    0.0
                } else {
                    1.0f32
                        .min(1.0 - ((a - s) - py).max(0.0))
                        .min(1.0 - (py - (b + s)).max(0.0))
                };
                self.c.blend(px, py as i32, color, cov.clamp(0.0, 1.0));
                py += 1.0;
            }
            prev_top = Some(top);
        }
    }

    /// Lignes de repère discrètes (25, 50, 75 %)
    fn grid(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if !self.fixed {
            return;
        }
        let s = self.s;
        for k in 1..4 {
            let gy = y + h * k as f32 / 4.0;
            self.c
                .fill_rect(x * s, gy * s, w * s, s.max(1.0), theme::palette().ink(0.05));
        }
    }

    /// Jauge circulaire : arc de 0 à `frac`, extrémités arrondies
    #[allow(clippy::too_many_arguments)]
    fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, frac: f32, color: Rgba) {
        let s = self.s;
        let p = theme::palette();
        let (cx, cy, r_out) = (cx * s, cy * s, r * s);
        let r_in = r_out - width * s;
        let frac = frac.clamp(0.0, 1.0);
        let end = frac * TAU;
        for py in (cy - r_out).floor() as i32..(cy + r_out).ceil() as i32 {
            for px in (cx - r_out).floor() as i32..(cx + r_out).ceil() as i32 {
                let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (d - r_in + 0.5).min(r_out - d + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                // Angle depuis le haut, dans le sens des aiguilles d'une montre
                let mut a = dx.atan2(-dy);
                if a < 0.0 {
                    a += TAU;
                }
                let c = if a <= end { color } else { p.ink(0.08) };
                self.c.blend(px, py, c, cov);
            }
        }
        // Extrémités arrondies de l'arc
        if frac > 0.005 {
            let mid = (r_out + r_in) / 2.0;
            let cap = width * s / 2.0;
            self.c.fill_circle(cx, cy - mid, cap, color);
            self.c
                .fill_circle(cx + mid * end.sin(), cy - mid * end.cos(), cap, color);
        }
    }

    fn cpu(&mut self, x: f32, y: f32, prev: &Snapshot, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, CPU_H);
        self.title(x, y, icon::CPU, "Processeur", &cur.cpu.model);
        let (gx, gy, gw, gh) = (x + 14.0, y + 86.0, COL_W - 28.0, 58.0);
        self.grid(gx, gy, gw, gh);
        if self.fixed {
            return;
        }
        let load = lerp(prev.cpu.load, cur.cpu.load, self.t);
        let value = format!("{:.0} %", load * 100.0);
        self.bold.draw_in(
            self.c,
            &value,
            (x + 16.0) * s,
            (y + 36.0) * s,
            40.0 * s,
            30.0 * s,
            load_color(load),
        );
        // Fréquence et température, en pastilles à droite
        let mut cx = x + COL_W - 14.0;
        if let Some(temp) = cur.cpu.temp {
            let text = format!("{temp:.0} °C");
            let w = self.regular.measure(&text, 12.0 * s) / s + 36.0;
            cx -= w;
            self.chip(cx, y + 44.0, icon::THERMOMETER, &text);
            cx -= 8.0;
        }
        if let Some(ghz) = cur.cpu.ghz {
            let text = format!("{:.2} GHz", ghz).replace('.', ",");
            let w = self.regular.measure(&text, 12.0 * s) / s + 36.0;
            cx -= w;
            self.chip(cx, y + 44.0, icon::ZAP, &text);
        }
        // Courbe des 60 dernières secondes
        let history: Vec<f32> = cur.cpu.history.iter().copied().collect();
        self.area(gx, gy, gw, gh, &history, p.accent, true);
        // Un trait par cœur
        let n = cur.cpu.cores.len().max(1);
        let (bx, by, bh) = (x + 14.0, y + 156.0, 26.0);
        let bw = (COL_W - 28.0 - (n - 1) as f32 * 4.0) / n as f32;
        for (i, core) in cur.cpu.cores.iter().enumerate() {
            let v = lerp(
                prev.cpu.cores.get(i).copied().unwrap_or(*core),
                *core,
                self.t,
            );
            let cx = bx + i as f32 * (bw + 4.0);
            self.c
                .fill_rounded(cx * s, by * s, bw * s, bh * s, 3.0 * s, p.ink(0.06));
            let fh = (bh * v).max(2.0);
            self.c.fill_rounded(
                cx * s,
                (by + bh - fh) * s,
                bw * s,
                fh * s,
                3.0 * s,
                load_color(v).with_alpha(0.85),
            );
        }
    }

    fn memory(&mut self, x: f32, y: f32, prev: &Snapshot, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, MEM_H);
        let m = &cur.memory;
        let total = bytes(m.total as f64);
        self.title(x, y, icon::MEMORY, "Mémoire", &format!("{total} au total"));
        if self.fixed {
            return;
        }
        let used = lerp(prev.memory.used as f32, m.used as f32, self.t);
        let cache = lerp(prev.memory.cache as f32, m.cache as f32, self.t);
        let frac = used / m.total.max(1) as f32;
        self.bold.draw_in(
            self.c,
            &bytes(used as f64),
            (x + 16.0) * s,
            (y + 36.0) * s,
            36.0 * s,
            24.0 * s,
            load_color(frac),
        );
        let free = m.total.saturating_sub(m.used) as f64;
        self.text_right(
            false,
            &format!("{} disponibles", bytes(free)),
            x + COL_W - 16.0,
            y + 40.0,
            30.0,
            12.5,
            p.text_dim,
        );
        // Barre empilée : utilisée, cache, libre
        let (bx, by, bw, bh) = (x + 16.0, y + 82.0, COL_W - 32.0, 10.0);
        self.c
            .fill_rounded(bx * s, by * s, bw * s, bh * s, 5.0 * s, p.ink(0.08));
        let uw = bw * frac;
        let cw = bw * (cache / m.total.max(1) as f32);
        self.c.fill_rounded(
            bx * s,
            by * s,
            (uw + cw).min(bw) * s,
            bh * s,
            5.0 * s,
            p.accent.with_alpha(0.35),
        );
        self.c.fill_rounded(
            bx * s,
            by * s,
            uw.max(bh) * s,
            bh * s,
            5.0 * s,
            load_color(frac),
        );
        // Légende
        let mut lx = bx;
        let swap = (m.swap_total > 0).then(|| {
            format!(
                "Échange {} / {}",
                bytes(m.swap_used as f64),
                bytes(m.swap_total as f64)
            )
        });
        for (color, label) in [
            (load_color(frac), "Utilisée".to_string()),
            (
                p.accent.with_alpha(0.35),
                format!("Cache {}", bytes(m.cache as f64)),
            ),
        ]
        .into_iter()
        .chain(swap.map(|t| (p.ink(0.3), t)))
        {
            self.c
                .fill_circle((lx + 4.0) * s, (y + 110.0) * s, 4.0 * s, color);
            self.regular.draw_in(
                self.c,
                &label,
                (lx + 12.0) * s,
                (y + 100.0) * s,
                20.0 * s,
                11.5 * s,
                p.text_dim,
            );
            lx += self.regular.measure(&label, 11.5 * s) / s + 28.0;
        }
    }

    /// Tuile des disques, étirée sur `h` pour finir à la hauteur de l'autre colonne
    fn disks(&mut self, x: f32, y: f32, h: f32, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, h);
        self.title(x, y, icon::HARD_DRIVE, "Disques", "");
        // Courbe des débits dans la place restante, sous les disques
        let gy = y + 44.0 + cur.disks.len() as f32 * 50.0 + 6.0;
        let (gx, gw, gh) = (x + 14.0, COL_W - 28.0, y + h - 46.0 - gy);
        if gh >= 24.0 {
            self.grid(gx, gy, gw, gh);
        }
        if self.fixed {
            return;
        }
        if gh >= 24.0 {
            let max = cur
                .disk_read_history
                .iter()
                .chain(cur.disk_write_history.iter())
                .fold(1024.0 * 1024.0, |m: f64, v| m.max(*v));
            let norm = |h: &std::collections::VecDeque<f64>| -> Vec<f32> {
                h.iter().map(|v| (v / max) as f32).collect()
            };
            self.area(
                gx,
                gy,
                gw,
                gh,
                &norm(&cur.disk_read_history),
                p.accent,
                true,
            );
            self.area(
                gx,
                gy,
                gw,
                gh,
                &norm(&cur.disk_write_history),
                p.success,
                false,
            );
        }
        let mut dy = y + 44.0;
        for disk in &cur.disks {
            let used = disk.total.saturating_sub(disk.free) as f32 / disk.total.max(1) as f32;
            self.bold.draw_in(
                self.c,
                &disk.label,
                (x + 16.0) * s,
                dy * s,
                22.0 * s,
                13.0 * s,
                p.text,
            );
            self.text_right(
                false,
                &format!(
                    "{} libres sur {}",
                    bytes(disk.free as f64),
                    bytes(disk.total as f64)
                ),
                x + COL_W - 16.0,
                dy,
                22.0,
                12.0,
                p.text_dim,
            );
            let (bx, by, bw) = (x + 16.0, dy + 28.0, COL_W - 32.0);
            self.c
                .fill_rounded(bx * s, by * s, bw * s, 6.0 * s, 3.0 * s, p.ink(0.08));
            self.c.fill_rounded(
                bx * s,
                by * s,
                (bw * used).max(6.0) * s,
                6.0 * s,
                3.0 * s,
                load_color(used),
            );
            dy += 50.0;
        }
        // Débits de lecture et d'écriture, aux couleurs des courbes
        let mut lx = x + 16.0;
        for (color, label) in [
            (p.accent, format!("Lecture {}", rate(cur.disk_read))),
            (p.success, format!("Écriture {}", rate(cur.disk_write))),
        ] {
            self.c
                .fill_circle((lx + 4.0) * s, (y + h - 23.0) * s, 4.0 * s, color);
            self.regular.draw_in(
                self.c,
                &label,
                (lx + 12.0) * s,
                (y + h - 34.0) * s,
                22.0 * s,
                12.0 * s,
                p.text_dim,
            );
            lx += self.regular.measure(&label, 12.0 * s) / s + 30.0;
        }
    }

    fn gpu(&mut self, x: f32, y: f32, prev: &Snapshot, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, GPU_H);
        let Some(g) = cur.gpu.as_ref() else {
            self.title(x, y, icon::GPU, "Carte graphique", "");
            if !self.fixed {
                return;
            }
            self.regular.draw_in(
                self.c,
                "Aucune carte graphique reconnue",
                (x + 16.0) * s,
                (y + 80.0) * s,
                24.0 * s,
                13.0 * s,
                p.text_dim,
            );
            return;
        };
        self.title(x, y, icon::GPU, "Carte graphique", &g.name);
        if self.fixed {
            return;
        }
        let pg = prev.gpu.as_ref().unwrap_or(g);
        let busy = lerp(pg.busy, g.busy, self.t);
        let vram =
            lerp(pg.vram_used as f32, g.vram_used as f32, self.t) / g.vram_total.max(1) as f32;
        let temp = g.temp.map(|t| lerp(pg.temp.unwrap_or(t), t, self.t));
        // Trois jauges : charge, mémoire vidéo, température (sur 100 °C)
        let gauges = [
            (busy, format!("{:.0} %", busy * 100.0), "Charge".to_string()),
            (
                vram,
                bytes(lerp(pg.vram_used as f32, g.vram_used as f32, self.t) as f64),
                format!("sur {}", bytes(g.vram_total as f64)),
            ),
            (
                temp.unwrap_or(0.0) / 100.0,
                temp.map(|t| format!("{t:.0} °C"))
                    .unwrap_or_else(|| "—".into()),
                "Température".to_string(),
            ),
        ];
        let slot = (COL_W - 28.0) / 3.0;
        for (i, (frac, value, label)) in gauges.into_iter().enumerate() {
            let cx = x + 14.0 + slot * (i as f32 + 0.5);
            let cy = y + 92.0;
            self.ring(cx, cy, 40.0, 7.0, frac, load_color(frac));
            self.bold.draw_centered(
                self.c,
                &value,
                cx * s,
                (cy - 12.0) * s,
                24.0 * s,
                14.0 * s,
                p.text,
            );
            self.regular.draw_centered(
                self.c,
                &label,
                cx * s,
                (cy + 46.0) * s,
                18.0 * s,
                11.5 * s,
                p.text_dim,
            );
        }
        // Consommation et ventilateur
        let mut cx = x + 14.0;
        if let Some(w) = g.watts {
            cx += self.chip(cx, y + GPU_H - 34.0, icon::ZAP, &format!("{w:.0} W")) + 8.0;
        }
        if let Some(rpm) = g.fan_rpm {
            let text = if rpm == 0 {
                "Ventilateur à l'arrêt".to_string()
            } else {
                format!("{rpm} tr/min")
            };
            cx += self.chip(cx, y + GPU_H - 34.0, icon::FAN, &text) + 8.0;
        }
        // Courbe de charge à droite des pastilles
        let gw = x + COL_W - 14.0 - cx;
        if gw > 60.0 {
            let history: Vec<f32> = g.history.iter().copied().collect();
            self.area(cx, y + GPU_H - 36.0, gw, 26.0, &history, p.accent, true);
        }
    }

    fn network(&mut self, x: f32, y: f32, prev: &Snapshot, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, NET_H);
        self.title(x, y, icon::NETWORK, "Réseau", &cur.net.interface);
        let (gx, gy, gw, gh) = (x + 14.0, y + 92.0, COL_W - 28.0, 44.0);
        self.grid(gx, gy, gw, gh);
        if self.fixed {
            return;
        }
        let down = lerp(prev.net.down as f32, cur.net.down as f32, self.t) as f64;
        let up = lerp(prev.net.up as f32, cur.net.up as f32, self.t) as f64;
        let half = (COL_W - 32.0) / 2.0;
        for (i, (glyph, value, label, color)) in [
            (icon::ARROW_DOWN, down, "Réception", p.accent),
            (icon::ARROW_UP, up, "Envoi", p.success),
        ]
        .into_iter()
        .enumerate()
        {
            let vx = x + 16.0 + i as f32 * half;
            self.icons.icon(
                self.c,
                glyph,
                (vx + 8.0) * s,
                (y + 54.0) * s,
                15.0 * s,
                color,
            );
            self.bold.draw_in(
                self.c,
                &rate(value),
                (vx + 24.0) * s,
                (y + 40.0) * s,
                28.0 * s,
                17.0 * s,
                p.text,
            );
            self.regular.draw_in(
                self.c,
                label,
                (vx + 24.0) * s,
                (y + 64.0) * s,
                16.0 * s,
                11.5 * s,
                p.text_dim,
            );
        }
        // Deux courbes sur la même échelle (au moins 64 Ko/s)
        let max = cur
            .net
            .down_history
            .iter()
            .chain(cur.net.up_history.iter())
            .fold(64.0 * 1024.0, |m: f64, v| m.max(*v));
        let norm = |h: &std::collections::VecDeque<f64>| -> Vec<f32> {
            h.iter().map(|v| (v / max) as f32).collect()
        };
        self.area(gx, gy, gw, gh, &norm(&cur.net.down_history), p.accent, true);
        self.area(gx, gy, gw, gh, &norm(&cur.net.up_history), p.success, false);
    }

    fn processes(&mut self, x: f32, y: f32, cur: &Snapshot) {
        let s = self.s;
        let p = theme::palette();
        self.tile(x, y, COL_W, proc_h());
        self.title(x, y, icon::ACTIVITY, "Processus", "% d'un cœur · mémoire");
        if self.fixed {
            return;
        }
        let mut ry = y + 44.0;
        for proc in cur.processes.iter().take(PROC_ROWS) {
            let armed = self.armed == Some(proc.pid);
            let hovered = self.hover == Some(Hit::Kill(proc.pid));
            if armed {
                self.c.fill_rounded(
                    (x + 8.0) * s,
                    ry * s,
                    (COL_W - 16.0) * s,
                    (PROC_ROW - 2.0) * s,
                    8.0 * s,
                    p.danger.with_alpha(0.16),
                );
            }
            let name = self.regular.truncate(&proc.name, 13.0 * s, 124.0 * s);
            self.regular.draw_in(
                self.c,
                &name,
                (x + 16.0) * s,
                ry * s,
                PROC_ROW * s,
                13.0 * s,
                p.text,
            );
            // Petite barre de charge
            let frac = (proc.cpu / 100.0).clamp(0.0, 1.0);
            let (bx, bw) = (x + 150.0, 48.0);
            let by = ry + PROC_ROW / 2.0 - 3.0;
            self.c
                .fill_rounded(bx * s, by * s, bw * s, 6.0 * s, 3.0 * s, p.ink(0.08));
            self.c.fill_rounded(
                bx * s,
                by * s,
                (bw * frac).max(6.0) * s,
                6.0 * s,
                3.0 * s,
                load_color(frac),
            );
            if armed {
                self.text_right(
                    true,
                    "Arrêter ?",
                    x + COL_W - 44.0,
                    ry,
                    PROC_ROW,
                    12.5,
                    p.danger,
                );
            } else {
                self.text_right(
                    false,
                    &format!("{:.0} %", proc.cpu),
                    x + 250.0,
                    ry,
                    PROC_ROW,
                    12.5,
                    p.text,
                );
                self.text_right(
                    false,
                    &bytes(proc.rss as f64),
                    x + COL_W - 44.0,
                    ry,
                    PROC_ROW,
                    12.5,
                    p.text_dim,
                );
            }
            // Bouton d'arrêt
            let (kx, ky) = (x + COL_W - 26.0, ry + PROC_ROW / 2.0);
            if hovered || armed {
                let bg = if armed { p.danger } else { p.tile_hover() };
                self.c.fill_circle(kx * s, ky * s, 11.0 * s, bg);
            }
            let kc = if armed {
                p.on_accent
            } else if hovered {
                p.text
            } else {
                p.text_dim
            };
            self.icons
                .icon(self.c, icon::CLOSE, kx * s, ky * s, 12.0 * s, kc);
            self.hits
                .push(((kx - 14.0, ry, 28.0, PROC_ROW), Hit::Kill(proc.pid)));
            ry += PROC_ROW;
        }
    }
}

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
    /// Clic ailleurs : le panneau se ferme
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.exit = true;
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.exit = true;
        }
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
        if capability == Capability::Keyboard
            && self.keyboard.is_none()
            && let Ok(k) = self.seat_state.get_keyboard(qh, &seat, None)
        {
            self.keyboard = Some(k);
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
delegate_registry!(App);
