//! Écran de verrouillage (protocole ext-session-lock) : fond d'écran flouté,
//! date et grande horloge en haut, avatar, nom et champ de mot de passe en bas.
//! Le mot de passe est vérifié par PAM dans un fil séparé ; un échec fait
//! trembler le champ, comme sur macOS.

mod background;
mod pam;

use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Timelike};
use common::{
    draw::{Canvas, Image, Text},
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            channel::{self, Event},
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
    },
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

const JOURS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MOIS: [&str; 12] = [
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
/// Durée du tremblement après un mauvais mot de passe
const SHAKE: Duration = Duration::from_millis(450);

struct Lock {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    shm: Shm,
    pool: Option<SlotPool>,
    loop_handle: LoopHandle<'static, Lock>,
    qh: QueueHandle<Lock>,
    session_lock: Option<SessionLock>,
    surfaces: Vec<SessionLockSurface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    regular: Text,
    bold: Text,
    icons: Text,
    /// Fond flouté, à la taille du tampon (recalculé si la taille change)
    background: Option<Image>,
    avatar: Option<Image>,
    user: String,
    full_name: String,
    width: u32,
    height: u32,
    scale: i32,
    password: String,
    /// Vérification en cours (fil PAM)
    checking: bool,
    error: Option<String>,
    shake_start: Option<Instant>,
    /// Réponse du fil PAM
    auth_tx: channel::Sender<Result<(), String>>,
    exit: bool,
}

fn main() {
    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<Lock> = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");
    let lock_state = SessionLockState::new(&globals, &qh);

    let mut event_loop: EventLoop<Lock> = EventLoop::try_new().expect("boucle d'événements");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .expect("source Wayland");

    let (auth_tx, auth_rx) = channel::channel::<Result<(), String>>();
    handle
        .insert_source(auth_rx, |event, _, lock| {
            if let Event::Msg(result) = event {
                lock.auth_result(result);
            }
        })
        .expect("canal PAM");

    // Horloge : redessin au début de chaque minute
    handle
        .insert_source(Timer::immediate(), |_, _, lock| {
            lock.draw();
            let s = 60 - Local::now().second().min(59) as u64;
            TimeoutAction::ToDuration(Duration::from_secs(s))
        })
        .expect("minuterie");

    let user = std::env::var("USER").unwrap_or_else(|_| "david".into());
    let full_name = full_name(&user);
    let mut lock = Lock {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        shm,
        pool: None,
        loop_handle: handle,
        qh: qh.clone(),
        session_lock: None,
        surfaces: Vec::new(),
        keyboard: None,
        modifiers: Modifiers::default(),
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        background: None,
        avatar: load_avatar(),
        user,
        full_name,
        width: 0,
        height: 0,
        scale: 1,
        password: String::new(),
        checking: false,
        error: None,
        shake_start: None,
        auth_tx,
        exit: false,
    };

    // Les sorties sont connues après un premier aller-retour
    event_loop
        .dispatch(Duration::from_millis(100), &mut lock)
        .expect("aller-retour Wayland");
    match lock_state.lock(&qh) {
        Ok(session_lock) => lock.session_lock = Some(session_lock),
        Err(e) => {
            eprintln!("odysseus-lock : verrouillage impossible ({e:?})");
            std::process::exit(1);
        }
    }
    for output in lock.output_state.outputs() {
        let surface = lock.compositor.create_surface(&qh);
        if let Some(session_lock) = lock.session_lock.as_ref() {
            let lock_surface = session_lock.create_lock_surface(surface, &output, &qh);
            lock.surfaces.push(lock_surface);
        }
    }

    while !lock.exit {
        if let Err(e) = event_loop.dispatch(None, &mut lock) {
            eprintln!("odysseus-lock : connexion au compositeur perdue ({e})");
            break;
        }
    }
    // Laisse le temps au déverrouillage de partir avant de fermer la connexion
    let _ = conn.flush();
}

/// Nom complet (champ GECOS de /etc/passwd), sinon l'identifiant avec une majuscule.
fn full_name(user: &str) -> String {
    let gecos = std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| {
            passwd.lines().find_map(|line| {
                let fields: Vec<&str> = line.split(':').collect();
                (fields.first() == Some(&user) && fields.len() > 4)
                    .then(|| fields[4].split(',').next().unwrap_or("").to_string())
            })
        });
    match gecos.filter(|g| !g.is_empty()) {
        Some(name) => name,
        None => {
            let mut chars = user.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// Photo de l'utilisateur : ~/.face (PNG ou JPEG), comme les autres bureaux.
fn load_avatar() -> Option<Image> {
    let path = std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".face");
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .thumbnail(256, 256)
        .to_rgba8();
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
        *px = [
            (b * a / 255) as u8,
            (g * a / 255) as u8,
            (r * a / 255) as u8,
            a as u8,
        ];
    }
    Some(Image {
        width,
        height,
        data,
    })
}

/// Initiales du nom complet (« DC »)
fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

impl Lock {
    fn key(&mut self, event: KeyEvent) {
        if self.checking {
            return;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.submit(),
            Keysym::Escape => {
                self.password.clear();
                self.error = None;
            }
            Keysym::BackSpace => {
                if self.modifiers.ctrl {
                    self.password.clear();
                } else {
                    self.password.pop();
                }
            }
            _ => {
                if self.modifiers.ctrl || self.modifiers.alt || self.modifiers.logo {
                    return;
                }
                if let Some(text) = event
                    .utf8
                    .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
                {
                    self.password.push_str(&text);
                    self.error = None;
                }
            }
        }
        self.draw();
    }

    fn submit(&mut self) {
        if self.password.is_empty() {
            return;
        }
        self.checking = true;
        let (user, password) = (self.user.clone(), std::mem::take(&mut self.password));
        let tx = self.auth_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(pam::authenticate(&user, &password));
        });
        self.draw();
    }

    fn auth_result(&mut self, result: Result<(), String>) {
        self.checking = false;
        match result {
            Ok(()) => {
                if let Some(session_lock) = self.session_lock.take() {
                    session_lock.unlock();
                }
                self.exit = true;
            }
            Err(e) => {
                eprintln!("odysseus-lock : authentification refusée ({e})");
                self.error = Some("Mot de passe incorrect".into());
                self.shake_start = Some(Instant::now());
                self.draw();
            }
        }
    }

    /// Décalage horizontal du champ pendant le tremblement (oscillation amortie).
    fn shake_offset(&mut self) -> f32 {
        let Some(start) = self.shake_start else {
            return 0.0;
        };
        let t = start.elapsed().as_secs_f32();
        if t >= SHAKE.as_secs_f32() {
            self.shake_start = None;
            return 0.0;
        }
        let decay = 1.0 - t / SHAKE.as_secs_f32();
        (t * 2.0 * std::f32::consts::PI * 9.0).sin() * 14.0 * decay
    }

    fn draw(&mut self) {
        if self.width == 0 || self.surfaces.is_empty() {
            return;
        }
        let s = self.scale.max(1) as f32;
        let (w, h) = (
            self.width * self.scale as u32,
            self.height * self.scale as u32,
        );
        if self
            .background
            .as_ref()
            .is_none_or(|b| b.width != w || b.height != h)
        {
            self.background = Some(background::blurred(w, h));
        }
        let shake = self.shake_offset() * s;
        let animating = self.shake_start.is_some();

        let pool = self.pool.get_or_insert_with(|| {
            SlotPool::new((w * h * 4) as usize, &self.shm).expect("pool shm")
        });
        let (buffer, data) =
            match pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("odysseus-lock : tampon ({e})");
                    return;
                }
            };
        if let Some(bg) = self.background.as_ref() {
            data.copy_from_slice(&bg.data);
        }
        let mut canvas = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        let (wf, hf) = (w as f32, h as f32);
        let white = |a: f32| Rgba::hex(0xffffff, a);

        // Haut : date, puis l'heure en très grand
        let now = Local::now();
        let date = format!(
            "{} {} {}",
            JOURS[now.weekday().num_days_from_monday() as usize],
            now.day(),
            MOIS[now.month0() as usize]
        );
        let top = hf * 0.11;
        self.bold.draw_centered(
            &mut canvas,
            &date,
            wf / 2.0,
            top,
            32.0 * s,
            22.0 * s,
            white(0.85),
        );
        let time = format!("{}:{:02}", now.hour(), now.minute());
        self.bold.draw_centered(
            &mut canvas,
            &time,
            wf / 2.0,
            top + 32.0 * s,
            120.0 * s,
            112.0 * s,
            white(0.92),
        );

        // Bas : avatar, nom, champ du mot de passe
        let cx = wf / 2.0;
        let avatar_r = 32.0 * s;
        let field_w = 240.0 * s;
        let field_h = 36.0 * s;
        let field_y = hf - 150.0 * s;
        let name_y = field_y - 40.0 * s;
        let avatar_cy = name_y - 12.0 * s - avatar_r;
        match self.avatar.as_ref() {
            Some(img) => canvas.image(
                img,
                cx - avatar_r,
                avatar_cy - avatar_r,
                2.0 * avatar_r,
                2.0 * avatar_r,
                avatar_r,
            ),
            None => {
                canvas.fill_circle(cx, avatar_cy, avatar_r, white(0.22));
                self.bold.draw_centered(
                    &mut canvas,
                    &initials(&self.full_name),
                    cx,
                    avatar_cy - avatar_r,
                    2.0 * avatar_r,
                    24.0 * s,
                    white(0.95),
                );
            }
        }
        self.bold.draw_centered(
            &mut canvas,
            &self.full_name,
            cx,
            name_y,
            28.0 * s,
            theme::FONT_TITLE * s,
            white(0.95),
        );

        let fx = cx - field_w / 2.0 + shake;
        let field_bg = if self.error.is_some() {
            Rgba::hex(0xf38ba8, 0.28)
        } else {
            white(0.16)
        };
        canvas.fill_rounded(fx, field_y, field_w, field_h, field_h / 2.0, field_bg);
        canvas.stroke_rounded(fx, field_y, field_w, field_h, field_h / 2.0, s, white(0.18));
        let text_px = theme::FONT_SIZE * s;
        if self.checking {
            self.icons.icon(
                &mut canvas,
                icon::LOADER,
                cx + shake,
                field_y + field_h / 2.0,
                16.0 * s,
                white(0.8),
            );
        } else if self.password.is_empty() {
            self.regular.draw_in(
                &mut canvas,
                "Saisir le mot de passe",
                fx + 16.0 * s,
                field_y,
                field_h,
                text_px,
                white(0.55),
            );
        } else {
            // Un point par caractère, sans dépasser le champ
            let n = self.password.chars().count().min(22);
            let dot = 3.5 * s;
            let gap = 10.0 * s;
            for i in 0..n {
                canvas.fill_circle(
                    fx + 18.0 * s + i as f32 * gap,
                    field_y + field_h / 2.0,
                    dot,
                    white(0.95),
                );
            }
            // Bouton « valider » à droite du champ
            let bx = fx + field_w - field_h / 2.0;
            canvas.fill_circle(
                bx,
                field_y + field_h / 2.0,
                field_h / 2.0 - 4.0 * s,
                white(0.25),
            );
            self.icons.icon(
                &mut canvas,
                icon::ARROW_RIGHT,
                bx,
                field_y + field_h / 2.0,
                18.0 * s,
                white(0.95),
            );
        }

        // Sous le champ : erreur, ou Verr. Maj, ou l'aide
        let hint_y = field_y + field_h + 10.0 * s;
        let (hint, color) = if let Some(e) = self.error.as_deref() {
            (e.to_string(), Rgba::hex(0xffb4c4, 1.0))
        } else if self.modifiers.caps_lock {
            ("Verrouillage majuscules activé".to_string(), white(0.8))
        } else {
            ("Entrée pour déverrouiller".to_string(), white(0.45))
        };
        self.regular.draw_centered(
            &mut canvas,
            &hint,
            cx,
            hint_y,
            20.0 * s,
            theme::FONT_SMALL * s,
            color,
        );

        let surface = self.surfaces[0].wl_surface();
        surface.set_buffer_scale(self.scale);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if animating {
            surface.frame(&self.qh, surface.clone());
        }
        if buffer.attach_to(surface).is_ok() {
            surface.commit();
        }
    }
}

impl SessionLockHandler for Lock {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {}

    /// Refus du compositeur (déjà verrouillé par un autre client…) : on s'arrête.
    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        eprintln!("odysseus-lock : verrouillage refusé par le compositeur");
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        self.width = w;
        self.height = h;
        self.draw();
    }
}

impl CompositorHandler for Lock {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            self.pool = None;
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
        if self.shake_start.is_some() {
            self.draw();
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

impl SeatHandler for Lock {
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
                Box::new(|lock, _, event| lock.key(event)),
            );
            match keyboard {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => eprintln!("odysseus-lock : clavier indisponible ({e})"),
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
            // Libéré une fois la distribution des événements finie : la
            // répétition des touches (smithay-client-toolkit) retire son
            // minuteur de calloop en disparaissant, ce qui plante si la
            // boucle est en train de distribuer (clavier débranché)
            self.loop_handle.insert_idle(move |_| k.release());
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Lock {
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
        let caps_changed = modifiers.caps_lock != self.modifiers.caps_lock;
        self.modifiers = modifiers;
        if caps_changed {
            self.draw();
        }
    }
}

impl OutputHandler for Lock {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Lock {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Lock {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Lock);
delegate_output!(Lock);
delegate_seat!(Lock);
delegate_keyboard!(Lock);
delegate_session_lock!(Lock);
delegate_shm!(Lock);
delegate_registry!(Lock);
