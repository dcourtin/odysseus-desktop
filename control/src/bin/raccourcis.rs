//! Aide-mémoire des raccourcis d'odysseus-desktop : panneau ouvert depuis la
//! barre (icône clavier) ou par Super+K. Les touches sont dessinées comme de
//! petites touches de clavier, regroupées par thème sur deux colonnes.
//! Se ferme avec Échap, un clic, ou dès qu'on clique ailleurs.

use std::path::PathBuf;

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
        calloop::EventLoop,
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

const MARGIN: f32 = 16.0;
const PAD: f32 = 20.0;
const COL_W: f32 = 360.0;
const COL_GAP: f32 = 28.0;
const ROW: f32 = 30.0;
const SECTION: f32 = 40.0;
const TITLE_H: f32 = 44.0;

/// (titre de section, [(touches, description)])
type Section = (
    &'static str,
    &'static [(&'static [&'static str], &'static str)],
);

const LEFT: &[Section] = &[
    (
        "Fenêtres",
        &[
            (&["Super", "Entrée"], "Terminal"),
            (&["Super", "Espace"], "Lanceur"),
            (&["Super", "Q"], "Fermer la fenêtre"),
            (&["Super", "F"], "Agrandir / restaurer"),
            (&["Super", "Tab"], "Fenêtre suivante"),
            (&["Super", "glisser"], "Déplacer"),
            (&["Super", "clic droit"], "Redimensionner (glisser)"),
        ],
    ),
    (
        "Aimantation",
        &[
            (&["glisser vers un bord"], "Moitié, quart ou plein écran"),
            (&["Super", "←"], "Moitié gauche"),
            (&["Super", "→"], "Moitié droite"),
            (&["Super", "↑"], "Plein écran utile"),
            (&["Super", "↓"], "Restaurer"),
        ],
    ),
    (
        "Bureaux",
        &[
            (&["Super", "1…9"], "Aller au bureau"),
            (&["Super", "Maj", "1…9"], "Y envoyer la fenêtre"),
        ],
    ),
];

const RIGHT: &[Section] = &[
    (
        "Captures d’écran",
        &[
            (&["Impr. écran"], "Zone (Espace : une fenêtre)"),
            (&["Maj", "Impr. écran"], "Écran entier"),
        ],
    ),
    (
        "Système",
        &[
            (&["Super", "L"], "Verrouiller"),
            (&["Super", "K"], "Ces raccourcis"),
            (&["Volume +", "Volume −"], "Volume"),
            (&["Muet"], "Couper le son"),
            (&["Ctrl", "Alt", "F1…F12"], "Changer de terminal"),
            (&["Super", "Maj", "E"], "Quitter la session"),
        ],
    ),
    (
        "Fichiers et Aperçu",
        &[
            (&["Espace"], "Aperçu rapide d'une image"),
            (&["Suppr"], "Mettre à la corbeille"),
            (&["Ctrl", "Z"], "Annuler la mise à la corbeille"),
            (&["←", "→"], "Image précédente / suivante"),
            (&["R"], "Pivoter"),
            (&["C", "Entrée"], "Recadrer"),
            (&["Ctrl", "S"], "Enregistrer"),
        ],
    ),
];

fn column_height(sections: &[Section]) -> f32 {
    sections
        .iter()
        .map(|(_, rows)| SECTION + rows.len() as f32 * ROW)
        .sum()
}

fn panel_size() -> (f32, f32) {
    let w = 2.0 * PAD + 2.0 * COL_W + COL_GAP;
    let h = 2.0 * PAD + TITLE_H + column_height(LEFT).max(column_height(RIGHT));
    (w, h)
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-raccourcis.pid"))
}

/// Déjà ouvert : on le ferme (bascule) et on renvoie vrai.
fn toggle_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let alive = stat.contains("(odysseus-racco")
        && !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z');
    if !alive || pid == std::process::id() {
        return false;
    }
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    true
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    regular: Text,
    bold: Text,
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
    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<App> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let (pw, ph) = panel_size();
    let (width, height) = ((pw + 2.0 * MARGIN) as u32, (ph + 2.0 * MARGIN) as u32);
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("odysseus-raccourcis"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_size(width, height);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.commit();

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024, &shm).expect("pool shm"),
        shm,
        layer,
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        width,
        height,
        scale: 1,
        configured: false,
        exit: false,
    };
    while !app.exit {
        if event_loop.dispatch(None, &mut app).is_err() {
            break;
        }
    }
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

impl App {
    fn draw(&mut self) {
        if !self.configured {
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
        let (pw, ph) = panel_size();
        let r = theme::PANEL_RADIUS * s;
        c.shadow(
            MARGIN * s,
            (MARGIN + 4.0) * s,
            pw * s,
            ph * s,
            r,
            22.0 * s,
            0.45,
        );
        c.fill_rounded(
            MARGIN * s,
            MARGIN * s,
            pw * s,
            ph * s,
            r,
            theme::palette().panel,
        );
        c.stroke_rounded(
            MARGIN * s,
            MARGIN * s,
            pw * s,
            ph * s,
            r,
            s.max(1.0),
            theme::palette().panel_border(),
        );

        let x0 = MARGIN + PAD;
        let y0 = MARGIN + PAD;
        self.bold.draw_in(
            &mut c,
            "Raccourcis clavier",
            x0 * s,
            y0 * s,
            28.0 * s,
            17.0 * s,
            theme::palette().text,
        );
        for (col, sections) in [LEFT, RIGHT].into_iter().enumerate() {
            let cx = x0 + col as f32 * (COL_W + COL_GAP);
            let mut y = y0 + TITLE_H;
            for (title, rows) in sections {
                self.bold.draw_in(
                    &mut c,
                    &title.to_uppercase(),
                    cx * s,
                    (y + 8.0) * s,
                    24.0 * s,
                    11.0 * s,
                    theme::palette().text_dim,
                );
                y += SECTION;
                for (keys, label) in rows.iter() {
                    row(&self.regular, &self.bold, &mut c, s, cx, y, keys, label);
                    y += ROW;
                }
            }
        }

        let surface = self.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

/// Une ligne : description à gauche, touches (façon touches de clavier) à droite
#[allow(clippy::too_many_arguments)]
fn row(
    regular: &Text,
    bold: &Text,
    c: &mut Canvas,
    s: f32,
    x: f32,
    y: f32,
    keys: &[&str],
    label: &str,
) {
    {
        let key_px = 12.0 * s;
        let cap_h = 22.0;
        // Largeur totale des touches, pour les aligner à droite de la colonne
        let caps: Vec<f32> = keys
            .iter()
            .map(|k| bold.measure(k, key_px) / s + 14.0)
            .collect();
        let total: f32 = caps.iter().sum::<f32>() + 6.0 * (caps.len().saturating_sub(1)) as f32;
        let label_w = COL_W - total - 12.0;
        let label = regular.truncate(label, 13.0 * s, label_w * s);
        regular.draw_in(
            c,
            &label,
            x * s,
            y * s,
            ROW * s,
            13.0 * s,
            theme::palette().text_soft(),
        );
        let mut kx = x + COL_W - total;
        let ky = y + (ROW - cap_h) / 2.0;
        for (k, cw) in keys.iter().zip(caps) {
            c.fill_rounded(
                kx * s,
                ky * s,
                cw * s,
                cap_h * s,
                6.0 * s,
                theme::palette().tile_hover(),
            );
            c.fill_rounded(
                kx * s,
                (ky + cap_h - 2.0) * s,
                cw * s,
                2.0 * s,
                1.0 * s,
                Rgba::hex(0x000000, 0.25),
            );
            bold.draw_centered(
                c,
                k,
                (kx + cw / 2.0) * s,
                ky * s,
                cap_h * s,
                key_px,
                theme::palette().text,
            );
            kx += cw + 6.0;
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
        if events
            .iter()
            .any(|e| matches!(e.kind, PointerEventKind::Press { .. }))
        {
            self.exit = true;
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
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
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
        _: Capability,
    ) {
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
