//! odysseus-roue : roue de raccourcis qui s'ouvre autour du pointeur
//! (Super+R, clic du milieu sur le bureau, Super+clic du milieu partout).
//!
//! Huit places en cercle, choisies dans Réglages > Roue : applications (un
//! point signale celles qui sont ouvertes ; on ramène alors leur fenêtre),
//! actions d'applications, entrées proposées par les applis, dossiers. Un
//! dossier ouvre un second anneau autour de la roue, à son survol ou à son
//! choix. Clic sur une place, ou chiffres 1 à 8, flèches et Entrée ; Échap
//! referme le dossier puis la roue, comme un clic au centre ou à côté. Une
//! place vide ouvre Réglages > Roue.
//!
//! Arguments : `--x X --y Y` (position du pointeur sur l'écran, donnée par le
//! compositeur) ; sans eux, la roue s'ouvre là où le pointeur entre.
//! Sous-commandes pour les applis : voir `cli`.

mod cli;

use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{
    apps::{self, App},
    draw::{Canvas, Image, Text},
    roue::{self, Item, SLOTS, Slot},
    theme::{self, Rgba, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop,
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

// Géométrie, en pixels logiques
const R_DISC: f32 = 148.0;
const R_ITEMS: f32 = 100.0;
const R_HOVER: f32 = 34.0;
const ICON: f32 = 40.0;
const R_CENTER: f32 = 36.0;
/// Second anneau (dossier ouvert) : rayon des icônes et bord extérieur
const R_RING_ITEMS: f32 = 196.0;
const R_RING: f32 = 242.0;
/// Écart minimal entre la roue et le bord de l'écran
const EDGE: f32 = 16.0;
/// Apparition : la roue grandit un peu en s'ouvrant
const APPEAR: Duration = Duration::from_millis(130);
const BTN_LEFT: u32 = 0x110;
const BTN_MIDDLE: u32 = 0x112;

const ICON_COLORS: [u32; 6] = [0x8aadf4, 0xc6a0f6, 0xa6e3a1, 0xf9e2af, 0xf5a3b8, 0x8bd5ca];

/// Une place de la roue
struct Place {
    item: Option<Item>,
    /// Fenêtres de l'appli (identifiant, active) ; vide : pas ouverte
    windows: Vec<(u64, bool)>,
    icon: Option<Option<Image>>,
    /// Même icône, à la taille des aperçus d'un dossier
    mini: Option<Option<Image>>,
    /// Dossier : son nom et ses places remplies (une place vide s'il n'en a pas)
    folder: Option<(String, Vec<Place>)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Center,
    Slot(usize),
    /// Place du dossier ouvert
    Inner(usize),
    Outside,
}

struct Roue {
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
    icons: Text,
    places: Vec<Place>,
    /// Centre voulu (position du pointeur), avant mise à l'écart des bords
    anchor: Option<(f32, f32)>,
    hover: Option<Target>,
    /// Dossier ouvert (place de la roue)
    open: Option<usize>,
    /// Le pointeur a quitté le centre depuis l'ouverture (geste « appuyer,
    /// glisser, relâcher » du clic du milieu)
    travelled: bool,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    opened: Instant,
    exit: bool,
}

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("odysseus-roue.pid"))
}

/// Si une roue est déjà ouverte, la fermer et renvoyer vrai (bascule)
fn toggle_existing() -> bool {
    let Some(pid) = pid_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|st| {
            st.rsplit(')')
                .next()
                .map(|r| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(true);
    if zombie || comm.trim() != "odysseus-roue" || pid == std::process::id() {
        return false;
    }
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    true
}

fn arg_f32(args: &[String], flag: &str) -> Option<f32> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

/// Fenêtres ouvertes, d'après le compositeur : (identifiant, app_id, active)
fn windows() -> Vec<(u64, String, bool)> {
    let Ok(json) = common::ipc::request(common::ipc::GET_WINDOWS, "") else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str(&json) else {
        return Vec::new();
    };
    list.iter()
        .map(|w| {
            (
                w["id"].as_u64().unwrap_or(0),
                w["app_id"].as_str().unwrap_or_default().to_string(),
                w["focused"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

fn places(apps: &[App]) -> Vec<Place> {
    let catalog = roue::catalog(apps);
    let wins = windows();
    let place = |slot: Option<&Slot>| {
        let item = slot.and_then(|s| catalog.iter().find(|it| &it.slot == s).cloned());
        let windows = item
            .as_ref()
            .and_then(|it| it.app)
            .map(|ai| {
                wins.iter()
                    .filter(|(_, app_id, _)| apps::for_window(app_id, apps) == Some(ai))
                    .map(|(id, _, focused)| (*id, *focused))
                    .collect()
            })
            .unwrap_or_default();
        Place {
            item,
            windows,
            icon: None,
            mini: None,
            folder: None,
        }
    };
    roue::load(apps)
        .iter()
        .map(|slot| match slot.as_ref().and_then(Slot::folder) {
            Some(f) => {
                let mut inner: Vec<Place> = f.filled().map(|s| place(Some(s))).collect();
                if inner.is_empty() {
                    inner.push(place(None));
                }
                Place {
                    folder: Some((f.name.clone(), inner)),
                    ..place(None)
                }
            }
            None => place(slot.as_ref()),
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first().filter(|a| !a.starts_with("--")) {
        let _ = first;
        std::process::exit(cli::run(&args));
    }
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
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("odysseus-roue"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    // Tout l'écran, barre comprise : un clic à côté de la roue la ferme
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let mut event_loop: EventLoop<'static, Roue> =
        EventLoop::try_new().expect("boucle d'événements");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("source Wayland");

    let apps = apps::load();
    let anchor = arg_f32(&args, "--x").zip(arg_f32(&args, "--y"));
    let mut roue = Roue {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        pool: SlotPool::new(1024 * 1024 * 8, &shm).expect("pool shm"),
        shm,
        layer,
        keyboard: None,
        pointer: None,
        regular: Text::load(theme::FONT_REGULAR).expect("police"),
        bold: Text::load(theme::FONT_BOLD).expect("police"),
        icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        places: places(&apps),
        anchor,
        hover: None,
        open: None,
        travelled: false,
        width: 0,
        height: 0,
        scale: 1,
        configured: false,
        opened: Instant::now(),
        exit: false,
    };
    // Apparition : quelques images pendant APPEAR
    let _ = event_loop.handle().insert_source(
        Timer::from_duration(Duration::from_millis(16)),
        |_, _, roue: &mut Roue| {
            roue.draw();
            if roue.opened.elapsed() < APPEAR {
                TimeoutAction::ToDuration(Duration::from_millis(16))
            } else {
                TimeoutAction::Drop
            }
        },
    );

    while !roue.exit {
        if let Err(e) = event_loop.dispatch(None, &mut roue) {
            eprintln!("odysseus-roue : connexion au compositeur perdue ({e})");
            break;
        }
    }
    if let Some(path) = pid_file() {
        let _ = std::fs::remove_file(path);
    }
}

impl Roue {
    /// Centre de la roue, écarté des bords de l'écran
    fn center(&self) -> Option<(f32, f32)> {
        let (x, y) = self.anchor?;
        let (w, h) = (self.width as f32, self.height as f32);
        // Place pour le second anneau s'il y a un dossier
        let outer = if self.places.iter().any(|p| p.folder.is_some()) {
            R_RING
        } else {
            R_DISC
        };
        let m = outer + EDGE;
        // Place pour le nom sous la roue
        let below = 48.0;
        Some((
            x.clamp(m.min(w / 2.0), (w - m).max(w / 2.0)),
            y.clamp(m.min(h / 2.0), (h - m - below).max(h / 2.0)),
        ))
    }

    /// Centre d'une place
    fn slot_pos(i: usize, (cx, cy): (f32, f32), k: f32) -> (f32, f32) {
        let a = (-90.0 + i as f32 * 360.0 / SLOTS as f32).to_radians();
        (cx + R_ITEMS * k * a.cos(), cy + R_ITEMS * k * a.sin())
    }

    /// Angle (degrés depuis le haut) et nombre des places du dossier ouvert
    fn ring(&self) -> Option<(f32, usize)> {
        let i = self.open?;
        let n = self.places.get(i)?.folder.as_ref()?.1.len();
        Some((i as f32 * 360.0 / SLOTS as f32, n))
    }

    /// Centre de la place j du dossier ouvert : autour de l'angle du dossier
    fn inner_pos(j: usize, (base, n): (f32, usize), (cx, cy): (f32, f32), k: f32) -> (f32, f32) {
        let step = 360.0 / SLOTS as f32;
        let deg = base + (j as f32 - (n as f32 - 1.0) / 2.0) * step;
        let a = (deg - 90.0).to_radians();
        (
            cx + R_RING_ITEMS * k * a.cos(),
            cy + R_RING_ITEMS * k * a.sin(),
        )
    }

    fn target(&self, x: f32, y: f32) -> Target {
        let Some((cx, cy)) = self.center() else {
            return Target::Outside;
        };
        let (dx, dy) = (x - cx, y - cy);
        let d = (dx * dx + dy * dy).sqrt();
        if d <= R_CENTER {
            return Target::Center;
        }
        // Angle depuis le haut, sens des aiguilles d'une montre
        let a = (dy.atan2(dx).to_degrees() + 90.0).rem_euclid(360.0);
        let step = 360.0 / SLOTS as f32;
        if d > R_DISC {
            // Dans le second anneau : la place du dossier la plus proche
            let Some((base, n)) = self.ring().filter(|_| d <= R_RING) else {
                return Target::Outside;
            };
            let first = base - (n as f32 - 1.0) / 2.0 * step;
            let off = (a - first + step / 2.0).rem_euclid(360.0);
            let j = (off / step) as usize;
            return if j < n {
                Target::Inner(j)
            } else {
                Target::Outside
            };
        }
        Target::Slot(((a + step / 2.0) / step) as usize % SLOTS)
    }

    fn spawn(command: &str) {
        if let Err(e) = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .process_group(0)
            .spawn()
        {
            eprintln!("odysseus-roue : impossible de lancer « {command} » ({e})");
        }
    }

    /// Lance une application d'odysseus voisine de ce programme
    fn sibling(name: &str, args: &[&str]) {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(name)));
        if let Some(exe) = exe {
            let _ = std::process::Command::new(exe)
                .args(args)
                .process_group(0)
                .spawn();
        }
    }

    /// Choix d'une place de la roue : un dossier s'ouvre, le reste se lance
    fn activate(&mut self, i: usize) {
        if self.places.get(i).is_some_and(|p| p.folder.is_some()) {
            self.open = Some(i);
            self.hover = Some(Target::Inner(0));
            return self.draw();
        }
        self.exit = true;
        if let Some(place) = self.places.get(i) {
            Self::launch(place);
        }
    }

    /// Choix d'une place du dossier ouvert
    fn activate_inner(&mut self, j: usize) {
        let place = self
            .open
            .and_then(|i| self.places.get(i))
            .and_then(|p| p.folder.as_ref())
            .and_then(|f| f.1.get(j));
        if let Some(place) = place {
            self.exit = true;
            Self::launch(place);
        }
    }

    fn launch(place: &Place) {
        let Some(item) = place.item.as_ref() else {
            // Place vide : la remplir dans Réglages
            return Self::sibling("odysseus-reglages", &["roue"]);
        };
        // Appli ouverte : ramener sa fenêtre (la suivante, si elle est déjà active)
        if !place.windows.is_empty() {
            let next = place
                .windows
                .iter()
                .position(|w| w.1)
                .map(|f| (f + 1) % place.windows.len())
                .unwrap_or(0);
            let (id, focused) = place.windows[next];
            if !focused {
                let _ = common::ipc::command(&format!("focus {id}"));
            }
            return;
        }
        Self::spawn(&item.command);
    }

    fn set_hover(&mut self, t: Option<Target>) {
        // Survoler un dossier l'ouvre, survoler une autre place le referme
        let open = match t {
            Some(Target::Slot(i)) => self.places[i].folder.is_some().then_some(i),
            _ => self.open,
        };
        if t != self.hover || open != self.open {
            self.hover = t;
            self.open = open;
            self.draw();
        }
    }

    fn close_folder(&mut self) {
        if let Some(i) = self.open.take() {
            self.hover = Some(Target::Slot(i));
            self.draw();
        }
    }

    fn key(&mut self, event: KeyEvent) {
        // Chiffres de la rangée du haut par code physique (AZERTY compris)
        let code = event.raw_code;
        let digit = if (2..=9).contains(&code) {
            Some((code - 2) as usize)
        } else {
            let k = event.keysym.raw();
            (Keysym::KP_1.raw()..=Keysym::KP_8.raw())
                .contains(&k)
                .then(|| (k - Keysym::KP_1.raw()) as usize)
        };
        // Dossier ouvert : les touches vont à ses places
        if let Some((_, n)) = self.ring() {
            let current = match self.hover {
                Some(Target::Inner(j)) => Some(j),
                _ => None,
            };
            if let Some(j) = digit {
                return self.activate_inner(j);
            }
            match event.keysym {
                Keysym::Escape | Keysym::BackSpace => self.close_folder(),
                Keysym::Return | Keysym::KP_Enter | Keysym::space => match current {
                    Some(j) => self.activate_inner(j),
                    None => self.close_folder(),
                },
                Keysym::Right | Keysym::Down | Keysym::Tab => {
                    let j = current.map_or(0, |j| (j + 1) % n);
                    self.set_hover(Some(Target::Inner(j)));
                }
                Keysym::Left | Keysym::Up | Keysym::ISO_Left_Tab => {
                    let j = current.map_or(n - 1, |j| (j + n - 1) % n);
                    self.set_hover(Some(Target::Inner(j)));
                }
                _ => {}
            }
            return;
        }
        if let Some(i) = digit {
            return self.activate(i);
        }
        let current = match self.hover {
            Some(Target::Slot(i)) => Some(i),
            _ => None,
        };
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter | Keysym::space => match current {
                Some(i) => self.activate(i),
                None => self.exit = true,
            },
            Keysym::Right | Keysym::Down | Keysym::Tab => {
                let i = current.map_or(0, |i| (i + 1) % SLOTS);
                self.set_hover(Some(Target::Slot(i)));
            }
            Keysym::Left | Keysym::Up | Keysym::ISO_Left_Tab => {
                let i = current.map_or(SLOTS - 1, |i| (i + SLOTS - 1) % SLOTS);
                self.set_hover(Some(Target::Slot(i)));
            }
            _ => {}
        }
    }

    fn draw(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let s = self.scale.max(1) as f32;
        let center = self.center();
        let ring = self.ring();
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
        let p = theme::palette();
        let t = (self.opened.elapsed().as_secs_f32() / APPEAR.as_secs_f32()).min(1.0);
        let ease = 1.0 - (1.0 - t).powi(3);
        // Voile léger sur le bureau
        c.fill(Rgba::hex(0x000000, 0.10 * ease));

        if let Some((cx, cy)) = center {
            let k = 0.88 + 0.12 * ease;
            let (x, y, r) = (cx * s, cy * s, R_DISC * k * s);
            // Dossier ouvert : un anneau autour de la roue
            if ring.is_some() {
                let ro = R_RING * k * s;
                c.shadow(
                    x - ro,
                    y - ro + 10.0 * s,
                    2.0 * ro,
                    2.0 * ro,
                    ro,
                    36.0 * s,
                    0.35,
                );
                c.fill_circle(x, y, ro, p.panel_border());
                c.fill_circle(x, y, ro - s, p.panel.with_alpha(0.94));
                c.fill_circle(x, y, r + 5.0 * s, p.ink(0.06));
            } else {
                c.shadow(x - r, y - r + 10.0 * s, 2.0 * r, 2.0 * r, r, 36.0 * s, 0.45);
            }
            c.fill_circle(x, y, r, p.panel_border());
            c.fill_circle(x, y, r - s, p.panel.with_alpha(0.97));
            // Centre : fermer
            let center_hover = self.hover == Some(Target::Center);
            c.fill_circle(
                x,
                y,
                R_CENTER * k * s,
                if center_hover {
                    p.tile_hover()
                } else {
                    p.tile()
                },
            );
            self.icons
                .icon(&mut c, icon::CLOSE, x, y, 16.0 * s, p.text_dim);

            let icon_px = (ICON * s).round() as u32;
            let fonts = (&self.icons, &self.bold);
            for i in 0..SLOTS {
                let (ix, iy) = Self::slot_pos(i, (cx, cy), k);
                let (ix, iy) = (ix * s, iy * s);
                if self.hover == Some(Target::Slot(i)) || self.open == Some(i) {
                    c.fill_circle(ix, iy, R_HOVER * s, p.accent.with_alpha(0.22));
                }
                draw_place(&mut c, fonts, &mut self.places[i], ix, iy, s, icon_px);
            }
            if let (Some(ring), Some(open)) = (ring, self.open)
                && let Some((_, inner)) = self.places[open].folder.as_mut()
            {
                for (j, place) in inner.iter_mut().enumerate() {
                    let (ix, iy) = Self::inner_pos(j, ring, (cx, cy), k);
                    let (ix, iy) = (ix * s, iy * s);
                    if self.hover == Some(Target::Inner(j)) {
                        c.fill_circle(ix, iy, R_HOVER * s, p.accent.with_alpha(0.22));
                    }
                    draw_place(&mut c, fonts, place, ix, iy, s, icon_px);
                }
            }

            // Nom de la place survolée, sous la roue
            let describe = |place: &Place| match (&place.folder, place.item.as_ref()) {
                (Some((name, _)), _) => format!("{name} · dossier"),
                (None, Some(it)) if it.app.is_none() && !it.detail.is_empty() => {
                    format!("{} · {}", it.name, it.detail)
                }
                (None, Some(it)) => it.name.clone(),
                (None, None) => "Choisir dans Réglages".to_string(),
            };
            let label = match self.hover {
                Some(Target::Slot(i)) => Some(describe(&self.places[i])),
                Some(Target::Inner(j)) => self
                    .open
                    .and_then(|i| self.places[i].folder.as_ref())
                    .and_then(|f| f.1.get(j))
                    .map(describe),
                Some(Target::Center) => Some("Fermer".to_string()),
                _ => None,
            };
            if let Some(label) = label {
                let px = 13.5 * s;
                let label = self.regular.truncate(&label, px, 360.0 * s);
                let lw = self.regular.measure(&label, px) + 28.0 * s;
                let below = if ring.is_some() { R_RING * k * s } else { r };
                let (lx, ly, lh) = (x - lw / 2.0, y + below + 12.0 * s, 32.0 * s);
                c.fill_rounded(lx, ly, lw, lh, lh / 2.0, p.panel_border());
                c.fill_rounded(
                    lx + s,
                    ly + s,
                    lw - 2.0 * s,
                    lh - 2.0 * s,
                    lh / 2.0 - s,
                    p.panel.with_alpha(0.97),
                );
                self.regular
                    .draw_centered(&mut c, &label, x, ly, lh, px, p.text);
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

/// Icône d'une place, centrée en (ix, iy) (pixels de l'écran)
fn draw_place(
    c: &mut Canvas,
    (icons, bold): (&Text, &Text),
    place: &mut Place,
    ix: f32,
    iy: f32,
    s: f32,
    icon_px: u32,
) {
    let p = theme::palette();
    let half = ICON / 2.0 * s;
    if let Some((_, inner)) = place.folder.as_mut() {
        // Dossier : une pastille avec ses premières icônes en petit
        c.fill_rounded(
            ix - half,
            iy - half,
            2.0 * half,
            2.0 * half,
            12.0 * s,
            p.accent.with_alpha(0.20),
        );
        let filled: Vec<&mut Place> = inner
            .iter_mut()
            .filter(|pl| pl.item.is_some())
            .take(4)
            .collect();
        if filled.is_empty() {
            icons.icon(c, icon::FOLDER, ix, iy, 20.0 * s, p.accent);
            return;
        }
        // Grille de deux colonnes au plus, centrée dans la pastille
        // Aperçus chargés à leur taille exacte : nets, sans réduction au dessin
        let mini_px = (15.0 * s).round() as u32;
        let mini = mini_px as f32;
        let gap = 3.0 * s;
        let cols = filled.len().min(2) as f32;
        let rows = filled.len().div_ceil(2) as f32;
        let x0 = ix - (cols * mini + (cols - 1.0) * gap) / 2.0;
        let y0 = iy - (rows * mini + (rows - 1.0) * gap) / 2.0;
        for (n, pl) in filled.into_iter().enumerate() {
            let (col, row) = ((n % 2) as f32, (n / 2) as f32);
            let mx = x0 + col * (mini + gap);
            let my = y0 + row * (mini + gap);
            let Some(item) = pl.item.as_ref() else {
                continue;
            };
            match pl
                .mini
                .get_or_insert_with(|| common::icons::load(&item.icon, mini_px))
            {
                // Une icône pas carrée garde ses proportions, centrée
                Some(img) => {
                    let (w, h) = (img.width as f32, img.height as f32);
                    c.image(img, mx + (mini - w) / 2.0, my + (mini - h) / 2.0, w, h, 0.0)
                }
                None => c.fill_circle(
                    mx + mini / 2.0,
                    my + mini / 2.0,
                    mini / 2.0,
                    initial_color(&item.name),
                ),
            }
        }
        return;
    }
    let Some(item) = place.item.as_ref() else {
        c.fill_circle(ix, iy, 18.0 * s, p.ink(0.05));
        icons.icon(c, icon::PLUS, ix, iy, 15.0 * s, p.ink(0.35));
        return;
    };
    let img = place
        .icon
        .get_or_insert_with(|| common::icons::load(&item.icon, icon_px));
    match img {
        // Taille exacte, centrée : nette (une icône pas carrée garde ses proportions)
        Some(img) => {
            let (w, h) = (img.width as f32, img.height as f32);
            c.image(img, ix - w / 2.0, iy - h / 2.0, w, h, 0.0)
        }
        None => {
            // Sans icône : l'initiale sur une pastille de couleur
            c.fill_circle(ix, iy, half, initial_color(&item.name));
            let initial: String = item.name.chars().take(1).collect::<String>().to_uppercase();
            bold.draw_centered(
                c,
                &initial,
                ix,
                iy - half,
                2.0 * half,
                18.0 * s,
                Rgba::hex(0x1d1916, 1.0),
            );
        }
    }
    // Action ou proposition : petit repère dans le coin
    if item.app.is_none() {
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 8.0 * s, p.panel);
        c.fill_circle(ix + half * 0.8, iy + half * 0.8, 6.5 * s, p.accent);
    }
    if !place.windows.is_empty() {
        c.fill_circle(ix, iy + half + 7.0 * s, 2.5 * s, p.text_dim);
    }
}

/// Couleur de pastille d'une appli sans icône, tirée de son nom
fn initial_color(name: &str) -> Rgba {
    let hash = name.bytes().map(|b| b as usize).sum::<usize>();
    Rgba::hex(ICON_COLORS[hash % ICON_COLORS.len()], 1.0)
}

impl CompositorHandler for Roue {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            for place in &mut self.places {
                place.icon = None;
                place.mini = None;
                for inner in place.folder.iter_mut().flat_map(|f| f.1.iter_mut()) {
                    inner.icon = None;
                    inner.mini = None;
                }
            }
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

impl LayerShellHandler for Roue {
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
        self.width = configure.new_size.0.max(1);
        self.height = configure.new_size.1.max(1);
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for Roue {
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
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => eprintln!("odysseus-roue : clavier indisponible ({e})"),
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => eprintln!("odysseus-roue : pointeur indisponible ({e})"),
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

impl KeyboardHandler for Roue {
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

impl PointerHandler for Roue {
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
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // Sans position donnée par le compositeur : là où le pointeur entre
                    if self.anchor.is_none() {
                        self.anchor = Some((x, y));
                    }
                    let t = self.target(x, y);
                    if matches!(t, Target::Slot(_) | Target::Inner(_)) {
                        self.travelled = true;
                    }
                    let hover = (t != Target::Outside).then_some(t);
                    self.set_hover(hover);
                }
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => match self.target(x, y) {
                    Target::Slot(i) => self.activate(i),
                    Target::Inner(j) => self.activate_inner(j),
                    _ => self.exit = true,
                },
                // Clic du milieu maintenu, glissé vers une place puis relâché
                PointerEventKind::Release {
                    button: BTN_MIDDLE, ..
                } if self.travelled => match self.target(x, y) {
                    // Relâché sur un dossier : il reste ouvert
                    Target::Slot(i) if self.places[i].folder.is_none() => self.activate(i),
                    Target::Inner(j) => self.activate_inner(j),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

impl OutputHandler for Roue {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Roue {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Roue {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Roue);
delegate_output!(Roue);
delegate_seat!(Roue);
delegate_keyboard!(Roue);
delegate_pointer!(Roue);
delegate_shm!(Roue);
delegate_layer!(Roue);
delegate_registry!(Roue);
