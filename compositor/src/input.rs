use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, Focus, GrabStartData as PointerGrabStartData, MotionEvent,
        },
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    grabs::MoveSurfaceGrab,
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
    Capture,
    Workspace(usize),
    MoveToWorkspace(usize),
    Launcher,
    ToggleMaximize,
    CycleWindows,
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
                        if pressed && handle.modified_sym() == Keysym::Print {
                            return FilterResult::Intercept(Action::Capture);
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
                    if let Some((window, _loc)) = self
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
            Action::Capture => self.capture_pending = true,
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
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.surface_under(pos);
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
}
