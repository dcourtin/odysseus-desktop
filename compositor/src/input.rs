use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, Focus,
            GrabStartData as PointerGrabStartData, MotionEvent,
        },
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    grabs::{MoveSurfaceGrab, resize_grab::ResizeEdge},
    state::{ModKey, Odysseus},
};

/// Codes evdev des boutons gauche et droit
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Actions déclenchées par les raccourcis du compositeur
#[derive(Clone, Copy, Debug)]
enum Action {
    Terminal,
    CloseWindow,
    Quit,
    /// Ctrl+Alt+F1…F12 : changer de terminal virtuel (mode tty)
    SwitchVt(i32),
    /// Capture d'écran : sélection (zone ou fenêtre), ou écran entier
    Capture,
    CaptureFull,
    /// Touche pendant une sélection de capture
    SelectionKey(Keysym),
    Workspace(usize),
    MoveToWorkspace(usize),
    Launcher,
    ToggleMaximize,
    CycleWindows,
    /// Super+M : réduire la fenêtre active
    Minimize,
    Lock,
    /// Super+K : aide-mémoire des raccourcis
    Shortcuts,
    /// Super+A : panneau de l'Assistant
    Assistant,
    /// Super+← / → / ↑ : ranger la fenêtre active ; Super+↓ : la restaurer
    Snap(crate::snap::Zone),
    Unsnap,
    /// Touches multimédia : volume +/- (pas de 5 %), sourdine
    Volume(i32),
    Mute,
}

impl ModKey {
    fn held(self, mods: &ModifiersState) -> bool {
        match self {
            ModKey::Alt => mods.alt,
            ModKey::Super => mods.logo,
        }
    }
}

impl Odysseus {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        let was_off = self.screen_off;
        if matches!(
            event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
        ) {
            self.note_activity();
        }
        // La touche qui rallume l'écran ne fait rien d'autre
        if was_off
            && matches!(
                event,
                InputEvent::Keyboard { .. } | InputEvent::PointerButton { .. }
            )
        {
            return;
        }
        let locked = self.is_locked();
        let selecting = self.selection.is_some();
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);

                let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
                let mod_key = self.mod_key;
                let action = self.seat.get_keyboard().unwrap().input::<Action, _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |_, mods, handle| {
                        // XF86Switch_VT_1 … XF86Switch_VT_12, produits par Ctrl+Alt+Fn
                        let raw = handle.modified_sym().raw();
                        if pressed && (0x1008_FE01..=0x1008_FE0C).contains(&raw) {
                            return FilterResult::Intercept(Action::SwitchVt(
                                (raw - 0x1008_FE00) as i32,
                            ));
                        }
                        // Sélection de capture en cours : elle reçoit tout le clavier
                        if selecting {
                            return FilterResult::Intercept(Action::SelectionKey(if pressed {
                                handle.modified_sym()
                            } else {
                                Keysym::NoSymbol
                            }));
                        }
                        // Verrouillé : seuls le changement de tty et Super+L (relancer le verrou)
                        if locked {
                            if pressed
                                && mod_key.held(mods)
                                && handle.raw_latin_sym_or_raw_current_sym() == Some(Keysym::l)
                            {
                                return FilterResult::Intercept(Action::Lock);
                            }
                            return FilterResult::Forward;
                        }
                        if pressed {
                            let media = match handle.modified_sym() {
                                Keysym::Print if mods.shift => Some(Action::CaptureFull),
                                Keysym::Print => Some(Action::Capture),
                                Keysym::XF86_AudioRaiseVolume => Some(Action::Volume(5)),
                                Keysym::XF86_AudioLowerVolume => Some(Action::Volume(-5)),
                                Keysym::XF86_AudioMute => Some(Action::Mute),
                                _ => None,
                            };
                            if let Some(action) = media {
                                return FilterResult::Intercept(action);
                            }
                        }
                        if pressed && mod_key.held(mods) {
                            // Rangée des chiffres par code physique (evdev KEY_1…KEY_9 + 8) :
                            // indépendant de la disposition (AZERTY : & é " ' ( - è _ ç)
                            let code = handle.raw_code().raw();
                            if (10..=18).contains(&code) {
                                let n = (code - 10) as usize;
                                return FilterResult::Intercept(if mods.shift {
                                    Action::MoveToWorkspace(n)
                                } else {
                                    Action::Workspace(n)
                                });
                            }
                            let action = match handle.modified_sym() {
                                Keysym::Return => Some(Action::Terminal),
                                Keysym::space => Some(Action::Launcher),
                                Keysym::f => Some(Action::ToggleMaximize),
                                Keysym::Tab => Some(Action::CycleWindows),
                                Keysym::m | Keysym::M => Some(Action::Minimize),
                                Keysym::l | Keysym::L => Some(Action::Lock),
                                Keysym::k | Keysym::K => Some(Action::Shortcuts),
                                Keysym::a | Keysym::A => Some(Action::Assistant),
                                Keysym::Left => Some(Action::Snap(crate::snap::Zone::Left)),
                                Keysym::Right => Some(Action::Snap(crate::snap::Zone::Right)),
                                Keysym::Up => Some(Action::Snap(crate::snap::Zone::Top)),
                                Keysym::Down => Some(Action::Unsnap),
                                Keysym::q | Keysym::Q => Some(Action::CloseWindow),
                                Keysym::E if mods.shift => Some(Action::Quit),
                                Keysym::S if mods.shift => Some(Action::Capture),
                                _ => None,
                            };
                            if let Some(action) = action {
                                return FilterResult::Intercept(action);
                            }
                        }
                        FilterResult::Forward
                    },
                );
                if let Some(action) = action {
                    self.run_action(action);
                }
            }
            // Souris en mode tty (libinput) : déplacement relatif, borné à l'écran
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let mut pos = pointer.current_location() + event.delta();
                if let Some(geo) = self
                    .space
                    .outputs()
                    .next()
                    .and_then(|o| self.space.output_geometry(o))
                {
                    let geo = geo.to_f64();
                    pos.x = pos.x.clamp(geo.loc.x, geo.loc.x + geo.size.w - 1.0);
                    pos.y = pos.y.clamp(geo.loc.y, geo.loc.y + geo.size.h - 1.0);
                }
                self.pointer_moved(pos, event.time_msec());
            }
            // Fenêtre winit : position absolue
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.space.outputs().next().unwrap();
                let output_geo = self.space.output_geometry(output).unwrap();
                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
                self.pointer_moved(pos, event.time_msec());
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();

                let button = event.button_code();

                let button_state = event.state();

                if selecting {
                    self.selection_button(button_state == ButtonState::Pressed);
                    return;
                }
                if locked {
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button,
                            state: button_state,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                    return;
                }

                // Clic sur une couche (barre, lanceur) : pas de changement de fenêtre active
                let on_layer = self.focusable_layer_at(pointer.current_location());
                if let Some(Some(layer_surface)) = on_layer.clone()
                    && ButtonState::Pressed == button_state
                {
                    keyboard.set_focus(self, Some(layer_surface), serial);
                }
                if ButtonState::Pressed == button_state
                    && !pointer.is_grabbed()
                    && on_layer.is_none()
                {
                    let title = (button == BTN_LEFT)
                        .then(|| self.titlebar_under(pointer.current_location()))
                        .flatten();
                    if let Some((window, part)) = title {
                        // Barre de titre dessinée par le compositeur : boutons, déplacer
                        let alt = keyboard.modifier_state().alt;
                        self.titlebar_click(window, part, serial, alt);
                    } else if let Some((window, _loc)) = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
                    {
                        self.space.raise_element(&window, true);
                        keyboard.set_focus(
                            self,
                            Some(window.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                        self.space.elements().for_each(|window| {
                            window.toplevel().unwrap().send_pending_configure();
                        });

                        // Mod + clic gauche maintenu : déplacer la fenêtre
                        if button == BTN_LEFT && self.mod_key.held(&keyboard.modifier_state()) {
                            let start_data = PointerGrabStartData {
                                focus: None,
                                button,
                                location: pointer.current_location(),
                            };
                            let initial_window_location =
                                self.space.element_location(&window).unwrap();
                            let grab = MoveSurfaceGrab {
                                start_data,
                                window,
                                initial_window_location,
                            };
                            pointer.set_grab(self, grab, serial, Focus::Clear);
                        } else if button == BTN_RIGHT
                            && self.mod_key.held(&keyboard.modifier_state())
                        {
                            // Mod + clic droit maintenu : redimensionner
                            self.start_mod_resize(window, button, serial);
                        }
                    } else if button == BTN_LEFT
                        && let Some((window, edges)) = self.edge_under(pointer.current_location())
                    {
                        // Bord d'une fenêtre : la redimensionner
                        self.space.raise_element(&window, true);
                        keyboard.set_focus(
                            self,
                            Some(window.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                        self.space.elements().for_each(|window| {
                            window.toplevel().unwrap().send_pending_configure();
                        });
                        self.start_resize(window, edges, button, serial);
                    } else {
                        self.space.elements().for_each(|window| {
                            window.set_activated(false);
                            window.toplevel().unwrap().send_pending_configure();
                        });
                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                    }
                };

                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { .. } if selecting => {}
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();

                let horizontal_amount = event.amount(Axis::Horizontal).unwrap_or_else(|| {
                    event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.
                });
                let vertical_amount = event.amount(Axis::Vertical).unwrap_or_else(|| {
                    event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.
                });
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}

impl Odysseus {
    fn run_action(&mut self, action: Action) {
        match action {
            Action::Terminal => {
                let terminal = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
                self.spawn(&terminal);
            }
            Action::CloseWindow => {
                let focused = self.seat.get_keyboard().unwrap().current_focus();
                if let Some(surface) = focused
                    && let Some(window) = self
                        .space
                        .elements()
                        .find(|w| w.toplevel().unwrap().wl_surface() == &surface)
                {
                    window.toplevel().unwrap().send_close();
                }
            }
            Action::Quit => self.loop_signal.stop(),
            Action::Capture => self.start_selection(),
            Action::CaptureFull => self.capture_full(),
            Action::SelectionKey(sym) => {
                if sym != Keysym::NoSymbol {
                    self.selection_key(sym);
                }
            }
            Action::Launcher => {
                // Le lanceur est installé à côté du compositeur
                let launcher = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("launcher")))
                    .filter(|p| p.exists());
                match launcher {
                    Some(path) => self.spawn(&path.to_string_lossy()),
                    None => tracing::warn!("lanceur introuvable à côté du compositeur"),
                }
            }
            Action::ToggleMaximize => {
                if let Some(window) = self.focused_window() {
                    self.toggle_maximize(&window);
                }
            }
            Action::CycleWindows => self.cycle_windows(),
            Action::Minimize => self.minimize_focused(),
            Action::Lock => self.lock_session(),
            Action::Shortcuts => self.spawn_sibling("odysseus-raccourcis", &[]),
            Action::Assistant => self.spawn_sibling("odysseus-assistant", &[]),
            Action::Snap(zone) => {
                if let Some(window) = self.focused_window() {
                    self.snap_window(&window, zone);
                }
            }
            Action::Unsnap => {
                if let Some(window) = self.focused_window() {
                    self.restore_window(&window);
                }
            }
            Action::Volume(step) => {
                let change = if step >= 0 {
                    format!("{step}%+")
                } else {
                    format!("{}%-", -step)
                };
                self.spawn(&format!(
                    "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0; wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ {change}; {}",
                    self.sibling_command("odysseus-notify --volume")
                ));
            }
            Action::Mute => self.spawn(&format!(
                "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle; {}",
                self.sibling_command("odysseus-notify --volume")
            )),
            Action::Workspace(n) => self.switch_workspace(n),
            Action::MoveToWorkspace(n) => self.move_focused_to_workspace(n),
            Action::SwitchVt(vt) => {
                if let Some(session) = self.session.as_mut() {
                    use smithay::backend::session::Session;
                    if let Err(e) = session.change_vt(vt) {
                        tracing::warn!("changement de tty {vt} : {e}");
                    }
                }
            }
        }
    }

    fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: u32) {
        self.hot_corner(pos);
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        // Pendant une sélection de capture, aucun client ne reçoit le pointeur
        let under = if self.selection.is_some() {
            self.selection_motion(pos);
            None
        } else {
            self.surface_under(pos)
        };
        self.update_edge_cursor(pos, under.is_none());
        self.update_title_hover(pos, under.is_none());
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Au bord d'une fenêtre, le curseur montre dans quel sens on peut la
    /// redimensionner ; il reste tel quel pendant le redimensionnement.
    fn update_edge_cursor(&mut self, pos: Point<f64, Logical>, nothing_under: bool) {
        let pointer = self.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            return;
        }
        let edge = (nothing_under && self.selection.is_none() && !self.is_locked())
            .then(|| self.edge_under(pos))
            .flatten()
            .map(|(_, e)| e);
        match edge {
            Some(edges) => {
                let icon = if edges == ResizeEdge::TOP_LEFT || edges == ResizeEdge::BOTTOM_RIGHT {
                    CursorIcon::NwseResize
                } else if edges == ResizeEdge::TOP_RIGHT || edges == ResizeEdge::BOTTOM_LEFT {
                    CursorIcon::NeswResize
                } else if edges.intersects(ResizeEdge::LEFT | ResizeEdge::RIGHT) {
                    CursorIcon::EwResize
                } else {
                    CursorIcon::NsResize
                };
                self.cursor_status = CursorImageStatus::Named(icon);
                self.edge_cursor = true;
            }
            None if self.edge_cursor => {
                self.edge_cursor = false;
                self.cursor_status = CursorImageStatus::default_named();
            }
            None => {}
        }
    }

    /// Coin actif en haut à droite, comme sur macOS : le centre de
    /// notifications s'ouvre. Il se réarme quand le pointeur s'éloigne ; rien
    /// pendant un glisser (aimantation d'une fenêtre dans le coin), une
    /// capture ou le verrouillage.
    fn hot_corner(&mut self, pos: Point<f64, Logical>) {
        let corner = self.space.outputs().find_map(|o| {
            let g = self.space.output_geometry(o)?;
            g.contains(pos.to_i32_round())
                .then(|| ((g.loc.x + g.size.w) as f64 - pos.x, pos.y - g.loc.y as f64))
        });
        let Some((dx, dy)) = corner else {
            return;
        };
        if dx <= HOT_CORNER && dy <= HOT_CORNER {
            let busy = self.seat.get_pointer().unwrap().is_grabbed()
                || self.selection.is_some()
                || self.is_locked();
            if !self.hot_corner_used && !busy {
                self.hot_corner_used = true;
                tracing::info!("coin actif : centre de notifications");
                self.spawn_sibling("odysseus-notify", &["--center", "show"]);
            }
        } else if dx > HOT_CORNER_REARM || dy > HOT_CORNER_REARM {
            self.hot_corner_used = false;
        }
    }
}

/// Coin actif : taille du coin, et distance pour le réarmer (pixels logiques)
const HOT_CORNER: f64 = 2.0;
const HOT_CORNER_REARM: f64 = 60.0;
