//! Lanceur façon Spotlight : panneau centré dans le tiers haut de l'écran,
//! recherche pendant la frappe dans les applications installées.
//! Flèches / souris pour choisir, Entrée pour lancer, Échap pour fermer.
//! Sans correspondance, Entrée exécute le texte tapé comme commande.
//! Relancer le lanceur alors qu'il est ouvert le ferme (bascule).
//! Arguments facultatifs : requête initiale (`launcher foot`).

mod apps;

use std::{os::unix::process::CommandExt, path::PathBuf};

use apps::App;
use common::{
    draw::{Canvas, Text},
    theme::{self, Rgba},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, LoopHandle},
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

// Géométrie du panneau, en pixels logiques (grille de 4)
const PANEL_W: f32 = 640.0;
/// Marge transparente autour du panneau, pour son ombre
const MARGIN: f32 = 40.0;
const SEARCH_H: f32 = 64.0;
const ROW_H: f32 = 48.0;
const PAD: f32 = 8.0;
const RADIUS: f32 = 20.0;
const MAX_ROWS: usize = 7;
/// Distance au haut de l'écran
const TOP_OFFSET: i32 = 180;
const BTN_LEFT: u32 = 0x110;

const ICON_COLORS: [u32; 6] = [0x8aadf4, 0xc6a0f6, 0xa6e3a1, 0xf9e2af, 0xf5a3b8, 0x8bd5ca];

enum Entry {
    App(usize),
    /// Exécuter le texte tapé comme commande
    Run(String),
    /// Poser la question à l'Assistant (« ? question », ou en dernier choix)
    Ask(String),
}

struct Launcher {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Launcher>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    apps: Vec<App>,
    query: String,
    entries: Vec<Entry>,
    selected: usize,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    /// Icônes chargées, par indice d'application (None : pas d'icône)
    icon_cache: std::collections::HashMap<usize, Option<common::draw::Image>>,
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-launcher.pid"))
}

/// Si un lanceur tourne déjà, le fermer et renvoyer vrai (comportement de bascule).
fn toggle_existing() -> bool {
    let Some(path) = pid_file() else {
        return false;
    };
    let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    // Un processus zombie (terminé, pas encore récupéré) ne compte pas
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|st| {
            st.rsplit(')')
                .next()
                .map(|r| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(true);
    if zombie || comm.trim() != "launcher" {
        return false;
    }
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    true
}

fn panel_height(rows: usize) -> f32 {
    if rows == 0 {
        SEARCH_H
    } else {
        SEARCH_H + 1.0 + 2.0 * PAD + rows as f32 * ROW_H
    }
}

fn main() {
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
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("odysseus-launcher"),
        None,
    );
    layer.set_anchor(Anchor::TOP);
    layer.set_margin(TOP_OFFSET, 0, 0, 0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);

    let mut event_loop: EventLoop<'static, Launcher> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let mut launcher = Launcher {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 4, &shm).expect("pool shm"),
        shm,
        layer,
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        apps: apps::load(),
        // Requête initiale optionnelle : `launcher foot`
        query: std::env::args().skip(1).collect::<Vec<_>>().join(" "),
        entries: Vec::new(),
        selected: 0,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,
        exit: false,
        icon_cache: std::collections::HashMap::new(),
    };
    launcher.update_entries();
    let (w, h) = launcher.surface_size();
    launcher.layer.set_size(w, h);
    launcher.layer.commit();

    while !launcher.exit {
        // Compositeur arrêté ou connexion perdue : on s'arrête proprement
        if let Err(e) = event_loop.dispatch(None, &mut launcher) {
            eprintln!("launcher : connexion au compositeur perdue ({e})");
            break;
        }
    }
    if let Some(path) = pid_file() {
        let _ = std::fs::remove_file(path);
    }
}

impl Launcher {
    fn surface_size(&self) -> (u32, u32) {
        let h = panel_height(self.entries.len()) + 2.0 * MARGIN;
        ((PANEL_W + 2.0 * MARGIN) as u32, h.ceil() as u32)
    }

    fn update_entries(&mut self) {
        let mut scored: Vec<(i32, usize)> = self
            .apps
            .iter()
            .enumerate()
            .filter_map(|(i, app)| apps::score(&self.query, app).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        let limit = if self.query.is_empty() {
            MAX_ROWS
        } else {
            MAX_ROWS - 1
        };
        self.entries = scored
            .into_iter()
            .take(limit)
            .map(|(_, i)| Entry::App(i))
            .collect();
        // « ? question » : rien que l'Assistant
        if let Some(q) = self.query.trim_start().strip_prefix('?') {
            self.entries = vec![Entry::Ask(q.trim().to_string())];
        } else if !self.query.trim().is_empty() {
            self.entries.truncate(MAX_ROWS - 2);
            self.entries.push(Entry::Run(self.query.trim().to_string()));
            self.entries.push(Entry::Ask(self.query.trim().to_string()));
        }
        self.selected = 0;
    }

    /// Après un changement de requête : redimensionner si besoin, sinon redessiner.
    fn refresh(&mut self) {
        self.update_entries();
        let (w, h) = self.surface_size();
        if h != self.height {
            self.layer.set_size(w, h);
            self.layer.commit();
        } else {
            self.draw();
        }
    }

    fn launch(&mut self, index: usize) {
        let command = match self.entries.get(index) {
            Some(Entry::App(i)) => {
                let app = &self.apps[*i];
                if app.terminal {
                    let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
                    format!("{term} -e {}", app.exec)
                } else {
                    app.exec.clone()
                }
            }
            Some(Entry::Run(cmd)) => cmd.clone(),
            Some(Entry::Ask(question)) => {
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("odysseus-assistant")))
                    .unwrap_or_else(|| "odysseus-assistant".into());
                let _ = std::process::Command::new(exe)
                    .arg(question)
                    .process_group(0)
                    .spawn();
                self.exit = true;
                return;
            }
            None => return,
        };
        // Groupe de processus à part : l'application survit à la fermeture du lanceur
        if let Err(e) = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .process_group(0)
            .spawn()
        {
            eprintln!("launcher : impossible de lancer « {command} » ({e})");
        }
        self.exit = true;
    }

    fn key(&mut self, event: KeyEvent) {
        let count = self.entries.len().max(1);
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.launch(self.selected),
            Keysym::Down | Keysym::Tab => {
                self.selected = (self.selected + 1) % count;
                self.draw();
            }
            Keysym::Up | Keysym::ISO_Left_Tab => {
                self.selected = (self.selected + count - 1) % count;
                self.draw();
            }
            Keysym::BackSpace => {
                if self.modifiers.ctrl {
                    // Ctrl+Retour arrière : effacer le dernier mot
                    let trimmed = self.query.trim_end().len();
                    self.query.truncate(trimmed);
                    let cut = self.query.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    self.query.truncate(cut);
                } else {
                    self.query.pop();
                }
                self.refresh();
            }
            _ => {
                if self.modifiers.ctrl || self.modifiers.logo || self.modifiers.alt {
                    return;
                }
                if let Some(text) = event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                {
                    self.query.push_str(&text);
                    self.refresh();
                }
            }
        }
    }

    /// Ligne sous le point (x, y) en coordonnées logiques de la surface.
    fn row_at(&self, x: f64, y: f64) -> Option<usize> {
        let top = (MARGIN + SEARCH_H + 1.0 + PAD) as f64;
        let (left, right) = (MARGIN as f64, (MARGIN + PANEL_W) as f64);
        if x < left || x > right || y < top {
            return None;
        }
        let row = ((y - top) / ROW_H as f64) as usize;
        (row < self.entries.len()).then_some(row)
    }

    fn inside_panel(&self, x: f64, y: f64) -> bool {
        let h = panel_height(self.entries.len()) as f64;
        let m = MARGIN as f64;
        x >= m && x <= m + PANEL_W as f64 && y >= m && y <= m + h
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
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
        c.clear();

        // Panneau : ombre, liseré, fond
        let (x0, y0) = (MARGIN * s, MARGIN * s);
        let (pw, ph) = (PANEL_W * s, panel_height(self.entries.len()) * s);
        c.shadow(x0, y0 + 12.0 * s, pw, ph, RADIUS * s, 32.0 * s, 0.5);
        c.fill_rounded(x0, y0, pw, ph, RADIUS * s, theme::palette().panel_border());
        c.fill_rounded(
            x0 + s,
            y0 + s,
            pw - 2.0 * s,
            ph - 2.0 * s,
            (RADIUS - 1.0) * s,
            theme::palette().panel.with_alpha(0.97),
        );

        // Champ de recherche : loupe, texte (ou indication), curseur
        let mid = y0 + SEARCH_H * s / 2.0;
        let ix = x0 + 24.0 * s;
        c.fill_rounded(
            ix,
            mid - 9.0 * s,
            15.0 * s,
            15.0 * s,
            7.5 * s,
            theme::palette().text_dim,
        );
        c.fill_rounded(
            ix + 2.0 * s,
            mid - 7.0 * s,
            11.0 * s,
            11.0 * s,
            5.5 * s,
            theme::palette().panel.with_alpha(0.97),
        );
        for k in 0..4 {
            let d = k as f32 * 1.5 * s;
            c.fill_rounded(
                ix + 11.5 * s + d,
                mid + 2.5 * s + d,
                3.0 * s,
                3.0 * s,
                1.5 * s,
                theme::palette().text_dim,
            );
        }
        let tx = x0 + 56.0 * s;
        let size = 20.0 * s;
        let caret_x = if self.query.is_empty() {
            self.regular.draw_in(
                &mut c,
                "Rechercher une application ou une commande",
                tx,
                y0,
                SEARCH_H * s,
                size,
                theme::palette().text_dim,
            );
            tx
        } else {
            let max_w = pw - (tx - x0) - 24.0 * s;
            let shown = self.regular.truncate(&self.query, size, max_w);
            self.regular.draw_in(
                &mut c,
                &shown,
                tx,
                y0,
                SEARCH_H * s,
                size,
                theme::palette().text,
            );
            tx + self.regular.measure(&shown, size)
        };
        c.fill_rounded(
            caret_x + s,
            mid - 12.0 * s,
            2.0 * s,
            24.0 * s,
            s,
            theme::palette().accent,
        );

        if !self.entries.is_empty() {
            c.fill_rounded(
                x0 + 16.0 * s,
                y0 + SEARCH_H * s,
                pw - 32.0 * s,
                s,
                0.0,
                theme::palette().subtle,
            );
        }

        // Icônes des applications affichées (chargées une fois, gardées en cache)
        let icon_px = (30.0 * s) as u32;
        for entry in &self.entries {
            if let Entry::App(idx) = entry {
                let idx = *idx;
                if let std::collections::hash_map::Entry::Vacant(slot) = self.icon_cache.entry(idx)
                {
                    slot.insert(common::icons::load(&self.apps[idx].icon, icon_px));
                }
            }
        }

        // Résultats
        for (i, entry) in self.entries.iter().enumerate() {
            let y = y0 + (SEARCH_H + 1.0 + PAD) * s + i as f32 * ROW_H * s;
            let row_h = ROW_H * s;
            if i == self.selected {
                let mut sel = theme::palette().accent;
                sel.a = 0.22;
                c.fill_rounded(x0 + PAD * s, y, pw - 2.0 * PAD * s, row_h, 12.0 * s, sel);
            }
            let (title, detail, initial, color) = match entry {
                Entry::App(idx) => {
                    let app = &self.apps[*idx];
                    let hash = app
                        .name
                        .bytes()
                        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
                    let initial = app
                        .name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string();
                    (
                        app.name.clone(),
                        app.comment.clone(),
                        initial,
                        ICON_COLORS[hash % ICON_COLORS.len()],
                    )
                }
                Entry::Run(cmd) => (
                    format!("Exécuter « {cmd} »"),
                    "Commande".to_string(),
                    "›".to_string(),
                    0x9a9ab0,
                ),
                Entry::Ask(q) => (
                    if q.is_empty() {
                        "Demander à l'Assistant".to_string()
                    } else {
                        format!("Demander à l'Assistant : « {q} »")
                    },
                    "Assistant".to_string(),
                    "?".to_string(),
                    0xd97757,
                ),
            };
            // Icône : celle de l'application, sinon carré arrondi coloré avec l'initiale
            let icon = 30.0 * s;
            let ix = x0 + 20.0 * s;
            let iy = y + (row_h - icon) / 2.0;
            let image = match entry {
                Entry::App(idx) => self.icon_cache.get(idx).and_then(Option::as_ref),
                Entry::Run(_) | Entry::Ask(_) => None,
            };
            if let Some(image) = image {
                c.image(image, ix, iy, icon, icon, 0.0);
            } else {
                c.fill_rounded(ix, iy, icon, icon, 9.0 * s, Rgba::hex(color, 0.9));
                let isz = 15.0 * s;
                let iw = self.bold.measure(&initial, isz);
                self.bold.draw_in(
                    &mut c,
                    &initial,
                    ix + (icon - iw) / 2.0,
                    iy,
                    icon,
                    isz,
                    Rgba::hex(0x1e1e2e, 1.0),
                );
            }

            // Nom puis description, raccourcis à la largeur disponible
            let nx = ix + icon + 14.0 * s;
            let right_limit = x0 + pw - 48.0 * s;
            let tsz = 16.0 * s;
            let title = self.bold.truncate(&title, tsz, right_limit - nx);
            self.bold
                .draw_in(&mut c, &title, nx, y, row_h, tsz, theme::palette().text);
            let dx = nx + self.bold.measure(&title, tsz) + 12.0 * s;
            let dsz = 13.0 * s;
            if right_limit - dx > 40.0 * s && !detail.is_empty() {
                let detail = self.regular.truncate(&detail, dsz, right_limit - dx);
                self.regular.draw_in(
                    &mut c,
                    &detail,
                    dx,
                    y,
                    row_h,
                    dsz,
                    theme::palette().text_dim,
                );
            }
            if i == self.selected {
                self.regular.draw_in(
                    &mut c,
                    "↵",
                    x0 + pw - 36.0 * s,
                    y,
                    row_h,
                    16.0 * s,
                    theme::palette().text_dim,
                );
            }
        }

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

impl CompositorHandler for Launcher {
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

impl LayerShellHandler for Launcher {
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
        let (w, h) = self.surface_size();
        self.width = if configure.new_size.0 > 0 {
            configure.new_size.0
        } else {
            w
        };
        self.height = if configure.new_size.1 > 0 {
            configure.new_size.1
        } else {
            h
        };
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for Launcher {
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
                Box::new(|launcher, _, event| launcher.key(event)),
            );
            match keyboard {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => eprintln!("launcher : clavier indisponible ({e})"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => eprintln!("launcher : pointeur indisponible ({e})"),
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

impl KeyboardHandler for Launcher {
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

impl PointerHandler for Launcher {
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
                PointerEventKind::Motion { .. } => {
                    if let Some(row) = self.row_at(x, y)
                        && row != self.selected
                    {
                        self.selected = row;
                        self.draw();
                    }
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => {
                    if let Some(row) = self.row_at(x, y) {
                        self.launch(row);
                    } else if !self.inside_panel(x, y) {
                        self.exit = true;
                    }
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_pointer!(Launcher);
delegate_shm!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);
