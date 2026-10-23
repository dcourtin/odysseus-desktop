//! odysseus-partage : choix de ce qu'une application va voir quand elle
//! demande à partager l'écran (visioconférence dans Firefox…).
//!
//! Appelé par xdg-desktop-portal-wlr (chooser_type=dmenu) : il reçoit sur
//! l'entrée standard une ligne par source (« Monitor: HDMI-A-1 … »,
//! « Window: titre (identifiant) ») et écrit la ligne choisie ; rien en cas
//! d'annulation. Les sources sont montrées en vignettes (captures du
//! compositeur), l'écran d'abord. Clic pour choisir, double-clic ou Entrée
//! pour partager, Échap pour annuler.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    draw::{Canvas, Image, Text},
    ipc,
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

// Géométrie, en pixels logiques
const CARD_W: f32 = 272.0;
const THUMB_H: f32 = 153.0;
const CARD_H: f32 = THUMB_H + 52.0;
const GAP: f32 = 16.0;
const PAD: f32 = 28.0;
const HEADER_H: f32 = 92.0;
const FOOTER_H: f32 = 76.0;
const RADIUS: f32 = 20.0;
const COLS: usize = 3;
const MAX_ROWS: usize = 3;
const BTN_LEFT: u32 = 0x110;

/// Une source proposée par le portail
struct Source {
    /// Ligne reçue, renvoyée telle quelle si elle est choisie
    line: String,
    title: String,
    detail: String,
    window: Option<u64>,
    app_id: String,
    thumb: Option<Image>,
    icon: Option<Image>,
}

/// Lit les sources sur l'entrée standard, avec ce que le compositeur sait des fenêtres
fn read_sources() -> Vec<Source> {
    let windows: Vec<serde_json::Value> = ipc::request(ipc::GET_WINDOWS, "")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut screens = Vec::new();
    let mut others = Vec::new();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let line = line.trim_end().to_string();
        if let Some(rest) = line.strip_prefix("Monitor: ") {
            let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
            screens.push(Source {
                title: "Écran entier".into(),
                detail: name,
                line,
                window: None,
                app_id: String::new(),
                thumb: None,
                icon: None,
            });
        } else if let Some(rest) = line.strip_prefix("Window: ") {
            // « titre (identifiant) » : l'identifiant est entre les dernières parenthèses
            let (title, ident) = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => (&rest[..i], &rest[i + 2..rest.len() - 1]),
                _ => (rest, ""),
            };
            let info = windows
                .iter()
                .find(|w| w["partage"].as_str() == Some(ident));
            let app_id = info
                .and_then(|w| w["app_id"].as_str())
                .unwrap_or("")
                .to_string();
            let workspace = info.and_then(|w| w["workspace"].as_u64());
            others.push(Source {
                title: if title.is_empty() {
                    app_id.clone()
                } else {
                    title.to_string()
                },
                detail: match workspace {
                    Some(n) => format!("Fenêtre · bureau {n}"),
                    None => "Fenêtre".into(),
                },
                line,
                window: info.and_then(|w| w["id"].as_u64()),
                app_id,
                thumb: None,
                icon: None,
            });
        }
    }
    screens.extend(others);
    screens.truncate(COLS * MAX_ROWS);
    screens
}

/// Capture par le compositeur (PNG), réduite en vignette prémultipliée B, G, R, A
fn thumbnail(window: Option<u64>, w: u32, h: u32) -> Option<Image> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path = PathBuf::from(dir).join(format!(
        "odysseus-partage-{}-{}.png",
        std::process::id(),
        window.unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&path);
    let cmd = match window {
        Some(id) => format!("agent-capture {} {id}", path.display()),
        None => format!("agent-capture {}", path.display()),
    };
    ipc::command(&cmd).ok()?;
    let start = Instant::now();
    while !path.exists() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let png = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let img = image::load_from_memory(&png)
        .ok()?
        .thumbnail(w, h)
        .to_rgba8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
        px[0] = (b * a / 255) as u8;
        px[1] = (g * a / 255) as u8;
        px[2] = (r * a / 255) as u8;
    }
    Some(Image {
        width,
        height,
        data,
    })
}

/// Icône d'une application d'après son app_id : « firefox-default » (profil
/// de Firefox) → « firefox » ; sinon l'application installée du même nom
fn app_icon(app_id: &str, apps: &[common::apps::App]) -> Option<Image> {
    let mut name = app_id.to_string();
    loop {
        if let Some(img) =
            common::icons::load(&name, 40).or_else(|| common::icons::load(&name.to_lowercase(), 40))
        {
            return Some(img);
        }
        match name.rfind(['-', '.']) {
            Some(i) if i > 0 => name.truncate(i),
            _ => break,
        }
    }
    apps.iter()
        .find(|a| a.name.eq_ignore_ascii_case(app_id))
        .and_then(|a| common::icons::load(&a.icon, 40))
}

struct Picker {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Picker>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
    icons: Text,
    sources: Vec<Source>,
    selected: Option<usize>,
    hover: Option<Hit>,
    last_click: Option<(usize, Instant)>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    chosen: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Card(usize),
    Cancel,
    Share,
    Outside,
    Panel,
}

impl Picker {
    fn rows(&self) -> usize {
        self.sources.len().div_ceil(COLS).max(1)
    }

    fn cols(&self) -> usize {
        self.sources.len().clamp(1, COLS)
    }

    fn panel_size(&self) -> (f32, f32) {
        let cols = self.cols().max(2) as f32;
        let rows = self.rows() as f32;
        (
            PAD * 2.0 + cols * CARD_W + (cols - 1.0) * GAP,
            HEADER_H + rows * CARD_H + (rows - 1.0) * GAP + FOOTER_H,
        )
    }

    /// Coin haut gauche du panneau, centré
    fn panel_origin(&self) -> (f32, f32) {
        let (pw, ph) = self.panel_size();
        (
            ((self.width as f32 - pw) / 2.0).max(0.0).round(),
            ((self.height as f32 - ph) / 2.0).max(0.0).round(),
        )
    }

    fn card_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (px, py) = self.panel_origin();
        let (pw, _) = self.panel_size();
        let row = i / COLS;
        let in_row = (self.sources.len() - row * COLS).min(COLS) as f32;
        // Rangée centrée dans le panneau
        let row_w = in_row * CARD_W + (in_row - 1.0) * GAP;
        let x0 = px + (pw - row_w) / 2.0;
        (
            x0 + (i % COLS) as f32 * (CARD_W + GAP),
            py + HEADER_H + row as f32 * (CARD_H + GAP),
            CARD_W,
            CARD_H,
        )
    }

    fn buttons(&self) -> [(f32, f32, f32, f32); 2] {
        let (px, py) = self.panel_origin();
        let (pw, ph) = self.panel_size();
        let (bw, bh) = (128.0, 40.0);
        let y = py + ph - FOOTER_H + (FOOTER_H - bh) / 2.0;
        let share_x = px + pw - PAD - bw;
        [(share_x - 12.0 - bw, y, bw, bh), (share_x, y, bw, bh)]
    }

    fn hit(&self, x: f64, y: f64) -> Hit {
        let (x, y) = (x as f32, y as f32);
        let inside =
            |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        for i in 0..self.sources.len() {
            if inside(self.card_rect(i)) {
                return Hit::Card(i);
            }
        }
        let [cancel, share] = self.buttons();
        if inside(cancel) {
            return Hit::Cancel;
        }
        if inside(share) {
            return Hit::Share;
        }
        let (px, py) = self.panel_origin();
        let (pw, ph) = self.panel_size();
        if inside((px, py, pw, ph)) {
            Hit::Panel
        } else {
            Hit::Outside
        }
    }

    fn share(&mut self) {
        if let Some(i) = self.selected {
            self.chosen = Some(self.sources[i].line.clone());
            self.exit = true;
        }
    }

    fn key(&mut self, event: KeyEvent) {
        let n = self.sources.len();
        if n == 0 {
            self.exit = true;
            return;
        }
        let cur = self.selected.unwrap_or(0);
        let next = match event.keysym {
            Keysym::Escape => {
                self.exit = true;
                return;
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                if self.selected.is_none() {
                    Some(0)
                } else {
                    self.share();
                    return;
                }
            }
            Keysym::Right | Keysym::Tab => Some((cur + 1) % n),
            Keysym::Left | Keysym::ISO_Left_Tab => Some((cur + n - 1) % n),
            Keysym::Down => Some((cur + COLS).min(n - 1)),
            Keysym::Up => Some(cur.saturating_sub(COLS)),
            _ => None,
        };
        if let Some(i) = next {
            self.selected = Some(i);
            self.draw();
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        match self.hit(x, y) {
            Hit::Card(i) => {
                let double = self
                    .last_click
                    .is_some_and(|(j, t)| j == i && t.elapsed() < Duration::from_millis(400));
                self.selected = Some(i);
                self.last_click = Some((i, Instant::now()));
                if double {
                    self.share();
                } else {
                    self.draw();
                }
            }
            Hit::Share => self.share(),
            Hit::Cancel | Hit::Outside => self.exit = true,
            Hit::Panel => {}
        }
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let p = theme::palette();
        let s = self.scale.max(1) as f32;
        let (w, h) = (self.width * s as u32, self.height * s as u32);
        // Géométrie calculée avant d'emprunter le tampon
        let (px, py) = self.panel_origin();
        let (pw, ph) = self.panel_size();
        let cards: Vec<_> = (0..self.sources.len()).map(|i| self.card_rect(i)).collect();
        let [cancel, share] = self.buttons();
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
        // Voile sur tout l'écran : la demande passe avant le reste
        c.fill(Rgba::hex(0x000000, 0.45));

        let (x0, y0) = (px * s, py * s);
        c.shadow(
            x0,
            y0 + 12.0 * s,
            pw * s,
            ph * s,
            RADIUS * s,
            40.0 * s,
            0.55,
        );
        c.fill_rounded(x0, y0, pw * s, ph * s, RADIUS * s, p.panel_border());
        c.fill_rounded(
            x0 + s,
            y0 + s,
            pw * s - 2.0 * s,
            ph * s - 2.0 * s,
            (RADIUS - 1.0) * s,
            p.panel.with_alpha(0.98),
        );

        // En-tête
        let ix = x0 + PAD * s;
        c.fill_rounded(
            ix,
            y0 + 26.0 * s,
            40.0 * s,
            40.0 * s,
            12.0 * s,
            p.accent.with_alpha(0.18),
        );
        self.icons.icon(
            &mut c,
            icon::SCREEN_SHARE,
            ix + 20.0 * s,
            y0 + 46.0 * s,
            20.0 * s,
            p.accent,
        );
        let tx = ix + 54.0 * s;
        self.bold.draw_in(
            &mut c,
            "Partager votre écran",
            tx,
            y0 + 22.0 * s,
            26.0 * s,
            18.0 * s,
            p.text,
        );
        let sub = self.regular.truncate(
            "Choisissez ce que l'application verra. Le témoin de la barre arrête le partage.",
            12.5 * s,
            (pw - PAD * 2.0 - 54.0) * s,
        );
        self.regular.draw_in(
            &mut c,
            &sub,
            tx,
            y0 + 48.0 * s,
            20.0 * s,
            12.5 * s,
            p.text_dim,
        );

        if self.sources.is_empty() {
            self.regular.draw_in(
                &mut c,
                "Rien à partager.",
                ix,
                y0 + HEADER_H * s,
                CARD_H * s,
                14.0 * s,
                p.text_dim,
            );
        }

        // Vignettes
        for (i, &(cx, cy, cw, _)) in cards.iter().enumerate() {
            let (cx, cy, cw) = (cx * s, cy * s, cw * s);
            let selected = self.selected == Some(i);
            let hovered = self.hover == Some(Hit::Card(i));
            let th = THUMB_H * s;
            // Cadre de la vignette : accent si choisie
            if selected {
                c.fill_rounded(
                    cx - 3.0 * s,
                    cy - 3.0 * s,
                    cw + 6.0 * s,
                    th + 6.0 * s,
                    15.0 * s,
                    p.accent,
                );
            }
            c.fill_rounded(
                cx,
                cy,
                cw,
                th,
                12.0 * s,
                if hovered { p.tile_hover() } else { p.tile() },
            );
            let src = &self.sources[i];
            match &src.thumb {
                Some(img) => {
                    // Image entière dans la vignette, centrée, proportions gardées
                    let (iw, ih) = (img.width as f32, img.height as f32);
                    let k = (cw - 16.0 * s) / iw;
                    let k = k.min((th - 16.0 * s) / ih);
                    let (dw, dh) = (iw * k, ih * k);
                    c.image(
                        img,
                        cx + (cw - dw) / 2.0,
                        cy + (th - dh) / 2.0,
                        dw,
                        dh,
                        6.0 * s,
                    );
                }
                None => {
                    let glyph = if src.window.is_some() {
                        icon::APP
                    } else {
                        icon::MONITOR
                    };
                    self.icons.icon(
                        &mut c,
                        glyph,
                        cx + cw / 2.0,
                        cy + th / 2.0,
                        34.0 * s,
                        p.text_dim,
                    );
                }
            }
            // Légende : icône de l'application (ou écran), titre, détail
            let ly = cy + th + 10.0 * s;
            let mut lx = cx + 4.0 * s;
            match &src.icon {
                Some(img) => {
                    c.image(img, lx, ly + 4.0 * s, 20.0 * s, 20.0 * s, 0.0);
                    lx += 28.0 * s;
                }
                None => {
                    let glyph = if src.window.is_some() {
                        icon::APP
                    } else {
                        icon::MONITOR
                    };
                    self.icons.icon(
                        &mut c,
                        glyph,
                        lx + 10.0 * s,
                        ly + 14.0 * s,
                        16.0 * s,
                        p.text_soft(),
                    );
                    lx += 28.0 * s;
                }
            }
            let max = cx + cw - lx;
            let title = self.bold.truncate(&src.title, 13.5 * s, max);
            self.bold
                .draw_in(&mut c, &title, lx, ly, 20.0 * s, 13.5 * s, p.text);
            let detail = self.regular.truncate(&src.detail, 11.5 * s, max);
            self.regular.draw_in(
                &mut c,
                &detail,
                lx,
                ly + 20.0 * s,
                16.0 * s,
                11.5 * s,
                p.text_dim,
            );
        }

        // Boutons
        let can_share = self.selected.is_some();
        for (rect, label, primary, hit) in [
            (cancel, "Annuler", false, Hit::Cancel),
            (share, "Partager", true, Hit::Share),
        ] {
            let (bx, by, bw, bh) = (rect.0 * s, rect.1 * s, rect.2 * s, rect.3 * s);
            let hovered = self.hover == Some(hit);
            let bg = match (primary, can_share, hovered) {
                (true, false, _) => p.accent.with_alpha(0.35),
                (true, true, true) => p.accent_hover(),
                (true, true, false) => p.accent,
                (false, _, true) => p.tile_hover(),
                (false, _, false) => p.tile(),
            };
            c.fill_rounded(bx, by, bw, bh, 12.0 * s, bg);
            let fg = if primary { p.on_accent } else { p.text };
            self.bold
                .draw_centered(&mut c, label, bx + bw / 2.0, by, bh, 14.0 * s, fg);
        }

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

fn main() {
    let mut sources = read_sources();
    // Vignette de l'écran avant que le sélecteur ne s'affiche (il serait dessus)
    let (tw, th) = (640, 360);
    for src in sources.iter_mut().filter(|s| s.window.is_none()) {
        src.thumb = thumbnail(None, tw, th);
    }
    let apps = common::apps::load();
    for src in sources.iter_mut().filter(|s| s.window.is_some()) {
        src.icon = app_icon(&src.app_id, &apps);
    }

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("odysseus-partage"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let mut event_loop: EventLoop<'static, Picker> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    // Vignettes des fenêtres, une à une, dans un fil : elles arrivent pendant qu'on choisit
    let (tx, rx) = channel::channel::<(usize, Image)>();
    let windows: Vec<(usize, u64)> = sources
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.window.map(|w| (i, w)))
        .collect();
    std::thread::spawn(move || {
        for (i, id) in windows {
            if let Some(img) = thumbnail(Some(id), tw, th)
                && tx.send((i, img)).is_err()
            {
                break;
            }
        }
    });
    event_loop
        .handle()
        .insert_source(rx, |event, _, picker| {
            if let ChannelEvent::Msg((i, img)) = event
                && let Some(src) = picker.sources.get_mut(i)
            {
                src.thumb = Some(img);
                picker.draw();
            }
        })
        .expect("canal des vignettes");

    let mut picker = Picker {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 16, &shm).expect("pool shm"),
        shm,
        layer,
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        // Un seul écran : déjà choisi, Entrée suffit
        selected: (sources.iter().filter(|s| s.window.is_none()).count() == 1).then_some(0),
        sources,
        hover: None,
        last_click: None,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,
        exit: false,
        chosen: None,
    };

    while !picker.exit {
        if let Err(e) = event_loop.dispatch(None, &mut picker) {
            eprintln!("odysseus-partage : connexion au compositeur perdue ({e})");
            break;
        }
    }
    if let Some(line) = picker.chosen {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

impl CompositorHandler for Picker {
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

impl LayerShellHandler for Picker {
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
        self.width = configure.new_size.0;
        self.height = configure.new_size.1;
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for Picker {
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
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|picker, _, event| picker.key(event)),
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

impl KeyboardHandler for Picker {
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

impl PointerHandler for Picker {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    let hit = Some(self.hit(x, y));
                    if hit != self.hover {
                        self.hover = hit;
                        self.draw();
                    }
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => self.click(x, y),
                _ => {}
            }
        }
    }
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Picker);
delegate_output!(Picker);
delegate_seat!(Picker);
delegate_keyboard!(Picker);
delegate_pointer!(Picker);
delegate_shm!(Picker);
delegate_layer!(Picker);
delegate_registry!(Picker);
