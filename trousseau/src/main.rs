//! odysseus-trousseau : fenêtre de mot de passe du trousseau.
//!
//!     odysseus-trousseau --demander <ouvrir|creer|changer> <nom du trousseau>
//!
//! Lancée par odysseus-notify quand oo7-daemon a besoin d'un mot de passe
//! (interface org.freedesktop.secrets.CliPrompter) : ouvrir le trousseau,
//! le créer (mot de passe saisi deux fois) ou en changer le mot de passe.
//! Le mot de passe est écrit sur la sortie standard, une socket dont oo7
//! lit l'autre extrémité ; rien n'est écrit si l'utilisateur annule.

use std::io::Write;

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

const PANEL_W: f32 = 520.0;
const PAD: f32 = 28.0;
const FIELD_H: f32 = 44.0;
const RADIUS: f32 = 20.0;
const BTN_LEFT: u32 = 0x110;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Open,
    Create,
    Change,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Field(usize),
    Cancel,
    Ok,
    Outside,
    Panel,
}

struct Prompt {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Prompt>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    icons: Text,
    role: Role,
    keyring: String,
    /// Mot de passe, et sa confirmation (création, changement)
    fields: [String; 2],
    focus: usize,
    error: Option<String>,
    hover: Option<Hit>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    exit: bool,
    answer: Option<String>,
}

/// Efface un mot de passe en mémoire avant de le rendre
fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    for b in bytes.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

impl Prompt {
    fn field_count(&self) -> usize {
        if self.role == Role::Open { 1 } else { 2 }
    }

    fn title(&self) -> &'static str {
        match self.role {
            Role::Open => "Ouvrir le trousseau",
            Role::Create => "Créer le trousseau",
            Role::Change => "Changer le mot de passe du trousseau",
        }
    }

    fn body(&self) -> String {
        let name = &self.keyring;
        match self.role {
            Role::Open => format!(
                "Une application demande un secret rangé dans {name}, qui est \
                 verrouillé. Saisissez son mot de passe (en principe celui de votre session)."
            ),
            Role::Create => format!(
                "Une application veut ranger un secret : {name} va être créé. \
                 Choisissez son mot de passe. Celui de votre session lui permet de s'ouvrir \
                 tout seul à la connexion."
            ),
            Role::Change => format!(
                "Choisissez le nouveau mot de passe pour {name}. Gardez celui de \
                 votre session pour qu'il s'ouvre tout seul à la connexion."
            ),
        }
    }

    fn body_lines(&self) -> Vec<String> {
        self.regular
            .wrap(&self.body(), 13.0, PANEL_W - 2.0 * PAD - 56.0, 4)
    }

    /// Hauteur du panneau (pixels logiques)
    fn panel_h(&self) -> f32 {
        let body = self.body_lines().len() as f32 * 19.0;
        let fields = self.field_count() as f32 * (22.0 + FIELD_H + 12.0);
        26.0 + 28.0 + body + 18.0 + fields + 22.0 + 20.0 + 40.0 + PAD
    }

    fn origin(&self) -> (f32, f32) {
        (
            ((self.width as f32 - PANEL_W) / 2.0).max(0.0).round(),
            ((self.height as f32 - self.panel_h()) / 2.5)
                .max(0.0)
                .round(),
        )
    }

    fn fields_top(&self) -> f32 {
        let (_, py) = self.origin();
        py + 26.0 + 28.0 + self.body_lines().len() as f32 * 19.0 + 18.0
    }

    fn field_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (px, _) = self.origin();
        let y = self.fields_top() + i as f32 * (22.0 + FIELD_H + 12.0) + 22.0;
        (px + PAD, y, PANEL_W - 2.0 * PAD, FIELD_H)
    }

    fn buttons(&self) -> [(f32, f32, f32, f32); 2] {
        let (px, py) = self.origin();
        let (bw, bh) = (128.0, 40.0);
        let y = py + self.panel_h() - PAD - bh;
        let ok_x = px + PANEL_W - PAD - bw;
        [(ok_x - 12.0 - bw, y, bw, bh), (ok_x, y, bw, bh)]
    }

    fn hit(&self, x: f64, y: f64) -> Hit {
        let (x, y) = (x as f32, y as f32);
        let inside =
            |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        for i in 0..self.field_count() {
            if inside(self.field_rect(i)) {
                return Hit::Field(i);
            }
        }
        let [cancel, ok] = self.buttons();
        if inside(cancel) {
            return Hit::Cancel;
        }
        if inside(ok) {
            return Hit::Ok;
        }
        let (px, py) = self.origin();
        if inside((px, py, PANEL_W, self.panel_h())) {
            Hit::Panel
        } else {
            Hit::Outside
        }
    }

    fn submit(&mut self) {
        if self.fields[0].is_empty() {
            self.focus = 0;
            self.error = Some("Le mot de passe est vide".into());
            self.draw();
            return;
        }
        if self.field_count() == 2 {
            if self.focus == 0 && self.fields[1].is_empty() {
                self.focus = 1;
                self.error = None;
                self.draw();
                return;
            }
            if self.fields[0] != self.fields[1] {
                wipe(&mut self.fields[1]);
                self.focus = 1;
                self.error = Some("Les deux mots de passe diffèrent".into());
                self.draw();
                return;
            }
        }
        self.answer = Some(std::mem::take(&mut self.fields[0]));
        wipe(&mut self.fields[1]);
        self.exit = true;
    }

    fn key(&mut self, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.submit(),
            Keysym::Tab | Keysym::ISO_Left_Tab | Keysym::Down | Keysym::Up => {
                self.focus = (self.focus + 1) % self.field_count();
                self.draw();
            }
            Keysym::BackSpace => {
                let f = &mut self.fields[self.focus];
                if self.modifiers.ctrl {
                    wipe(f);
                } else {
                    f.pop();
                }
                self.error = None;
                self.draw();
            }
            _ => {
                if self.modifiers.ctrl || self.modifiers.logo || self.modifiers.alt {
                    return;
                }
                if let Some(text) = event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                {
                    self.fields[self.focus].push_str(&text);
                    self.error = None;
                    self.draw();
                }
            }
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        match self.hit(x, y) {
            Hit::Field(i) => {
                self.focus = i;
                self.draw();
            }
            Hit::Ok => self.submit(),
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
        let (px, py) = self.origin();
        let ph = self.panel_h();
        let lines = self.body_lines();
        let fields: Vec<_> = (0..self.field_count())
            .map(|i| self.field_rect(i))
            .collect();
        let [cancel, ok] = self.buttons();
        let title = self.title();
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
        c.fill(Rgba::hex(0x000000, 0.45));

        let (x0, y0, pw, ph) = (px * s, py * s, PANEL_W * s, ph * s);
        c.shadow(x0, y0 + 12.0 * s, pw, ph, RADIUS * s, 40.0 * s, 0.55);
        c.fill_rounded(x0, y0, pw, ph, RADIUS * s, p.panel_border());
        c.fill_rounded(
            x0 + s,
            y0 + s,
            pw - 2.0 * s,
            ph - 2.0 * s,
            (RADIUS - 1.0) * s,
            p.panel.with_alpha(0.98),
        );

        // Cadenas, titre, explication
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
            icon::LOCK,
            ix + 20.0 * s,
            y0 + 46.0 * s,
            20.0 * s,
            p.accent,
        );
        let tx = ix + 56.0 * s;
        self.bold
            .draw_in(&mut c, title, tx, y0 + 22.0 * s, 26.0 * s, 17.0 * s, p.text);
        for (i, l) in lines.iter().enumerate() {
            self.regular.draw_in(
                &mut c,
                l,
                tx,
                y0 + (50.0 + i as f32 * 19.0) * s,
                19.0 * s,
                13.0 * s,
                p.text_dim,
            );
        }

        // Champs : un point par caractère
        for (i, &(fx, fy, fw, fh)) in fields.iter().enumerate() {
            let label = match (self.role, i) {
                (Role::Open, _) => "Mot de passe",
                (_, 0) => "Nouveau mot de passe",
                _ => "Confirmation",
            };
            self.bold.draw_in(
                &mut c,
                label,
                fx * s,
                (fy - 22.0) * s,
                20.0 * s,
                12.0 * s,
                p.text_soft(),
            );
            let (fx, fy, fw, fh) = (fx * s, fy * s, fw * s, fh * s);
            let focused = self.focus == i;
            if focused {
                c.fill_rounded(
                    fx - 2.0 * s,
                    fy - 2.0 * s,
                    fw + 4.0 * s,
                    fh + 4.0 * s,
                    13.0 * s,
                    p.accent,
                );
            }
            c.fill_rounded(fx, fy, fw, fh, 11.0 * s, p.panel);
            c.fill_rounded(fx, fy, fw, fh, 11.0 * s, p.tile());
            let n = self.fields[i].chars().count().min(36);
            for k in 0..n {
                c.fill_circle(
                    fx + 18.0 * s + k as f32 * 11.0 * s,
                    fy + fh / 2.0,
                    3.5 * s,
                    p.text,
                );
            }
            if focused {
                let cx = fx + 14.0 * s + n as f32 * 11.0 * s;
                c.fill_rounded(cx, fy + 12.0 * s, 2.0 * s, fh - 24.0 * s, s, p.accent);
            }
        }

        // Erreur, Verr. Maj ou aide
        let hint_y = fields.last().map(|f| f.1 + f.3 + 8.0).unwrap_or(0.0) * s;
        let (hint, color) = match &self.error {
            Some(e) => (e.as_str(), p.danger),
            None if self.modifiers.caps_lock => ("Verrouillage majuscules activé", p.accent),
            None => ("Entrée pour valider, Échap pour annuler", p.text_dim),
        };
        self.regular.draw_in(
            &mut c,
            hint,
            x0 + PAD * s,
            hint_y,
            20.0 * s,
            12.0 * s,
            color,
        );

        for (rect, label, primary, hit) in [
            (cancel, "Annuler", false, Hit::Cancel),
            (
                ok,
                if self.role == Role::Open {
                    "Ouvrir"
                } else {
                    "Valider"
                },
                true,
                Hit::Ok,
            ),
        ] {
            let (bx, by, bw, bh) = (rect.0 * s, rect.1 * s, rect.2 * s, rect.3 * s);
            let hovered = self.hover == Some(hit);
            let bg = match (primary, hovered) {
                (true, true) => p.accent_hover(),
                (true, false) => p.accent,
                (false, true) => p.tile_hover(),
                (false, false) => p.tile(),
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (role, keyring) = match args.as_slice() {
        [flag, role, rest @ ..] if flag == "--demander" => (
            match role.as_str() {
                "creer" => Role::Create,
                "changer" => Role::Change,
                _ => Role::Open,
            },
            rest.join(" "),
        ),
        _ => {
            eprintln!("usage : odysseus-trousseau --demander <ouvrir|creer|changer> <trousseau>");
            std::process::exit(2);
        }
    };
    // Le trousseau de session s'appelle « login » (ou « Login ») chez oo7 et gnome-keyring
    let keyring = match keyring.to_lowercase().as_str() {
        "" | "login" | "default" | "default keyring" => "le trousseau de session".to_string(),
        _ => format!("le trousseau « {keyring} »"),
    };

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
        Some("odysseus-trousseau"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let mut event_loop: EventLoop<'static, Prompt> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let mut prompt = Prompt {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 16, &shm).expect("pool shm"),
        shm,
        layer,
        loop_handle: event_loop.handle(),
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        role,
        keyring,
        fields: [String::new(), String::new()],
        focus: 0,
        error: None,
        hover: None,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,
        exit: false,
        answer: None,
    };

    while !prompt.exit {
        if let Err(e) = event_loop.dispatch(None, &mut prompt) {
            eprintln!("odysseus-trousseau : connexion au compositeur perdue ({e})");
            break;
        }
    }
    wipe(&mut prompt.fields[0]);
    wipe(&mut prompt.fields[1]);
    if let Some(mut answer) = prompt.answer.take() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(answer.as_bytes());
        let _ = out.flush();
        wipe(&mut answer);
    }
}

impl CompositorHandler for Prompt {
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

impl LayerShellHandler for Prompt {
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

impl SeatHandler for Prompt {
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
                Box::new(|prompt, _, event| prompt.key(event)),
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

impl KeyboardHandler for Prompt {
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
        let caps = modifiers.caps_lock != self.modifiers.caps_lock;
        self.modifiers = modifiers;
        if caps {
            self.draw();
        }
    }
}

impl PointerHandler for Prompt {
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

impl OutputHandler for Prompt {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Prompt {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Prompt {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Prompt);
delegate_output!(Prompt);
delegate_seat!(Prompt);
delegate_keyboard!(Prompt);
delegate_pointer!(Prompt);
delegate_shm!(Prompt);
delegate_layer!(Prompt);
delegate_registry!(Prompt);
