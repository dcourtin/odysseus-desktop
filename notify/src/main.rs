//! odysseus-notify : démon de notifications du bureau, inspiré de macOS.
//!
//! - Bannières en haut à droite : elles glissent depuis le bord, s'empilent
//!   (3 au plus), restent 5 s ; le survol suspend le délai et montre la croix.
//! - Centre de notifications (clic sur l'heure de la barre) : panneau qui glisse
//!   depuis la droite, calendrier du mois, historique groupé par application.
//! - « Ne pas déranger » : plus de bannières (sauf critiques), l'historique reste.
//!
//! Utilisation en client : `odysseus-notify --center` (ouvrir/fermer le centre),
//! `--dnd on|off|toggle`, `--send TITRE [TEXTE]`, `--volume` (indicateur de volume).

mod bus;
mod calendar;
mod card;
mod client;
mod icons;
mod model;

use std::time::{Duration, Instant};

use common::{
    draw::{Canvas, Text},
    theme::{self, icon},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, Mode, PostAction,
            generic::{FdWrapper, Generic},
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

use bus::{Bus, Request};
use card::{CARD_W, Fonts, Hit};
use model::{Reason, Store};

/// Marge transparente autour des cartes (place pour l'ombre)
const MARGIN: f32 = 16.0;
const STACK_GAP: f32 = 10.0;
/// Hauteur réservée aux bannières (3 cartes au plus)
const BANNERS_H: u32 = 640;
const SLIDE: Duration = Duration::from_millis(260);
const LEAVE: Duration = Duration::from_millis(220);
const BTN_LEFT: u32 = 0x110;

/// Une surface layer-shell et son tampon
struct Panel {
    layer: LayerSurface,
    pool: SlotPool,
    width: u32,
    height: u32,
    configured: bool,
    /// Rappel de rendu demandé (animation en cours)
    frame_pending: bool,
}

impl Panel {
    fn surface(&self) -> &wl_surface::WlSurface {
        self.layer.wl_surface()
    }
}

struct Center {
    panel: Panel,
    opened: Instant,
    closing: Option<Instant>,
    scroll: f32,
    hits: Vec<Hit>,
    /// Boutons du centre : « Tout effacer » et effacement par groupe (zones logiques)
    clear_all: Option<(f32, f32, f32, f32)>,
    clear_groups: Vec<(String, f32, f32, f32, f32)>,
    /// Hauteur totale du contenu (pour borner le défilement)
    content_h: f32,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<App>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    fonts: Fonts,
    bus: Bus,
    store: Store,
    banners: Option<Panel>,
    banner_hits: Vec<Hit>,
    center: Option<Center>,
    /// Pointeur : surface survolée et position logique
    pointer_at: Option<(wl_surface::WlSurface, f32, f32)>,
    scale: i32,
    last_frame: Instant,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(client::run(&args));
    }

    let bus = match Bus::connect() {
        Ok(bus) => bus,
        Err(e) => {
            eprintln!("odysseus-notify : {e}");
            std::process::exit(1);
        }
    };

    let conn = Connection::connect_to_env().expect("pas de compositeur Wayland (WAYLAND_DISPLAY)");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registre Wayland");
    let qh: QueueHandle<App> = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponible");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell indisponible");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponible");

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("boucle d'événements");
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .expect("source Wayland");

    // Messages D-Bus : le descripteur de la connexion est surveillé par la boucle
    let fd = bus.channel.watch().fd;
    handle
        .insert_source(
            Generic::new(unsafe { FdWrapper::new(fd) }, Interest::READ, Mode::Level),
            |_, _, app| {
                app.handle_bus();
                Ok(PostAction::Continue)
            },
        )
        .expect("source D-Bus");

    // Échéances des bannières et heures relatives (« il y a 2 min »)
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(250)),
            |_, _, app| {
                app.tick();
                TimeoutAction::ToDuration(Duration::from_millis(250))
            },
        )
        .expect("minuterie");

    let mut store = Store::default();
    store.dnd = client::dnd_file().is_some_and(|p| p.exists());

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        qh,
        pointer: None,
        keyboard: None,
        fonts: Fonts {
            regular: Text::load(theme::FONT_REGULAR).expect("police"),
            bold: Text::load(theme::FONT_BOLD).expect("police"),
            icons: Text::load(theme::FONT_ICONS).expect("police d'icônes"),
        },
        bus,
        store,
        banners: None,
        banner_hits: Vec::new(),
        center: None,
        pointer_at: None,
        scale: 1,
        last_frame: Instant::now(),
    };

    // Une notification a pu arriver pendant la réservation du nom (démarrage à la
    // demande par D-Bus) : déjà lue, elle n'activerait plus le descripteur
    app.handle_bus();

    loop {
        if let Err(e) = event_loop.dispatch(None, &mut app) {
            eprintln!("odysseus-notify : connexion au compositeur perdue ({e})");
            break;
        }
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

impl App {
    fn new_panel(
        &self,
        layer_kind: Layer,
        namespace: &str,
        anchor: Anchor,
        size: (u32, u32),
        keyboard: bool,
    ) -> Panel {
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            layer_kind,
            Some(namespace),
            None,
        );
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(if keyboard {
            KeyboardInteractivity::OnDemand
        } else {
            KeyboardInteractivity::None
        });
        layer.commit();
        let pool = SlotPool::new(1024 * 1024, &self.shm).expect("pool shm");
        Panel {
            layer,
            pool,
            width: size.0,
            height: size.1,
            configured: false,
            frame_pending: false,
        }
    }

    fn handle_bus(&mut self) {
        for request in self.bus.poll() {
            match request {
                Request::Notify(incoming, msg) => {
                    let id = self.store.add(incoming);
                    self.bus.reply(msg.method_return().append1(id));
                    self.refresh();
                }
                Request::Close(id) => {
                    if self.store.remove(id) {
                        self.bus.closed(id, Reason::Closed as u32);
                    }
                    self.refresh();
                }
                Request::ToggleCenter => self.toggle_center(),
                Request::SetDnd(on) => self.set_dnd(on),
                Request::GetDnd(msg) => {
                    let reply = msg.method_return().append1(self.store.dnd);
                    self.bus.reply(reply);
                }
            }
        }
    }

    fn set_dnd(&mut self, on: bool) {
        self.store.dnd = on;
        if let Some(path) = client::dnd_file() {
            if on {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, b"1\n");
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        self.redraw_center();
    }

    fn tick(&mut self) {
        let hovered = self.hovered_banner();
        self.store.tick(hovered);
        self.refresh();
    }

    /// Bannière sous le pointeur
    fn hovered_banner(&self) -> Option<u32> {
        let (surface, x, y) = self.pointer_at.as_ref()?;
        let panel = self.banners.as_ref()?;
        if surface != panel.surface() {
            return None;
        }
        self.banner_hits
            .iter()
            .find(|h| h.contains(*x, *y))
            .map(|h| h.id)
    }

    /// Élément survolé (bannière, carte ou bouton du centre) : un redessin
    /// n'est utile que s'il change.
    fn hover_key(&self) -> Option<String> {
        if let Some(id) = self.hovered_banner() {
            return Some(format!("b{id}"));
        }
        let (surface, x, y) = self.pointer_at.as_ref()?;
        let center = self.center.as_ref()?;
        if surface != center.panel.surface() {
            return None;
        }
        let (x, y) = (*x, *y);
        if let Some((bx, by, bw, bh)) = center.clear_all
            && x >= bx
            && x < bx + bw
            && y >= by
            && y < by + bh
        {
            return Some("tout".into());
        }
        if let Some((app, ..)) = center
            .clear_groups
            .iter()
            .find(|(_, gx, gy, gw, gh)| x >= *gx && x < gx + gw && y >= *gy && y < gy + gh)
        {
            return Some(format!("g{app}"));
        }
        center
            .hits
            .iter()
            .find(|h| h.contains(x, y + 4.0) || h.on_close(x, y))
            .map(|h| format!("c{}", h.id))
    }

    /// Après tout changement : crée, redessine ou détruit la surface des bannières.
    fn refresh(&mut self) {
        for id in self.store.reap(LEAVE) {
            self.bus.closed(id, Reason::Expired as u32);
        }
        if self.store.banners.is_empty() {
            if self.banners.take().is_some() {
                self.banner_hits.clear();
            }
            self.redraw_center();
            return;
        }
        if self.banners.is_none() {
            let w = (CARD_W + 2.0 * MARGIN) as u32;
            self.banners = Some(self.new_panel(
                Layer::Overlay,
                "odysseus-notifications",
                Anchor::TOP | Anchor::RIGHT,
                (w, BANNERS_H),
                false,
            ));
            self.last_frame = Instant::now();
            return; // dessin à la configuration
        }
        self.draw_banners();
        self.redraw_center();
    }

    fn draw_banners(&mut self) {
        let hovered = self.hovered_banner();
        let s = self.scale.max(1) as f32;
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
        let Some(panel) = self.banners.as_mut() else {
            return;
        };
        if !panel.configured || panel.frame_pending {
            return;
        }
        let (w, h) = (panel.width * s as u32, panel.height * s as u32);
        let Ok((buffer, data)) =
            panel
                .pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut canvas = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        canvas.clear();

        let mut animating = false;
        let mut target = MARGIN;
        let mut hits = Vec::new();
        let region = Region::new(&self.compositor).ok();
        for n in self.store.banners.iter_mut().take(model::MAX_BANNERS) {
            let card_h = card::height(&self.fonts, n, 3);
            // Glisse vers sa place dans la pile
            if n.y == 0.0 {
                n.y = target;
            }
            let k = (dt * 16.0).min(1.0);
            n.y += (target - n.y) * k;
            if (target - n.y).abs() > 0.3 {
                animating = true;
            } else {
                n.y = target;
            }
            // Entrée depuis la droite, sortie vers la droite
            let t_in = n.appear.elapsed().as_secs_f32() / SLIDE.as_secs_f32();
            let mut dx = (1.0 - ease_out(t_in)) * (CARD_W + MARGIN);
            if t_in < 1.0 || n.value_animating() {
                animating = true;
            }
            if let Some(leaving) = n.leaving {
                let t = leaving.elapsed().as_secs_f32() / LEAVE.as_secs_f32();
                dx += ease_out(t) * (CARD_W + MARGIN);
                animating = true;
            }
            let hit = card::draw(
                &mut canvas,
                &self.fonts,
                n,
                MARGIN + dx,
                n.y,
                s,
                hovered == Some(n.id),
                3,
            );
            if let Some(r) = region.as_ref() {
                r.add(
                    hit.x as i32 - 4,
                    hit.y as i32 - 4,
                    hit.w as i32 + 8,
                    hit.h as i32 + 8,
                );
            }
            hits.push(hit);
            target += card_h + STACK_GAP;
        }
        self.banner_hits = hits;

        let surface = panel.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        if let Some(r) = region.as_ref() {
            surface.set_input_region(Some(r.wl_region()));
        }
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if animating {
            surface.frame(&self.qh, surface.clone());
            panel.frame_pending = true;
        }
        if buffer.attach_to(surface).is_ok() {
            panel.layer.commit();
        }
    }

    fn toggle_center(&mut self) {
        match self.center.as_mut() {
            Some(center) if center.closing.is_none() => {
                center.closing = Some(Instant::now());
                self.draw_center();
            }
            Some(_) => {}
            None => self.open_center(),
        }
    }

    fn open_center(&mut self) {
        // Les bannières affichées rejoignent le centre, comme sur macOS
        for n in self.store.banners.iter_mut() {
            if n.leaving.is_none() {
                n.leaving = Some(Instant::now() - LEAVE);
                n.deadline = Some(Instant::now() - Duration::from_millis(1));
            }
        }
        let w = (CARD_W + 2.0 * MARGIN) as u32;
        let panel = self.new_panel(
            Layer::Top,
            "odysseus-notification-center",
            Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM,
            (w, 0),
            true,
        );
        self.center = Some(Center {
            panel,
            opened: Instant::now(),
            closing: None,
            scroll: 0.0,
            hits: Vec::new(),
            clear_all: None,
            clear_groups: Vec::new(),
            content_h: 0.0,
        });
        self.refresh();
    }

    fn close_center(&mut self) {
        if let Some(center) = self.center.as_mut()
            && center.closing.is_none()
        {
            center.closing = Some(Instant::now());
            self.draw_center();
        }
    }

    fn redraw_center(&mut self) {
        if self.center.is_some() {
            self.draw_center();
        }
    }

    fn draw_center(&mut self) {
        let s = self.scale.max(1) as f32;
        let hovered_point = self.pointer_at.clone();
        // Fin de l'animation de fermeture : le panneau disparaît
        if self
            .center
            .as_ref()
            .is_some_and(|c| c.closing.is_some_and(|t| t.elapsed() >= LEAVE))
        {
            self.center = None;
            return;
        }
        let Some(center) = self.center.as_mut() else {
            return;
        };
        let panel = &mut center.panel;
        if !panel.configured || panel.frame_pending || panel.height == 0 {
            return;
        }
        let (w, h) = (panel.width * s as u32, panel.height * s as u32);
        let Ok((buffer, data)) =
            panel
                .pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut canvas = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        canvas.clear();

        // Glissement depuis la droite à l'ouverture, vers la droite à la fermeture
        let t_open = ease_out(center.opened.elapsed().as_secs_f32() / SLIDE.as_secs_f32());
        let t_close = center
            .closing
            .map(|t| ease_out(t.elapsed().as_secs_f32() / LEAVE.as_secs_f32()))
            .unwrap_or(0.0);
        let animating = t_open < 1.0 || center.closing.is_some();
        let dx = ((1.0 - t_open) + t_close) * (CARD_W + MARGIN);
        let x = MARGIN + dx;
        let hover = hovered_point
            .as_ref()
            .filter(|(surf, _, _)| surf == panel.layer.wl_surface())
            .map(|(_, px, py)| (*px, *py));

        let fonts = &self.fonts;
        let mut y = MARGIN - center.scroll;
        // Calendrier du mois
        y += calendar::draw(&mut canvas, fonts, x, y, CARD_W, s) + 2.0 * STACK_GAP;

        // « Ne pas déranger » actif : rappel discret
        if self.store.dnd {
            let ph = 32.0;
            canvas.fill_rounded(x * s, y * s, CARD_W * s, ph * s, ph / 2.0 * s, theme::PANEL);
            fonts.icons.icon(
                &mut canvas,
                icon::MOON,
                (x + 20.0) * s,
                (y + ph / 2.0) * s,
                15.0 * s,
                theme::ACCENT,
            );
            fonts.bold.draw_in(
                &mut canvas,
                "Ne pas déranger",
                (x + 36.0) * s,
                y * s,
                ph * s,
                13.0 * s,
                theme::TEXT,
            );
            y += ph + STACK_GAP;
        }

        // En-tête « Notifications » et « Tout effacer »
        let header_h = 28.0;
        fonts.bold.draw_in(
            &mut canvas,
            "Notifications",
            (x + 4.0) * s,
            y * s,
            header_h * s,
            theme::FONT_TITLE * s,
            theme::TEXT,
        );
        center.clear_all = None;
        center.clear_groups.clear();
        center.hits.clear();
        let groups = self.store.groups();
        if !groups.is_empty() {
            let label = "Tout effacer";
            let lw = fonts.regular.measure(label, 13.0) + 20.0;
            let (bx, by, bh) = (x + CARD_W - lw, y + 2.0, header_h - 4.0);
            let on =
                hover.is_some_and(|(px, py)| px >= bx && px < bx + lw && py >= by && py < by + bh);
            canvas.fill_rounded(
                bx * s,
                by * s,
                lw * s,
                bh * s,
                bh / 2.0 * s,
                if on { theme::TILE_HOVER } else { theme::TILE },
            );
            fonts.regular.draw_centered(
                &mut canvas,
                label,
                (bx + lw / 2.0) * s,
                by * s,
                bh * s,
                13.0 * s,
                theme::TEXT,
            );
            center.clear_all = Some((bx, by, lw, bh));
        }
        y += header_h + STACK_GAP;

        if groups.is_empty() {
            let eh = 120.0;
            fonts.icons.icon(
                &mut canvas,
                icon::BELL_OFF,
                (x + CARD_W / 2.0) * s,
                (y + 40.0) * s,
                28.0 * s,
                theme::TEXT_DIM,
            );
            fonts.regular.draw_centered(
                &mut canvas,
                "Aucune notification",
                (x + CARD_W / 2.0) * s,
                (y + 64.0) * s,
                24.0 * s,
                theme::FONT_SIZE * s,
                theme::TEXT_DIM,
            );
            y += eh;
        }
        for (app_name, list) in &groups {
            // Groupe : nom de l'application, nombre, bouton d'effacement
            let gh = 24.0;
            let title = if list.len() > 1 {
                format!("{app_name} · {}", list.len())
            } else {
                app_name.clone()
            };
            fonts.bold.draw_in(
                &mut canvas,
                &title,
                (x + 4.0) * s,
                y * s,
                gh * s,
                13.0 * s,
                theme::TEXT_DIM,
            );
            let (cx, cy) = (x + CARD_W - 12.0, y + gh / 2.0);
            let on = hover.is_some_and(|(px, py)| (px - cx).powi(2) + (py - cy).powi(2) < 144.0);
            canvas.fill_circle(
                cx * s,
                cy * s,
                10.0 * s,
                if on { theme::TILE_HOVER } else { theme::TILE },
            );
            fonts.icons.icon(
                &mut canvas,
                icon::CLOSE,
                cx * s,
                cy * s,
                11.0 * s,
                theme::TEXT_DIM,
            );
            center
                .clear_groups
                .push((app_name.clone(), cx - 12.0, cy - 12.0, 24.0, 24.0));
            y += gh + 4.0;
            for n in list {
                let card_h = card::height(fonts, n, 4);
                let over = hover.is_some_and(|(px, py)| {
                    px >= x && px < x + CARD_W && py >= y - 4.0 && py < y + card_h
                });
                let hit = card::draw(&mut canvas, fonts, n, x, y, s, over, 4);
                center.hits.push(hit);
                y += card_h + STACK_GAP;
            }
            y += STACK_GAP;
        }
        center.content_h = y + center.scroll + MARGIN;

        let surface = panel.layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if animating {
            surface.frame(&self.qh, surface.clone());
            panel.frame_pending = true;
        }
        if buffer.attach_to(surface).is_ok() {
            panel.layer.commit();
        }
    }

    /// Clic sur une bannière ou une carte du centre.
    fn click(&mut self, surface: &wl_surface::WlSurface, x: f32, y: f32) {
        let in_banners = self
            .banners
            .as_ref()
            .is_some_and(|p| p.surface() == surface);
        let in_center = self
            .center
            .as_ref()
            .is_some_and(|c| c.panel.surface() == surface);
        if in_center {
            let center = self.center.as_ref().unwrap();
            if let Some((bx, by, bw, bh)) = center.clear_all
                && x >= bx
                && x < bx + bw
                && y >= by
                && y < by + bh
            {
                let ids: Vec<u32> = self.store.history.iter().map(|n| n.id).collect();
                for id in ids {
                    self.store.remove(id);
                    self.bus.closed(id, Reason::Dismissed as u32);
                }
                self.draw_center();
                return;
            }
            if let Some(app) = center
                .clear_groups
                .iter()
                .find(|(_, gx, gy, gw, gh)| x >= *gx && x < gx + gw && y >= *gy && y < gy + gh)
                .map(|(a, ..)| a.clone())
            {
                let ids: Vec<u32> = self
                    .store
                    .history
                    .iter()
                    .filter(|n| n.app_name == app)
                    .map(|n| n.id)
                    .collect();
                for id in ids {
                    self.store.remove(id);
                    self.bus.closed(id, Reason::Dismissed as u32);
                }
                self.draw_center();
                return;
            }
        }
        let hits = if in_banners {
            &self.banner_hits
        } else if in_center {
            &self.center.as_ref().unwrap().hits
        } else {
            return;
        };
        let Some(hit) = hits.iter().find(|h| h.contains(x, y) || h.on_close(x, y)) else {
            return;
        };
        let id = hit.id;
        if hit.on_close(x, y) {
            // Croix : simple fermeture
        } else if let Some(key) = hit.button_at(x, y) {
            let key = key.to_string();
            self.bus.action_invoked(id, &key);
        } else if self.store.get(id).is_some_and(|n| n.has_default) {
            self.bus.action_invoked(id, "default");
            // L'application ouverte passe devant : le centre se ferme
            self.close_center();
        }
        let resident = self.store.get(id).is_some_and(|n| n.resident);
        if !resident && self.store.remove(id) {
            self.bus.closed(id, Reason::Dismissed as u32);
        }
        self.refresh();
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.banners.as_ref().is_some_and(|p| &p.layer == layer) {
            self.banners = None;
        }
        if self
            .center
            .as_ref()
            .is_some_and(|c| &c.panel.layer == layer)
        {
            self.center = None;
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if let Some(panel) = self.banners.as_mut().filter(|p| &p.layer == layer) {
            if w > 0 {
                panel.width = w;
            }
            if h > 0 {
                panel.height = h;
            }
            panel.configured = true;
            self.draw_banners();
        } else if let Some(center) = self.center.as_mut().filter(|c| &c.panel.layer == layer) {
            if w > 0 {
                center.panel.width = w;
            }
            if h > 0 {
                center.panel.height = h;
            }
            center.panel.configured = true;
            self.draw_center();
        }
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
            self.draw_banners();
            self.redraw_center();
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
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(panel) = self.banners.as_mut().filter(|p| p.surface() == surface) {
            panel.frame_pending = false;
            self.refresh();
        } else if let Some(center) = self
            .center
            .as_mut()
            .filter(|c| c.panel.surface() == surface)
        {
            center.panel.frame_pending = false;
            self.draw_center();
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

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        // Une souris rapide envoie des centaines de positions par seconde : on ne
        // redessine qu'une fois par lot, et seulement si l'élément survolé change.
        let before = self.hover_key();
        let mut scrolled = false;
        for event in events {
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_at = Some((event.surface.clone(), x, y));
                }
                PointerEventKind::Leave { .. } => self.pointer_at = None,
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => self.click(&event.surface.clone(), x, y),
                PointerEventKind::Axis { vertical, .. } => {
                    if let Some(center) = self.center.as_mut()
                        && center.panel.surface() == &event.surface
                    {
                        let max = (center.content_h - center.panel.height as f32).max(0.0);
                        center.scroll = (center.scroll + vertical.absolute as f32).clamp(0.0, max);
                        scrolled = true;
                    }
                }
                _ => {}
            }
        }
        if scrolled || self.hover_key() != before {
            self.draw_banners();
            self.redraw_center();
        }
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
    /// Clic ailleurs (une fenêtre prend le clavier) : le centre se ferme.
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self
            .center
            .as_ref()
            .is_some_and(|c| c.panel.surface() == surface)
        {
            self.close_center();
        }
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
            self.close_center();
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
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
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
delegate_pointer!(App);
delegate_keyboard!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);
